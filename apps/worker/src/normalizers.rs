//! Source-type dispatch.
//!
//! The one place that knows which adapter handles which source. Adding Azure
//! Monitor later means one new arm here and one new adapter crate — nothing
//! downstream of `RawSignal` changes, which is the whole point of the
//! ports-and-adapters boundary (architecture 7).

use chrono::{DateTime, Utc};
use ops_core::ports::SignalNormalizer;
use ops_core::{DomainError, Event, RawSignal, SourceType};

pub struct DispatchingNormalizer;

impl SignalNormalizer for DispatchingNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        match source_type {
            SourceType::CsvImport => {
                ops_source_csv::CsvNormalizer.normalize(raw, source_type, created_at)
            }
            SourceType::GenericWebhook => {
                ops_source_webhook::WebhookNormalizer.normalize(raw, source_type, created_at)
            }
            SourceType::Grafana => {
                ops_source_grafana::GrafanaNormalizer.normalize(raw, source_type, created_at)
            }
            // Not a panic and not a silent skip: the signal is recorded as
            // failed with this reason, so it stays visible and replayable once
            // the adapter exists (decision 0001 keeps vendor adapters out until
            // the source inventory picks them).
            other => Err(DomainError::Source(format!(
                "no normalizer registered for source type {other}"
            ))),
        }
    }
}
