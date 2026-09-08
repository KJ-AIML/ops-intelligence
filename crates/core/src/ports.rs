use crate::domains::events::Event;
use crate::domains::events::Severity;
use crate::domains::incidents::IncidentStatus;
use crate::domains::incidents::{Incident, IncidentEvent};
use crate::domains::insights::Insight;
use crate::domains::organizations::Organization;
use crate::domains::pilot::{PilotDataset, PilotRun, RunStats};
use crate::domains::raw_signals::ProcessingStatus;
use crate::domains::raw_signals::RawSignal;
use crate::domains::sources::{Source, SourceType};
use crate::error::DomainError;
use crate::ids::{DatasetId, RunId};
use crate::ids::{IncidentId, OrganizationId, RawSignalId};

use crate::insights::{IncidentSummary, OperationsSummary};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Injected time (tech sheet 12).
///
/// This matters more than it looks. Correlation windows are computed from event
/// `occurred_at`, and importing a historical CSV in 200ms would collapse an
/// entire day into one window if any code reached for the wall clock instead.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Result of an idempotent insert. `Duplicate` is a normal outcome, not an error:
/// a redelivered alert is expected traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted(RawSignalId),
    Duplicate,
}

#[async_trait]
pub trait OrganizationRepository: Send + Sync {
    async fn ensure_by_slug(&self, slug: &str, name: &str) -> Result<Organization, DomainError>;
}

#[async_trait]
pub trait SourceRepository: Send + Sync {
    async fn ensure(
        &self,
        organization_id: OrganizationId,
        source_type: SourceType,
        name: &str,
    ) -> Result<Source, DomainError>;

