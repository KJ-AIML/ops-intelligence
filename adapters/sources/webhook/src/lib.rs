//! Generic Webhook source adapter — the first live-style ingestion interface.
//!
//! Accepts the canonical payload from tech sheet 14. Like every source adapter it
//! extracts facts and hints only: no severity mapping, no state mapping, no
//! `event_family` classification. Those belong to the core normalization layer
//! (decision 0002), which is why a webhook alert and a CSV row end up
//! indistinguishable once past this boundary.

use chrono::{DateTime, Utc};
use ops_core::normalization::{normalize, FamilyHints, SourceFacts, SourceNormalizationConfig};
use ops_core::ports::SignalNormalizer;
use ops_core::{DomainError, Event, RawSignal, SourceType};
use serde_json::Value;
use std::collections::BTreeMap;

pub const CONTENT_TYPE: &str = "application/json";

/// Origin recorded when the caller does not name one. Keeps the severity/state
/// dialect lookup working for anonymous posters.
pub const DEFAULT_ORIGIN: &str = "generic_webhook";

pub struct WebhookNormalizer;

impl SignalNormalizer for WebhookNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        if source_type != SourceType::GenericWebhook || raw.content_type != CONTENT_TYPE {
            return Err(DomainError::Source(
                "unsupported normalization source/content type".into(),
            ));
        }
        let facts = extract_facts(&raw.payload)?;
        if facts.external_id != raw.external_id {
            return Err(DomainError::Validation(
                "payload external_id differs from RawSignal".into(),
            ));
        }
        let config = SourceNormalizationConfig::for_origin(&facts.origin);
        normalize(
            raw.organization_id,
            raw.source_id,
            raw.id,
            &facts,
            &config,
            created_at,
        )
    }
}

