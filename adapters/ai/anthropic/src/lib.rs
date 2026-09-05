//! Anthropic Messages API provider.
//!
//! Vendor mechanics only: credentials, transport, request shape, error mapping.
//! It has no idea what an incident is — the prompt, the schema and the rules
//! about what the model may say all live in the reasoner (architecture 7).
//!
//! Rust has no official Anthropic SDK, so this is the documented raw HTTP
//! surface. Swapping in OpenAI or Azure OpenAI later means another file
//! implementing `ReasoningProvider`; nothing else in the system changes.

use async_trait::async_trait;
use ops_core::error::DomainError;
use ops_core::intelligence::{
    ProviderDescriptor, ReasoningProvider, ReasoningRequest, ReasoningResponse,
};
use serde_json::{json, Value};
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Server-side refusal fallbacks: on a policy decline the API re-runs the same
/// request on a fallback model inside the same call, so a refusal does not
/// simply lose the insight.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub const DEFAULT_MODEL: &str = "claude-opus-5";

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub model: String,
    /// Thinking depth and token spend. `low` suits a short, well-specified
    /// structured explanation; raise it only if quality measurably needs it.
    pub effort: String,
    pub timeout: Duration,
}

impl AnthropicConfig {
    /// Reads configuration from the environment. Returns `None` when AI is
    /// switched off or unconfigured — the caller then uses `DisabledProvider`,
    /// which is the shipping default until the data boundary is agreed.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("AI_ENABLED")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let api_key = std::env::var("AI_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self {
            api_key,
            model: std::env::var("AI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            effort: std::env::var("AI_EFFORT").unwrap_or_else(|_| "low".to_string()),
            timeout: Duration::from_secs(
                std::env::var("AI_TIMEOUT_SECONDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60),
            ),
        })
    }
}

pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> Result<Self, DomainError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| DomainError::Reasoning(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { config, client })
    }

    /// The request body, separated out so its shape is testable without a
    /// network call or an API key.
    pub fn body(&self, request: &ReasoningRequest) -> Value {
        json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": [{ "role": "user", "content": request.user }],
            // Constrains the response to the reasoner's schema. The reasoner
            // re-validates anyway: a schema the vendor enforces is a
            // convenience, not a guarantee we are willing to depend on.
            "output_config": {
                "effort": self.config.effort,
                "format": { "type": "json_schema", "schema": request.schema }
            },
            "fallbacks": "default"
        })
    }
}

#[async_trait]
impl ReasoningProvider for AnthropicProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            provider: "anthropic".into(),
            model: self.config.model.clone(),
        }
    }

    async fn complete_json(
        &self,
        request: &ReasoningRequest,
    ) -> Result<ReasoningResponse, DomainError> {
        let response = self
            .client
            .post(API_URL)
            // The key goes in a header and is never logged or stored.
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", FALLBACK_BETA)
            .header("content-type", "application/json")
            .json(&self.body(request))
            .send()
            .await
            .map_err(|e| DomainError::Reasoning(format!("request failed: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| DomainError::Reasoning(format!("cannot read response body: {e}")))?;

        if !status.is_success() {
            // Surface the provider's own message: an auth or quota failure that
            // reads as a generic error costs an hour of debugging.
            return Err(DomainError::Reasoning(format!(
                "provider returned {status}: {}",
                summarize_error(&text)
            )));
        }

        parse_success(&text)
    }
}

/// Pull the structured text out of a successful Messages response.
///
/// Public for testing: every branch here is a real response shape, and none of
/// them should need a live API call to verify.
pub fn parse_success(body: &str) -> Result<ReasoningResponse, DomainError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| DomainError::Reasoning(format!("response is not JSON: {e}")))?;

    let stop_reason = value
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    // A refusal is HTTP 200. Checking stop_reason before reading content is the
    // difference between a recorded failure and a confusing empty insight.
    if stop_reason.as_deref() == Some("refusal") {
        let category = value
            .get("stop_details")
            .and_then(|d| d.get("category"))
            .and_then(Value::as_str)
            .unwrap_or("unspecified");
        return Err(DomainError::Reasoning(format!(
            "provider declined the request (category: {category})"
        )));
    }

    let json_text = value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| DomainError::Reasoning("response contained no text block".into()))?;

    let usage = value.get("usage");
    Ok(ReasoningResponse {
        json_text,
        // The model that actually answered, which may be a fallback rather than
        // the one requested.
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        request_id: value.get("id").and_then(Value::as_str).map(str::to_string),
        input_tokens: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        output_tokens: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        stop_reason,
    })
}

