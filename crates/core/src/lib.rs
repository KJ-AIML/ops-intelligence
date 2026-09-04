//! Operations Intelligence Engine — core domain and ports.
//!
//! Boundary rule (architecture 7, tech sheet 26): this crate depends on no
//! transport, no database driver and no vendor SDK. Adapters depend on core;
//! core never depends on an adapter.

pub mod error;
pub mod ids;
pub mod ports;

pub mod domains {
    pub mod organizations;
    pub mod raw_signals;
    pub mod sources;
}

pub use error::DomainError;
pub use ids::{OrganizationId, RawSignalId, SourceId};
pub use ports::{
    Clock, InsertOutcome, OrganizationRepository, RawSignalRepository, SourceRepository,
    SystemClock,
};

pub use domains::organizations::Organization;
pub use domains::raw_signals::{ProcessingStatus, RawSignal};
pub use domains::sources::{Source, SourceType};
