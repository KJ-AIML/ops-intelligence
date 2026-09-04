use crate::error::DomainError;
use crate::ids::{OrganizationId, RawSignalId, SourceId};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// Every signal must end in a known state — "no silent loss" (architecture 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStatus {
    Received,
    Queued,
    Processing,
    Processed,
    Failed,
    Ignored,
}

impl ProcessingStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Processed => "processed",
            Self::Failed => "failed",
            Self::Ignored => "ignored",
        }
    }

    /// A signal in a terminal status needs no further work.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Processed | Self::Failed | Self::Ignored)
    }
}

impl fmt::Display for ProcessingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProcessingStatus {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "received" => Ok(Self::Received),
            "queued" => Ok(Self::Queued),
            "processing" => Ok(Self::Processing),
            "processed" => Ok(Self::Processed),
            "failed" => Ok(Self::Failed),
            "ignored" => Ok(Self::Ignored),
            other => Err(DomainError::Validation(format!(
                "unknown processing status: {other}"
            ))),
        }
    }
}

/// The original source payload, preserved verbatim (architecture 2, "preserve
/// evidence"). Nothing downstream may rewrite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSignal {
    pub id: RawSignalId,
    pub organization_id: OrganizationId,
    pub source_id: SourceId,
    pub received_at: DateTime<Utc>,
    pub external_id: Option<String>,
    pub content_type: String,
    pub payload: serde_json::Value,
    pub payload_hash: String,
    pub processing_status: ProcessingStatus,
    pub processing_error: Option<String>,
}

impl RawSignal {
    pub fn received(
        organization_id: OrganizationId,
        source_id: SourceId,
        external_id: Option<String>,
        content_type: impl Into<String>,
        payload: serde_json::Value,
        received_at: DateTime<Utc>,
    ) -> Self {
        let payload_hash = hash_payload(&payload);
        Self {
            id: RawSignalId::new(),
            organization_id,
            source_id,
            received_at,
            external_id,
            content_type: content_type.into(),
            payload,
            payload_hash,
            processing_status: ProcessingStatus::Received,
            processing_error: None,
        }
    }

    /// Which key the store should deduplicate on, in the priority order given by
    /// tech sheet 9: a stable source-side id when one exists, otherwise content.
    pub fn idempotency_key(&self) -> IdempotencyKey<'_> {
        match self.external_id.as_deref() {
            Some(id) => IdempotencyKey::ExternalId(id),
            None => IdempotencyKey::PayloadHash(&self.payload_hash),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyKey<'a> {
    ExternalId(&'a str),
    PayloadHash(&'a str),
}

/// Deterministic content hash.
///
/// `serde_json::Value` stores objects in a `BTreeMap` unless the `preserve_order`
/// feature is enabled, so serialisation is key-sorted and stable across runs.
/// Do not enable `preserve_order` in this workspace without revisiting this.
pub fn hash_payload(payload: &serde_json::Value) -> String {
    let canonical = serde_json::to_vec(payload).expect("serde_json::Value always serialises");
    let digest = Sha256::digest(&canonical);
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hash_is_stable_and_key_order_independent() {
        let a = json!({ "title": "CPU high", "severity": "Sev1", "resource": "api-prod-01" });
        let b = json!({ "resource": "api-prod-01", "severity": "Sev1", "title": "CPU high" });
        assert_eq!(
            hash_payload(&a),
            hash_payload(&b),
            "key order must not change the hash"
        );
        assert_eq!(hash_payload(&a), hash_payload(&a.clone()));
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let a = json!({ "title": "CPU high" });
        let b = json!({ "title": "CPU normal" });
        assert_ne!(hash_payload(&a), hash_payload(&b));
    }

    #[test]
    fn idempotency_prefers_external_id_then_falls_back_to_hash() {
        let org = OrganizationId::new();
        let src = SourceId::new();
        let now = Utc::now();
        let payload = json!({ "title": "x" });

        let with_id = RawSignal::received(
            org,
            src,
            Some("EM-88213".into()),
            "text/csv",
            payload.clone(),
            now,
        );
        assert_eq!(
            with_id.idempotency_key(),
            IdempotencyKey::ExternalId("EM-88213")
        );

        let without = RawSignal::received(org, src, None, "text/csv", payload, now);
        assert_eq!(
            without.idempotency_key(),
            IdempotencyKey::PayloadHash(&without.payload_hash)
        );
    }

    #[test]
    fn new_signals_start_non_terminal() {
        let s = RawSignal::received(
            OrganizationId::new(),
            SourceId::new(),
            None,
            "text/csv",
            json!({}),
            Utc::now(),
        );
        assert_eq!(s.processing_status, ProcessingStatus::Received);
        assert!(!s.processing_status.is_terminal());
    }
}
