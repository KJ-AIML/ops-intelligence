//! ExplainIncident — the first and only reasoner in v0.1.
//!
//! Owns three things the provider must not touch: the prompt, the output schema,
//! and the validation that decides whether a response is allowed to be
//! persisted. Everything here is pure; the only I/O is the provider call.

use super::{ReasoningProvider, ReasoningRequest, ReasoningResponse};
use crate::domains::events::{EventFamily, Severity};
use crate::domains::incidents::IncidentStatus;
use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Bump when the prompt or schema changes in a way that makes older stored
/// insights non-comparable. Recorded on every insight.
pub const PROMPT_VERSION: &str = "explain-incident/v1";
pub const SCHEMA_VERSION: i32 = 1;

/// Bounded retries for a response that fails validation. Beyond this the
/// attempt is recorded as failed rather than persisted as a success
/// (tech sheet 11).
const MAX_ATTEMPTS: u32 = 2;

const MAX_FIELD_CHARS: usize = 600;

/// What the model is asked to decide. Deliberately coarse: a three-way triage
/// hint, not a severity — severity is deterministic and already known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actionability {
    ActNow,
    Review,
    Informational,
}

impl Actionability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActNow => "act_now",
            Self::Review => "review",
            Self::Informational => "informational",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentExplanation {
    pub actionability: Actionability,
    pub explanation: String,
    pub likely_impact: String,
    pub suggested_check: String,
}

/// Exactly what the model is allowed to see.
///
/// Derived facts only — no raw payloads, no log lines, no free-text messages
/// from the source. That is what makes the data boundary in source inventory 21
/// reviewable: this struct IS the boundary, and it is small enough to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentContext {
    pub incident_id: String,
    pub title: String,
    pub status: IncidentStatus,
    pub severity: Severity,
    pub event_family: EventFamily,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub duration_seconds: i64,
    pub event_count: i64,
    pub source_count: i64,
    pub recent_recurrences: i64,
    pub reopened_count: i64,
    /// Distinct event titles, deduplicated. Titles only — never messages.
    pub evidence_titles: Vec<String>,
    pub source_names: Vec<String>,
}

impl IncidentContext {
    /// The anchored facts, rendered for the prompt. Every claim the model is
    /// permitted to make has to be traceable to a line in here.
    fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |k: &str, v: String| {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(&v);
            out.push('\n');
        };
        line("incident_id", self.incident_id.clone());
        line("title", self.title.clone());
        line("status", self.status.to_string());
        line("severity", self.severity.to_string());
        line("event_family", self.event_family.to_string());
        line(
            "environment",
            self.environment.clone().unwrap_or_else(|| "unknown".into()),
        );
        line(
            "service",
            self.service.clone().unwrap_or_else(|| "unknown".into()),
        );
        line(
            "resource",
            self.resource.clone().unwrap_or_else(|| "unknown".into()),
        );
        line("duration_seconds", self.duration_seconds.to_string());
        line("event_count", self.event_count.to_string());
        line("reporting_sources", self.source_names.join(", "));
        line("distinct_source_count", self.source_count.to_string());
        line("recent_recurrences", self.recent_recurrences.to_string());
        line("reopened_count", self.reopened_count.to_string());
        out.push_str("evidence_titles:\n");
        for title in &self.evidence_titles {
            out.push_str("  - ");
            out.push_str(title);
            out.push('\n');
        }
        out
    }

    /// Everything the model is allowed to name, lowercased, for the
    /// anti-fabrication check.
    fn vocabulary(&self) -> String {
        let mut v = self.render().to_lowercase();
        v.push(' ');
        v.push_str(&self.title.to_lowercase());
        v
    }
}

pub struct IncidentReasoner;

