use chrono::{DateTime, Utc};
use ops_core::normalization::{normalize, FamilyHints, SourceFacts, SourceNormalizationConfig};
use ops_core::ports::SignalNormalizer;
use ops_core::{DomainError, Event, RawSignal, SourceType};
use std::collections::BTreeMap;

pub struct CsvNormalizer;

impl SignalNormalizer for CsvNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        if source_type != SourceType::CsvImport || raw.content_type != super::CONTENT_TYPE {
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

/// CSV mechanics only: retain raw dialects and extract neutral evidence hints.
pub fn extract_facts(payload: &serde_json::Value) -> Result<SourceFacts, DomainError> {
    let object = payload
        .as_object()
        .ok_or_else(|| DomainError::Validation("CSV payload must be an object".into()))?;
    let optional = |key: &str| -> Result<Option<String>, DomainError> {
        match object.get(key) {
            None => Ok(None),
            Some(serde_json::Value::String(s)) => {
                Ok((!s.trim().is_empty()).then(|| s.trim().to_owned()))
            }
            Some(_) => Err(DomainError::Validation(format!(
                "CSV field {key} must be a string"
            ))),
        }
    };
    let required = |key: &str| {
        optional(key)?.ok_or_else(|| DomainError::Validation(format!("missing CSV field {key}")))
    };
    let timestamp = required("timestamp")?;
    let occurred_at = DateTime::parse_from_rfc3339(&timestamp)
        .map_err(|_| {
            DomainError::Validation(
                "invalid source timestamp: expected RFC3339 with timezone".into(),
            )
        })?
        .with_timezone(&Utc);
    let title = required("title")?;
    let message = optional("message")?;
    // Optional export columns carry JSON label maps; never derive hints from row IDs.
    let mut labels: BTreeMap<String, String> = optional("labels")?
        .map(|s| {
            serde_json::from_str(&s).map_err(|_| {
                DomainError::Validation("invalid labels: expected JSON string map".into())
            })
        })
        .transpose()?
        .unwrap_or_default();
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
        origin: required("source")?,
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