fn summarize_error(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(AnthropicConfig {
            api_key: "sk-ant-test".into(),
            model: DEFAULT_MODEL.into(),
            effort: "low".into(),
            timeout: Duration::from_secs(5),
        })
        .unwrap()
    }

    fn request() -> ReasoningRequest {
        ReasoningRequest {
            system: "sys".into(),
            user: "facts".into(),
            schema: json!({ "type": "object" }),
            max_tokens: 1024,
        }
    }

    #[test]
    fn request_body_matches_the_documented_shape() {
        let body = provider().body(&request());
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        // budget_tokens is rejected on this model family; thinking is on by
        // default, so the parameter is deliberately absent.
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn the_api_key_never_appears_in_the_request_body() {
        let body = provider().body(&request()).to_string();
        assert!(!body.contains("sk-ant-test"));
    }

    #[test]
    fn parses_a_successful_structured_response() {
        let body = json!({
            "id": "msg_01ABC",
            "model": "claude-opus-5",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{\"actionability\":\"review\"}" }],
            "usage": { "input_tokens": 420, "output_tokens": 88 }
        })
        .to_string();

        let parsed = parse_success(&body).unwrap();
        assert_eq!(parsed.json_text, "{\"actionability\":\"review\"}");
        assert_eq!(parsed.request_id.as_deref(), Some("msg_01ABC"));
        assert_eq!(parsed.input_tokens, Some(420));
        assert_eq!(parsed.output_tokens, Some(88));
    }

    #[test]
    fn a_refusal_is_an_error_not_an_empty_insight() {
        // Refusals arrive as HTTP 200, so this branch is easy to miss.
        let body = json!({
            "id": "msg_02", "model": "claude-opus-5", "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "cyber" },
            "content": []
        })
        .to_string();
        let err = parse_success(&body).unwrap_err().to_string();
        assert!(err.contains("declined"), "got: {err}");
        assert!(err.contains("cyber"), "got: {err}");
    }

    #[test]
    fn records_the_model_that_actually_answered_after_a_fallback() {
        let body = json!({
            "id": "msg_03", "model": "claude-opus-4-8", "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "{}" }]
        })
        .to_string();
        assert_eq!(parse_success(&body).unwrap().model, "claude-opus-4-8");
    }

    #[test]
    fn ignores_non_text_blocks_such_as_thinking() {
        let body = json!({
            "id": "msg_04", "model": "claude-opus-5", "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "{\"ok\":true}" }
            ]
        })
        .to_string();
        assert_eq!(parse_success(&body).unwrap().json_text, "{\"ok\":true}");
    }

    #[test]
    fn an_empty_or_malformed_response_is_an_error() {
        let empty = json!({ "id": "m", "model": "x", "content": [] }).to_string();
        assert!(parse_success(&empty).is_err());
        assert!(parse_success("not json at all").is_err());
    }

    #[test]
    fn provider_error_messages_are_surfaced_not_swallowed() {
        let body = json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid x-api-key" }
        })
        .to_string();
        assert_eq!(summarize_error(&body), "invalid x-api-key");
    }

    #[test]
    fn config_is_off_unless_explicitly_enabled_with_a_key() {
        // Guards the default: no AI_ENABLED, no calls, no data leaving.
        temp_env(&[("AI_ENABLED", None), ("AI_API_KEY", None)], || {
            assert!(AnthropicConfig::from_env().is_none());
        });
        temp_env(
            &[("AI_ENABLED", Some("true")), ("AI_API_KEY", None)],
            || {
                assert!(
                    AnthropicConfig::from_env().is_none(),
                    "enabled without a key must not produce a config"
                );
            },
        );
        temp_env(
            &[("AI_ENABLED", Some("true")), ("AI_API_KEY", Some("k"))],
            || {
                let cfg = AnthropicConfig::from_env().expect("configured");
                assert_eq!(cfg.model, DEFAULT_MODEL);
            },
        );
    }

    /// Minimal scoped env helper; the process-global env makes these tests
    /// order-dependent otherwise.
    fn temp_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
