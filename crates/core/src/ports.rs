use crate::domains::events::Event;
use crate::domains::incidents::{Incident, IncidentEvent};
use crate::domains::organizations::Organization;
use crate::domains::raw_signals::ProcessingStatus;
use crate::domains::raw_signals::RawSignal;
use crate::domains::sources::{Source, SourceType};
use crate::error::DomainError;
use crate::ids::{OrganizationId, RawSignalId};
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