    async fn touch_last_seen(
        &self,
        organization_id: OrganizationId,
        source_id: crate::ids::SourceId,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError>;

    async fn list_sources(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<Source>, DomainError>;

    /// Create a webhook-fed source (`GenericWebhook` or `Grafana`) and return
    /// it together with its freshly minted token. The token is returned
    /// exactly once, here; it is never readable again from a list or detail
    /// endpoint.
    async fn create_webhook(
        &self,
        organization_id: OrganizationId,
        source_type: SourceType,
        name: &str,
        token: &str,
    ) -> Result<Source, DomainError>;

    /// Resolve an ingestion token to its source. The tenant comes from the
    /// token, never from the request body (tech sheet 20).
    async fn find_by_ingest_token(&self, token: &str) -> Result<Option<Source>, DomainError>;

    async fn set_enabled(
        &self,
        organization_id: OrganizationId,
        source_id: crate::ids::SourceId,
        enabled: bool,
    ) -> Result<Source, DomainError>;
}

#[async_trait]
pub trait RawSignalRepository: Send + Sync {
    /// Insert, or report the signal as already known. Idempotency is resolved by
    /// `RawSignal::idempotency_key`.
    async fn insert_if_new(&self, signal: &RawSignal) -> Result<InsertOutcome, DomainError>;

    /// No-silent-loss accounting: counts per processing status for one tenant.
    async fn count_by_status(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<(String, i64)>, DomainError>;
}

/// Synchronous, CPU-only normalization. Implementations must not perform I/O.
pub trait SignalNormalizer: Send + Sync {
    fn normalize(
        &self,
        signal: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError>;
}

#[async_trait]
pub trait EventRepository: Send + Sync {
    /// Atomically claim one received signal and persist its Event or failure.
    /// A persistence error rolls back the claim; a normalization error commits failed.
    async fn process_next(
        &self,
        organization_id: OrganizationId,
        normalizer: &dyn SignalNormalizer,
        clock: &dyn Clock,
    ) -> Result<Option<ProcessingStatus>, DomainError>;

    async fn list(&self, organization_id: OrganizationId) -> Result<Vec<Event>, DomainError>;
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub source_id: Option<crate::ids::SourceId>,
    pub severity: Option<Severity>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone, Default)]
pub struct IncidentFilter {
    pub status: Option<IncidentStatus>,
    pub severity: Option<Severity>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: i64,
}

/// An incident with the evidence chain that justifies it: every member Event in
/// source-time order, each carrying the RawSignal it came from.
#[derive(Debug, Clone)]
pub struct IncidentWithEvidence {
    pub incident: IncidentSummary,
    pub timeline: Vec<EvidenceEntry>,
}

#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    pub event: Event,
    pub relation: crate::domains::incidents::IncidentEventRelation,
    pub raw_signal_id: RawSignalId,
    pub source_name: String,
    pub raw_payload: serde_json::Value,
}

#[async_trait]
pub trait IncidentRepository: Send + Sync {
    /// Use Event.occurred_at only; processing-time is deliberately absent here.
    async fn correlate_next(&self, organization_id: OrganizationId) -> Result<bool, DomainError>;
    async fn list_incidents(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<Incident>, DomainError>;
    async fn list_incident_events(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<IncidentEvent>, DomainError>;
    async fn count_correlation_status(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<(String, i64)>, DomainError>;
}

/// Read side of the product API, plus the two manual transitions.
///
/// Separate from the write-path repositories on purpose: the server only ever
/// needs this trait, so an HTTP handler cannot reach the correlation or
/// ingestion machinery by accident.
#[async_trait]
pub trait ProductQueries: Send + Sync {
    async fn query_incidents(
        &self,
        organization_id: OrganizationId,
        filter: IncidentFilter,
    ) -> Result<Vec<IncidentSummary>, DomainError>;

    async fn incident_with_evidence(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
    ) -> Result<Option<IncidentWithEvidence>, DomainError>;

    /// `at` is injected rather than read from the wall clock inside the query,
    /// so the transition is testable and the audit trail records a real time.
    async fn acknowledge_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
        at: DateTime<Utc>,
    ) -> Result<IncidentSummary, DomainError>;

    async fn resolve_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
        at: DateTime<Utc>,
    ) -> Result<IncidentSummary, DomainError>;

    /// Deterministic Operations View payload, aggregated in SQL.
    async fn operations_summary(
        &self,
        organization_id: OrganizationId,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<OperationsSummary, DomainError>;

    /// Event explorer. Bounded by `limit` so one request cannot pull the whole
    /// table into memory.
    async fn query_events(
        &self,
        organization_id: OrganizationId,
        filter: EventFilter,
    ) -> Result<Vec<Event>, DomainError>;
}

#[async_trait]
pub trait InsightRepository: Send + Sync {
    /// Record a reasoning outcome, success or failure. Failures are stored too:
    /// no silent loss applies to intelligence as much as to ingestion.
    async fn insert(&self, insight: &Insight) -> Result<(), DomainError>;

    async fn list_for_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
    ) -> Result<Vec<Insight>, DomainError>;

    async fn list_recent(
        &self,
        organization_id: OrganizationId,
        limit: i64,
    ) -> Result<Vec<Insight>, DomainError>;

    /// Incidents that have no successful insight of this type yet, ranked so a
    /// bounded run explains the ones that matter most first.
    async fn incidents_needing_insight(
        &self,
        organization_id: OrganizationId,
        insight_type: crate::domains::insights::InsightType,
        limit: i64,
    ) -> Result<Vec<IncidentSummary>, DomainError>;
}

/// Pilot Lab: capture real signals into frozen datasets, then replay them.
#[async_trait]
pub trait PilotRepository: Send + Sync {
    /// Freeze the captured signals matching the filter into a named dataset.
    /// Membership is by reference — the dataset points at the original
    /// evidence rather than copying it, so it can never drift from what was
    /// actually received.
    async fn create_dataset(
        &self,
        organization_id: OrganizationId,
        name: &str,
        description: Option<&str>,
        source_name: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<PilotDataset, DomainError>;

    async fn list_datasets(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<PilotDataset>, DomainError>;

    async fn find_dataset(
        &self,
        organization_id: OrganizationId,
        name: &str,
    ) -> Result<Option<PilotDataset>, DomainError>;

    /// Start a run, provisioning the throwaway tenant it will replay into.
    async fn start_run(
        &self,
        dataset_id: DatasetId,
        label: &str,
        engine_version: &str,
        ai: Option<(&str, &str)>,
        at: DateTime<Utc>,
    ) -> Result<PilotRun, DomainError>;

    /// Copy the dataset's signals into the run's tenant as fresh RawSignals.
    /// Returns (replayed, suppressed).
    async fn replay_signals_into_run(
        &self,
        run: &PilotRun,
        at: DateTime<Utc>,
    ) -> Result<(i64, i64), DomainError>;

    async fn finish_run(
        &self,
        run_id: RunId,
        stats: &RunStats,
        error: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError>;

    async fn collect_stats(&self, organization_id: OrganizationId)
        -> Result<RunStats, DomainError>;

    async fn list_runs(&self, dataset_id: DatasetId) -> Result<Vec<PilotRun>, DomainError>;

    async fn find_run(&self, label: &str) -> Result<Option<PilotRun>, DomainError>;
}