impl IncidentReasoner {
    pub fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "actionability": {
                    "type": "string",
                    "enum": ["act_now", "review", "informational"],
                    "description": "act_now if a human should look now; review if it should be checked later; informational if no action is implied."
                },
                "explanation": {
                    "type": "string",
                    "description": "What happened, in one or two plain sentences, using only the supplied facts."
                },
                "likely_impact": {
                    "type": "string",
                    "description": "What a user or dependent system may have experienced. Say it is unknown if the facts do not support a claim."
                },
                "suggested_check": {
                    "type": "string",
                    "description": "The single most useful thing to inspect first."
                }
            },
            "required": ["actionability", "explanation", "likely_impact", "suggested_check"],
            "additionalProperties": false
        })
    }

    fn system_prompt() -> String {
        // The constraints are stated as hard rules because the validator below
        // enforces them: anything the prompt asks for that cannot be checked is
        // a wish, not a contract.
        [
            "You explain infrastructure incidents to an on-call engineer.",
            "",
            "You are given a fixed set of facts about one incident. Rules:",
            "- Use only those facts. Do not invent hostnames, services, metrics, numbers or causes.",
            "- If the facts do not support a claim, say the facts do not show it.",
            "- Do not restate the severity or status; the engineer can already see them.",
            "- Offer interpretation, clearly hedged, not certainty. 'may have', not 'did'.",
            "- Be terse. Two sentences per field at most. No preamble, no headings.",
            "- Suggest one concrete check, not a checklist.",
        ]
        .join("\n")
    }

    fn build_request(context: &IncidentContext) -> ReasoningRequest {
        ReasoningRequest {
            system: Self::system_prompt(),
            user: format!("Facts about this incident:\n\n{}", context.render()),
            schema: Self::schema(),
            max_tokens: 1024,
        }
    }

    /// Validate a raw provider response. Public so the rules are testable
    /// directly, without a provider.
    pub fn validate(
        raw: &str,
        context: &IncidentContext,
    ) -> Result<IncidentExplanation, DomainError> {
        let parsed: IncidentExplanation = serde_json::from_str(raw.trim()).map_err(|e| {
            DomainError::Reasoning(format!("response is not valid for the schema: {e}"))
        })?;

        for (field, value) in [
            ("explanation", &parsed.explanation),
            ("likely_impact", &parsed.likely_impact),
            ("suggested_check", &parsed.suggested_check),
        ] {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(DomainError::Reasoning(format!("{field} is empty")));
            }
            if trimmed.chars().count() > MAX_FIELD_CHARS {
                return Err(DomainError::Reasoning(format!(
                    "{field} is longer than {MAX_FIELD_CHARS} characters"
                )));
            }
        }

        // Source anchoring (tech sheet 30, "no invented source facts"). A model
        // naming a host that was never in the context is fabricating, and that
        // is exactly the failure that would destroy trust in the product.
        let vocabulary = context.vocabulary();
        for field in [
            &parsed.explanation,
            &parsed.likely_impact,
            &parsed.suggested_check,
        ] {
            if let Some(token) = first_unknown_identifier(field, &vocabulary) {
                return Err(DomainError::Reasoning(format!(
                    "response names {token:?}, which is not in the supplied facts"
                )));
            }
        }

        Ok(parsed)
    }

    /// Run the reasoner, retrying a schema-invalid response within a bounded
    /// policy. Returns the validated explanation and the raw response that
    /// produced it, so the caller can record audit metadata.
    pub async fn explain(
        provider: &dyn ReasoningProvider,
        context: &IncidentContext,
    ) -> Result<(IncidentExplanation, ReasoningResponse, u32), DomainError> {
        if !provider.is_enabled() {
            return Err(DomainError::Reasoning(
                "AI is disabled (AI_ENABLED=false)".into(),
            ));
        }
        let request = Self::build_request(context);
        let mut last: Option<DomainError> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            match provider.complete_json(&request).await {
                // A transport or provider failure is not retried here: retrying
                // a refusal or an auth error just spends money twice.
                Err(e) => return Err(e),
                Ok(response) => match Self::validate(&response.json_text, context) {
                    Ok(explanation) => return Ok((explanation, response, attempt)),
                    Err(e) => last = Some(e),
                },
            }
        }

        Err(DomainError::Reasoning(format!(
            "no schema-valid response after {MAX_ATTEMPTS} attempts: {}",
            last.map_or_else(|| "unknown".into(), |e| e.to_string())
        )))
    }
}

