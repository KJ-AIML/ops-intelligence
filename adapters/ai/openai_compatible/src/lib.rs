//! OpenAI-compatible provider — for a model running on this machine.
//!
//! Works with LM Studio, Ollama, vLLM, llama.cpp's server, LocalAI: anything
//! exposing `POST /v1/chat/completions`.
//!
//! This exists for one reason. The pilot cannot send real incident context to a
//! cloud provider until the data boundary in source inventory 21 is agreed. A
//! model on localhost has no such boundary to cross, so reasoning can be
//! evaluated against **real captured signals** immediately, and the cloud
//! decision can be made later on evidence instead of guesswork.
//!
//! Same port as every other provider. The reasoner cannot tell the difference,
//! which is the point of having the port at all.

use async_trait::async_trait;
use ops_core::error::DomainError;
use ops_core::intelligence::{
    ProviderDescriptor, ReasoningProvider, ReasoningRequest, ReasoningResponse,
};
use serde_json::{json, Value};
use std::time::Duration;

/// LM Studio's default. Ollama is 11434, vLLM is 8000 — all overridable.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:1234/v1";

#[derive(Debug, Clone)]
pub struct LocalConfig {
    pub base_url: String,
    pub model: String,
    /// Most local servers ignore this; some require a placeholder.
    pub api_key: Option<String>,
    /// Local models are slower than a hosted API and the box may be busy.
    pub timeout: Duration,
}

impl LocalConfig {
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("AI_ENABLED")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        // A local model has no sensible default name — every install differs —
        // so refuse rather than guess and produce a confusing 404.
        let model = std::env::var("AI_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())?;
        Some(Self {
            base_url: std::env::var("AI_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            model,
            api_key: std::env::var("AI_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty()),
            timeout: Duration::from_secs(
                std::env::var("AI_TIMEOUT_SECONDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(180),
            ),
        })
    }
}

pub struct LocalProvider {
    config: LocalConfig,
    client: reqwest::Client,
}

impl LocalProvider {
    pub fn new(config: LocalConfig) -> Result<Self, DomainError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| DomainError::Reasoning(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { config, client })
    }

    pub fn body(&self, request: &ReasoningRequest) -> Value {
        json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens,
            // Sampling off: two runs over one dataset must be comparable, and a
            // creative incident explanation is not a feature.
            "temperature": 0,
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "incident_explanation",
                    "strict": true,
                    "schema": request.schema
                }
            }
        })
    }
}

#[async_trait]
impl ReasoningProvider for LocalProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            provider: "local".into(),
            model: self.config.model.clone(),
        }
    }

    async fn complete_json(
        &self,
        request: &ReasoningRequest,
    ) -> Result<ReasoningResponse, DomainError> {
        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("content-type", "application/json");
        if let Some(key) = &self.config.api_key {
            builder = builder.bearer_auth(key);
        }

        let response = builder
            .json(&self.body(request))
            .send()
            .await
            .map_err(|e| {
                // The overwhelmingly likely cause is that nothing is listening.
                // Saying so saves an hour of debugging the wrong thing.
                DomainError::Reasoning(format!(
                    "local model at {} is unreachable ({e}). Is LM Studio / Ollama running?",
                    self.config.base_url
                ))
            })?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| DomainError::Reasoning(format!("cannot read response body: {e}")))?;

        if !status.is_success() {
            return Err(DomainError::Reasoning(format!(
                "local model returned {status}: {}",
                summarize_error(&text)
            )));
        }
        parse_success(&text, &self.config.model)
    }
}

