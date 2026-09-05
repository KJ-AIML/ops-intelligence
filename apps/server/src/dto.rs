//! Wire types.
//!
//! Deliberately separate from the domain: an HTTP response shape is a contract
//! with the UI, and renaming a domain field should not silently break it.

use chrono::{DateTime, Utc};
use ops_core::domains::insights::Insight;
use ops_core::insights::{IncidentSummary, OperationsSummary, RecurringPattern, SourceNoise};
use ops_core::ports::{EvidenceEntry, IncidentWithEvidence};
use ops_core::{Event, Source};
use serde::Serialize;

#[derive(Serialize)]
pub struct SummaryResponse {
    pub window: WindowDto,
    pub raw_signals: i64,
    pub events: i64,
    pub events_without_incident: i64,
    pub incidents: i64,
    pub open: i64,
    pub acknowledged: i64,
    pub recovered: i64,
    pub resolved: i64,
    pub critical: i64,
    pub needs_attention: i64,
    pub noisiest_sources: Vec<SourceNoiseDto>,
    pub recurring: Vec<RecurringDto>,
}

#[derive(Serialize)]
pub struct WindowDto {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct SourceNoiseDto {
    pub source_id: String,
    pub name: String,
    pub source_type: String,
    pub signals: i64,
    pub events: i64,
    pub incidents: i64,
    pub share_percent: f64,
}

#[derive(Serialize)]
pub struct RecurringDto {
    pub fingerprint: String,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: String,
    pub occurrences: i64,
    pub last_seen_at: DateTime<Utc>,
    pub currently_active: bool,
}

#[derive(Serialize)]
pub struct IncidentDto {
    pub id: String,
    pub title: String,
    pub status: String,
    pub severity: String,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: String,
    pub event_count: i64,
    pub source_count: i64,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub recovered_at: Option<DateTime<Utc>>,
    pub reopened_count: i64,
    pub occurrences: i64,
    /// Deterministic triage rank. Exposed so the UI sorts identically to the API
    /// and the ordering is explainable rather than mysterious.
    pub attention_score: i64,
}

#[derive(Serialize)]
pub struct IncidentDetailDto {
    #[serde(flatten)]
    pub incident: IncidentDto,
    pub timeline: Vec<EvidenceDto>,
}

#[derive(Serialize)]
pub struct EvidenceDto {
    pub event_id: String,
    pub relation: String,
    pub occurred_at: DateTime<Utc>,
    pub severity: String,
    pub state: String,
    pub title: String,
    pub message: Option<String>,
    pub source_name: String,
    pub external_id: Option<String>,
    /// The original payload, unmodified. This is what makes the chain auditable.
    pub raw_signal_id: String,
    pub raw_payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct EventDto {
    pub id: String,
    pub occurred_at: DateTime<Utc>,
    pub severity: String,
    pub state: String,
    pub event_family: String,
    pub title: String,
    pub message: Option<String>,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub external_id: Option<String>,
    pub source_id: String,
    pub raw_signal_id: String,
}

#[derive(Serialize)]
pub struct SourceDto {
    pub id: String,
    pub name: String,
    pub source_type: String,
    pub enabled: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Whether a webhook token exists — never the token itself.
    pub has_ingest_token: bool,
}

/// Returned only by source creation, the one moment the token is readable.
#[derive(Serialize)]
pub struct CreatedSourceDto {
    #[serde(flatten)]
    pub source: SourceDto,
    pub ingest_token: String,
    pub ingest_url: String,
}

impl From<&IncidentSummary> for IncidentDto {
    fn from(i: &IncidentSummary) -> Self {
        Self {
            id: i.id.to_string(),
            title: i.title.clone(),
            status: i.status.to_string(),
            severity: i.severity.to_string(),
            environment: i.environment.clone(),
            service: i.service.clone(),
            resource: i.resource.clone(),
            event_family: i.event_family.to_string(),
            event_count: i.event_count,
            source_count: i.source_count,
            started_at: i.started_at,
            last_event_at: i.last_event_at,
            recovered_at: i.recovered_at,
            reopened_count: i.reopened_count,
            occurrences: i.occurrences,
            attention_score: ops_core::insights::attention_score(i),
        }
    }
}

impl From<&EvidenceEntry> for EvidenceDto {
    fn from(e: &EvidenceEntry) -> Self {
        Self {
            event_id: e.event.id.to_string(),
            relation: e.relation.to_string(),
            occurred_at: e.event.occurred_at,
            severity: e.event.severity.to_string(),
            state: e.event.state.to_string(),
            title: e.event.title.clone(),
            message: e.event.message.clone(),
            source_name: e.source_name.clone(),
            external_id: e.event.external_id.clone(),
            raw_signal_id: e.raw_signal_id.to_string(),
            raw_payload: e.raw_payload.clone(),
        }
    }
}

impl From<IncidentWithEvidence> for IncidentDetailDto {
    fn from(d: IncidentWithEvidence) -> Self {
        Self {
            incident: IncidentDto::from(&d.incident),
            timeline: d.timeline.iter().map(EvidenceDto::from).collect(),
        }
    }
}

impl From<&Event> for EventDto {
    fn from(e: &Event) -> Self {
        Self {
            id: e.id.to_string(),
            occurred_at: e.occurred_at,
            severity: e.severity.to_string(),
            state: e.state.to_string(),
            event_family: e.event_family.to_string(),
            title: e.title.clone(),
            message: e.message.clone(),
            environment: e.environment.clone(),
            service: e.service.clone(),
            resource: e.resource.clone(),
            external_id: e.external_id.clone(),
            source_id: e.source_id.to_string(),
            raw_signal_id: e.raw_signal_id.to_string(),
        }
    }
}

impl From<&Source> for SourceDto {
    fn from(s: &Source) -> Self {
        Self {
            id: s.id.to_string(),
            name: s.name.clone(),
            source_type: s.source_type.to_string(),
            enabled: s.enabled,
            last_seen_at: s.last_seen_at,
            has_ingest_token: s.ingest_token.is_some(),
        }
    }
}

impl From<&SourceNoise> for SourceNoiseDto {
    fn from(s: &SourceNoise) -> Self {
        Self {
            source_id: s.source_id.to_string(),
            name: s.name.clone(),
            source_type: s.source_type.clone(),
            signals: s.signals,
            events: s.events,
            incidents: s.incidents,
            share_percent: s.share_percent,
        }
    }
}

impl From<&RecurringPattern> for RecurringDto {
    fn from(r: &RecurringPattern) -> Self {
        Self {
            fingerprint: r.fingerprint.clone(),
            environment: r.environment.clone(),
            service: r.service.clone(),
            resource: r.resource.clone(),
            event_family: r.event_family.to_string(),
            occurrences: r.occurrences,
            last_seen_at: r.last_seen_at,
            currently_active: r.currently_active,
        }
    }
}

impl From<OperationsSummary> for SummaryResponse {
    fn from(s: OperationsSummary) -> Self {
        Self {
            window: WindowDto {
                from: s.from,
                to: s.to,
            },
            raw_signals: s.raw_signals,
            events: s.events,
            events_without_incident: s.events_without_incident,
            incidents: s.incidents,
            open: s.open,
            acknowledged: s.acknowledged,
            recovered: s.recovered,
            resolved: s.resolved,
            critical: s.critical,
            needs_attention: s.needs_attention,
            noisiest_sources: s
                .noisiest_sources
                .iter()
                .map(SourceNoiseDto::from)
                .collect(),
            recurring: s.recurring.iter().map(RecurringDto::from).collect(),
        }
    }
}

/// An insight is always labelled with who produced it and, for AI, exactly
/// which model — a computed count and a model's interpretation must never look
/// like the same class of claim (architecture 16).
#[derive(Serialize)]
pub struct InsightDto {
    pub id: String,
    pub incident_id: Option<String>,
    pub insight_type: String,
    pub source: String,
    pub status: String,
    pub title: String,
    pub summary: Option<String>,
    pub structured_payload: Option<serde_json::Value>,
    pub schema_version: i32,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub prompt_version: Option<String>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&Insight> for InsightDto {
    fn from(i: &Insight) -> Self {
        let ai = i.source == ops_core::InsightSource::Ai;
        Self {
            id: i.id.to_string(),
            incident_id: i.incident_id.map(|x| x.to_string()),
            insight_type: i.insight_type.to_string(),
            source: i.source.to_string(),
            status: i.status.to_string(),
            title: i.title.clone(),
            summary: i.summary.clone(),
            structured_payload: i.structured_payload.clone(),
            schema_version: i.schema_version,
            model: ai.then(|| i.model_metadata.model.clone()),
            provider: ai.then(|| i.model_metadata.provider.clone()),
            prompt_version: ai.then(|| i.model_metadata.prompt_version.clone()),
            latency_ms: ai.then_some(i.model_metadata.latency_ms),
            error: i.error.clone(),
            created_at: i.created_at,
        }
    }
}
