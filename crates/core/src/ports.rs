use crate::domains::organizations::Organization;
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