/// Find the first token that looks like an infrastructure identifier but does
/// not appear in the supplied facts.
///
/// ponytail: a deterministic heuristic, not a semantic check — it catches
/// invented hosts and services (`api-prod-07`, `web-stg-02`), which is the
/// failure that actually matters, and says nothing about invented prose. Written
/// without a regex dependency. Tighten it if real payloads show a gap.
fn first_unknown_identifier(text: &str, vocabulary: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        // Trim trailing sentence punctuation before BOTH tests. Comparing the
        // untrimmed token against the vocabulary made "api-prod-01." look
        // invented purely because it ended a sentence.
        .map(normalize_token)
        .filter(|token| looks_like_identifier(token))
        .find(|token| !vocabulary.contains(&token.to_lowercase()))
        .map(str::to_string)
}

fn normalize_token(token: &str) -> &str {
    token.trim_matches(|c| c == '.' || c == ',' || c == ';' || c == ':')
}

fn looks_like_identifier(token: &str) -> bool {
    if token.len() < 5 || !token.contains('-') {
        return false;
    }
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let env_ish = ["prod", "stg", "staging", "dev", "qa", "uat"]
        .iter()
        .any(|marker| token.to_lowercase().contains(marker));
    has_digit || env_ish
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::{DisabledProvider, ProviderDescriptor};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn context() -> IncidentContext {
        IncidentContext {
            incident_id: "11111111-1111-1111-1111-111111111111".into(),
            title: "[FIRING:1] PaymentAPIHealthCheck (payment-api api-prod-01)".into(),
            status: IncidentStatus::Recovered,
            severity: Severity::Critical,
            event_family: EventFamily::Availability,
            environment: Some("production".into()),
            service: Some("payment-api".into()),
            resource: Some("api-prod-01".into()),
            duration_seconds: 358,
            event_count: 5,
            source_count: 2,
            recent_recurrences: 1,
            reopened_count: 0,
            evidence_titles: vec!["[payment-api] [Down] Connection refused".into()],
            source_names: vec!["csv:grafana".into(), "csv:uptime_kuma".into()],
        }
    }

    fn valid_json() -> String {
        serde_json::to_string(&serde_json::json!({
            "actionability": "review",
            "explanation": "The payment API health check failed for about six minutes before recovering.",
            "likely_impact": "Requests to payment-api may have failed during that window.",
            "suggested_check": "Inspect api-prod-01 around the reported window."
        }))
        .unwrap()
    }

    /// Records what it was asked and replays canned responses in order.
    struct StubProvider {
        responses: Mutex<Vec<Result<String, DomainError>>>,
        calls: Mutex<u32>,
    }

    impl StubProvider {
        fn new(responses: Vec<Result<String, DomainError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(0),
            }
        }
        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl ReasoningProvider for StubProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                provider: "stub".into(),
                model: "stub-1".into(),
            }
        }
        async fn complete_json(
            &self,
            _request: &ReasoningRequest,
        ) -> Result<ReasoningResponse, DomainError> {
            *self.calls.lock().unwrap() += 1;
            let mut queue = self.responses.lock().unwrap();
            match queue.remove(0) {
                Err(e) => Err(e),
                Ok(json_text) => Ok(ReasoningResponse {
                    json_text,
                    model: "stub-1".into(),
                    request_id: Some("req_1".into()),
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    stop_reason: Some("end_turn".into()),
                }),
            }
        }
    }

    #[test]
    fn accepts_a_well_formed_grounded_response() {
        let parsed = IncidentReasoner::validate(&valid_json(), &context()).unwrap();
        assert_eq!(parsed.actionability, Actionability::Review);
    }

    #[test]
    fn rejects_a_response_naming_a_host_that_was_never_supplied() {
        let bad = serde_json::json!({
            "actionability": "review",
            "explanation": "The outage also affected api-prod-07 and its replica.",
            "likely_impact": "Unknown.",
            "suggested_check": "Check the pool."
        })
        .to_string();
        let err = IncidentReasoner::validate(&bad, &context()).unwrap_err();
        assert!(err.to_string().contains("api-prod-07"), "got: {err}");
    }

    #[test]
    fn accepts_identifiers_that_were_supplied() {
        // api-prod-01 IS in the facts, so naming it is grounded, not invented.
        let ok = serde_json::json!({
            "actionability": "act_now",
            "explanation": "api-prod-01 stopped answering health checks.",
            "likely_impact": "payment-api callers may have seen errors.",
            "suggested_check": "Check api-prod-01."
        })
        .to_string();
        // Note the trailing period: sentence punctuation must not make a
        // grounded identifier look invented.
        assert!(IncidentReasoner::validate(&ok, &context()).is_ok());
    }

    #[test]
    fn rejects_unknown_actionability_values() {
        let bad = serde_json::json!({
            "actionability": "panic",
            "explanation": "x", "likely_impact": "y", "suggested_check": "z"
        })
        .to_string();
        assert!(IncidentReasoner::validate(&bad, &context()).is_err());
    }

    #[test]
    fn rejects_missing_fields_empty_fields_and_non_json() {
        let missing = serde_json::json!({ "actionability": "review" }).to_string();
        assert!(IncidentReasoner::validate(&missing, &context()).is_err());

        let empty = serde_json::json!({
            "actionability": "review",
            "explanation": "   ", "likely_impact": "y", "suggested_check": "z"
        })
        .to_string();
        assert!(IncidentReasoner::validate(&empty, &context()).is_err());

        assert!(IncidentReasoner::validate("I think the API broke!", &context()).is_err());
    }

    #[test]
    fn rejects_an_overlong_field() {
        let long = serde_json::json!({
            "actionability": "review",
            "explanation": "a".repeat(MAX_FIELD_CHARS + 1),
            "likely_impact": "y", "suggested_check": "z"
        })
        .to_string();
        assert!(IncidentReasoner::validate(&long, &context()).is_err());
    }

    #[tokio::test]
    async fn retries_once_then_succeeds() {
        let provider = StubProvider::new(vec![Ok("not json".into()), Ok(valid_json())]);
        let (_, _, attempts) = IncidentReasoner::explain(&provider, &context())
            .await
            .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(provider.calls(), 2);
    }

    #[tokio::test]
    async fn gives_up_after_the_bounded_retries_rather_than_persisting_junk() {
        let provider = StubProvider::new(vec![Ok("nope".into()), Ok("still nope".into())]);
        let err = IncidentReasoner::explain(&provider, &context())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no schema-valid response"),
            "got: {err}"
        );
        assert_eq!(provider.calls(), MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn a_provider_error_is_not_retried() {
        // Retrying a refusal or an auth failure just spends money twice.
        let provider = StubProvider::new(vec![Err(DomainError::Reasoning("refused".into()))]);
        assert!(IncidentReasoner::explain(&provider, &context())
            .await
            .is_err());
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn the_disabled_provider_never_calls_out_and_says_why() {
        let err = IncidentReasoner::explain(&DisabledProvider, &context())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("AI_ENABLED=false"), "got: {err}");
    }

    #[test]
    fn the_prompt_carries_only_derived_facts() {
        // The data boundary is reviewable precisely because this is checkable:
        // nothing from a raw payload reaches the model.
        let rendered = context().render();
        assert!(rendered.contains("payment-api"));
        assert!(rendered.contains("duration_seconds: 358"));
        assert!(
            !rendered.contains("row_id"),
            "raw payload fields must never reach the prompt"
        );
    }

    #[test]
    fn identifier_heuristic_ignores_ordinary_hyphenated_words() {
        assert!(!looks_like_identifier("read-only"));
        assert!(!looks_like_identifier("well-known"));
        assert!(looks_like_identifier("api-prod-01"));
        assert!(looks_like_identifier("web-stg-02"));
    }
}
