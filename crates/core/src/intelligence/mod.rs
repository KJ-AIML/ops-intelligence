//! Bounded AI reasoning.
//!
//! ```text
//! GenerateInsightUseCase
//!         |
//! IncidentReasoner      owns the prompt, the output schema and the validation
//!         |
//! ReasoningProvider     owns credentials, transport and vendor mechanics
//!         |
//! Vendor adapter
//! ```
//!
//! The split matters (architecture 7). The reasoner is vendor-agnostic and
//! testable with a stub provider, so every rule about what the model is allowed
//! to say is verifiable without a network call or an API key.
//!
//! What AI may do here: explain, interpret impact, suggest an investigation.
//! What it may never do: assign a fingerprint, a severity, a status, or any
//! other value correlation depends on. Those are deterministic (architecture 2).

pub mod reasoner;

use crate::error::DomainError;
use async_trait::async_trait;

pub use reasoner::{Actionability, IncidentContext, IncidentExplanation, IncidentReasoner};

/// One structured-output request. The reasoner builds it; the provider ships it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningRequest {
    pub system: String,
    pub user: String,
    /// JSON Schema the response must satisfy. Enforced by the provider where the
    /// vendor supports it, and re-validated locally regardless.
    pub schema: serde_json::Value,
    pub max_tokens: u32,
}

/// What came back, before any interpretation. `json_text` is unvalidated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningResponse {
    pub json_text: String,
    pub model: String,
    pub request_id: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub provider: String,
    pub model: String,
}

#[async_trait]
pub trait ReasoningProvider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;

    /// True when the provider is configured and permitted to run. The whole
    /// pipeline must remain useful when this is false (tech sheet 33).
    fn is_enabled(&self) -> bool {
        true
    }

    async fn complete_json(
        &self,
        request: &ReasoningRequest,
    ) -> Result<ReasoningResponse, DomainError>;
}

/// The provider used when AI is switched off — the default until the pilot's
/// data boundary is agreed (source inventory 21).
///
/// It is not a stub for tests: it is the shipping default, and it is why the
/// product works with no API key and no data leaving the deployment.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledProvider;

#[async_trait]
impl ReasoningProvider for DisabledProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            provider: "disabled".into(),
            model: "none".into(),
        }
    }

    fn is_enabled(&self) -> bool {
        false
    }

    async fn complete_json(
        &self,
        _request: &ReasoningRequest,
    ) -> Result<ReasoningResponse, DomainError> {
        Err(DomainError::Reasoning(
            "AI is disabled (AI_ENABLED=false)".into(),
        ))
    }
}