pub fn parse_success(body: &str, requested_model: &str) -> Result<ReasoningResponse, DomainError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| DomainError::Reasoning(format!("response is not JSON: {e}")))?;

    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| DomainError::Reasoning("response contained no choices".into()))?;

    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    // A truncated response often still parses as JSON-ish and would otherwise
    // be handed to the validator as if it were complete.
    if finish_reason.as_deref() == Some("length") {
        return Err(DomainError::Reasoning(
            "local model hit its token limit before finishing the response".into(),
        ));
    }

    let json_text = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| DomainError::Reasoning("response contained no message content".into()))?;

    let usage = value.get("usage");
    Ok(ReasoningResponse {
        json_text,
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(requested_model)
            .to_string(),
        request_id: value.get("id").and_then(Value::as_str).map(str::to_string),
        input_tokens: usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        output_tokens: usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        stop_reason: finish_reason,
    })
}

fn summarize_error(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message").or(Some(e)))
                .map(|m| {
                    m.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| m.to_string())
                })
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> LocalProvider {
        LocalProvider::new(LocalConfig {
            base_url: DEFAULT_BASE_URL.into(),
            model: "qwen2.5-14b-instruct".into(),
            api_key: None,
            timeout: Duration::from_secs(30),
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
    fn request_body_is_openai_chat_completions_shaped() {
        let body = provider().body(&request());
        assert_eq!(body["model"], "qwen2.5-14b-instruct");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn sampling_is_off_so_two_runs_over_one_dataset_are_comparable() {
        assert_eq!(provider().body(&request())["temperature"], 0);
    }

    #[test]
    fn parses_a_chat_completion() {
        let body = json!({
            "id": "chatcmpl-1",
            "model": "qwen2.5-14b-instruct",
            "choices": [{
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "{\"actionability\":\"review\"}" }
            }],
            "usage": { "prompt_tokens": 300, "completion_tokens": 60 }
        })
        .to_string();

        let parsed = parse_success(&body, "requested").unwrap();
        assert_eq!(parsed.json_text, "{\"actionability\":\"review\"}");
        assert_eq!(parsed.model, "qwen2.5-14b-instruct");
        assert_eq!(parsed.input_tokens, Some(300));
        assert_eq!(parsed.output_tokens, Some(60));
    }

    #[test]
    fn a_truncated_response_is_an_error_not_a_half_insight() {
        let body = json!({
            "id": "c", "model": "m",
            "choices": [{ "finish_reason": "length",
                          "message": { "content": "{\"actionability\":\"rev" } }]
        })
        .to_string();
        let err = parse_success(&body, "m").unwrap_err().to_string();
        assert!(err.contains("token limit"), "got: {err}");
    }

    #[test]
    fn falls_back_to_the_requested_model_name_when_the_server_omits_it() {
        // Some local servers do not echo the model back.
        let body = json!({
            "choices": [{ "finish_reason": "stop", "message": { "content": "{}" } }]
        })
        .to_string();
        assert_eq!(
            parse_success(&body, "local-model").unwrap().model,
            "local-model"
        );
    }

    #[test]
    fn empty_and_malformed_responses_are_errors() {
        assert!(parse_success(&json!({ "choices": [] }).to_string(), "m").is_err());
        assert!(parse_success("<html>404</html>", "m").is_err());
        let no_content = json!({ "choices": [{ "message": { "content": "  " } }] }).to_string();
        assert!(parse_success(&no_content, "m").is_err());
    }

    #[test]
    fn config_requires_an_explicit_model_because_every_install_differs() {
        let saved = (
            std::env::var("AI_ENABLED").ok(),
            std::env::var("AI_MODEL").ok(),
        );
        std::env::set_var("AI_ENABLED", "true");
        std::env::remove_var("AI_MODEL");
        assert!(
            LocalConfig::from_env().is_none(),
            "guessing a local model name produces a confusing 404"
        );

        std::env::set_var("AI_MODEL", "some-local-model");
        let cfg = LocalConfig::from_env().expect("configured");
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);

        match saved.0 {
            Some(v) => std::env::set_var("AI_ENABLED", v),
            None => std::env::remove_var("AI_ENABLED"),
        }
        match saved.1 {
            Some(v) => std::env::set_var("AI_MODEL", v),
            None => std::env::remove_var("AI_MODEL"),
        }
    }
}