/// Pull the source-side `external_id` out of a webhook body without normalizing
/// it. Ingestion needs this before the payload is ever parsed for meaning,
/// because it is the idempotency key (tech sheet 9).
pub fn external_id_of(payload: &Value) -> Option<String> {
    payload
        .get("external_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Webhook mechanics only. Required minimum is `timestamp` + `title`
/// (tech sheet 14); everything else is optional.
pub fn extract_facts(payload: &Value) -> Result<SourceFacts, DomainError> {
    let object = payload
        .as_object()
        .ok_or_else(|| DomainError::Validation("webhook payload must be a JSON object".into()))?;

    let optional = |key: &str| -> Result<Option<String>, DomainError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok((!s.trim().is_empty()).then(|| s.trim().to_owned())),
            // Numbers are common for severity in real webhooks; accept them
            // verbatim rather than rejecting an otherwise usable alert.
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(Value::Bool(b)) => Ok(Some(b.to_string())),
            Some(_) => Err(DomainError::Validation(format!(
                "webhook field {key} must be a scalar"
            ))),
        }
    };
    let required = |key: &str| {
        optional(key)?
            .ok_or_else(|| DomainError::Validation(format!("missing webhook field {key}")))
    };

    let timestamp = required("timestamp")?;
    let occurred_at = DateTime::parse_from_rfc3339(&timestamp)
        .map_err(|_| {
            DomainError::Validation(
                "invalid timestamp: expected RFC3339 with timezone, e.g. 2026-09-03T09:42:10+07:00"
                    .into(),
            )
        })?
        .with_timezone(&Utc);

    let title = required("title")?;
    let message = optional("message")?;

    let mut labels: BTreeMap<String, String> = match object.get("labels") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                let value = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), value)
            })
            .collect(),
        Some(_) => {
            return Err(DomainError::Validation(
                "labels must be a JSON object".into(),
            ))
        }
    };
    // An explicit event_family in the body is an operator instruction and wins
    // over the keyword classifier (decision 0002, layer 1).
    if let Some(family) = optional("event_family")? {
        labels.insert("event_family".into(), family);
    }

    let hints = FamilyHints {
        title: title.clone(),
        message: message.clone(),
        labels: labels.clone(),
        metric_name: optional("metric_name")?,
        monitor_type: optional("monitor_type")?,
        vendor_category: optional("vendor_category")?,
    };

    Ok(SourceFacts {
        origin: optional("source")?.unwrap_or_else(|| DEFAULT_ORIGIN.to_string()),
        occurred_at,
        title,
        message,
        environment: optional("environment")?,
        service: optional("service")?,
        resource: optional("resource")?,
        severity_raw: optional("severity")?,
        state_raw: optional("state")?,
        external_id: optional("external_id")?,
        labels,
        hints,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical() -> Value {
        // The example payload from tech sheet 14, verbatim.
        json!({
            "timestamp": "2026-09-03T09:42:10+07:00",
            "title": "API latency high",
            "severity": "warning",
            "service": "payment-api",
            "resource": "api-prod-01",
            "environment": "production",
            "event_family": "latency",
            "state": "firing",
            "external_id": "grafana-123",
            "labels": { "team": "payments" },
            "message": "p95 latency above threshold"
        })
    }

    #[test]
    fn accepts_the_canonical_payload_from_the_tech_sheet() {
        let facts = extract_facts(&canonical()).unwrap();
        assert_eq!(facts.title, "API latency high");
        assert_eq!(facts.service.as_deref(), Some("payment-api"));
        assert_eq!(facts.severity_raw.as_deref(), Some("warning"));
        assert_eq!(facts.state_raw.as_deref(), Some("firing"));
        assert_eq!(facts.external_id.as_deref(), Some("grafana-123"));
        assert_eq!(
            facts.labels.get("team").map(String::as_str),
            Some("payments")
        );
        assert_eq!(
            facts.occurred_at.to_rfc3339(),
            "2026-09-03T02:42:10+00:00",
            "timestamp must convert to UTC, not be reinterpreted"
        );
    }

    #[test]
    fn required_minimum_is_timestamp_and_title() {
        let minimal = json!({ "timestamp": "2026-09-03T09:42:10+07:00", "title": "something" });
        let facts = extract_facts(&minimal).unwrap();
        assert_eq!(facts.origin, DEFAULT_ORIGIN);
        assert!(facts.severity_raw.is_none());
        assert!(facts.state_raw.is_none());

        assert!(extract_facts(&json!({ "title": "no timestamp" })).is_err());
        assert!(extract_facts(&json!({ "timestamp": "2026-09-03T09:42:10+07:00" })).is_err());
    }

    #[test]
    fn a_naive_timestamp_is_rejected_rather_than_assumed_utc() {
        let payload = json!({ "timestamp": "2026-09-03 09:42:10", "title": "x" });
        let err = extract_facts(&payload).unwrap_err().to_string();
        assert!(err.contains("RFC3339"), "got: {err}");
    }

    #[test]
    fn explicit_event_family_reaches_the_classifier_as_a_label() {
        let facts = extract_facts(&canonical()).unwrap();
        assert_eq!(
            facts.hints.labels.get("event_family").map(String::as_str),
            Some("latency")
        );
    }

    #[test]
    fn numeric_severity_is_kept_verbatim_for_the_normalizer_to_map() {
        let payload = json!({
            "timestamp": "2026-09-03T09:42:10+07:00", "title": "x", "severity": 2
        });
        assert_eq!(
            extract_facts(&payload).unwrap().severity_raw.as_deref(),
            Some("2")
        );
    }

    #[test]
    fn external_id_is_readable_before_the_payload_is_understood() {
        assert_eq!(external_id_of(&canonical()).as_deref(), Some("grafana-123"));
        assert_eq!(external_id_of(&json!({ "external_id": "  " })), None);
        assert_eq!(external_id_of(&json!({})), None);
    }

    #[test]
    fn non_object_payloads_are_rejected() {
        assert!(extract_facts(&json!([1, 2, 3])).is_err());
        assert!(extract_facts(&json!("a string")).is_err());
    }
}
