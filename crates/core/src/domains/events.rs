use crate::error::DomainError;
use crate::ids::{EventId, OrganizationId, RawSignalId, SourceId};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Normalized severity (tech sheet 8).
///
/// Ordered deliberately: `Ord` is what lets an incident take the max severity of
/// its members rather than the severity of whichever event happened to arrive
/// first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Debug,
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// info and debug are noise, not conditions: they never open an incident.
    pub const fn is_actionable(&self) -> bool {
        matches!(self, Self::Warning | Self::Critical)
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "critical" => Ok(Self::Critical),
            other => Err(DomainError::Validation(format!(
                "unknown severity: {other}"
            ))),
        }
    }
}

/// Canonical event state. Vendor dialects (`alerting`, `Fired`, `Down`,
/// `FAILED`, ...) stop at the normalization layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventState {
    Firing,
    Resolved,
    Informational,
}

impl EventState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Resolved => "resolved",
            Self::Informational => "informational",
        }
    }
}

impl fmt::Display for EventState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventState {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "firing" => Ok(Self::Firing),
            "resolved" => Ok(Self::Resolved),
            "informational" => Ok(Self::Informational),
            other => Err(DomainError::Validation(format!(
                "unknown event state: {other}"
            ))),
        }
    }
}

/// Canonical event taxonomy (decision 0002).
///
/// Flat and small on purpose. The `Saturation*` families are separate rather
/// than one `Saturation` because they are fingerprint inputs: CPU pressure and
/// disk pressure on the same host are different operational problems and must
/// not collapse into one incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventFamily {
    Availability,
    Latency,
    ErrorRate,
    SaturationCpu,
    SaturationMemory,
    SaturationDisk,
    Connectivity,
    Certificate,
    Backup,
    Job,
    Unclassified,
}

impl EventFamily {
    pub const ALL: [EventFamily; 11] = [
        Self::Availability,
        Self::Latency,
        Self::ErrorRate,
        Self::SaturationCpu,
        Self::SaturationMemory,
        Self::SaturationDisk,
        Self::Connectivity,
        Self::Certificate,
        Self::Backup,
        Self::Job,
        Self::Unclassified,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Availability => "availability",
            Self::Latency => "latency",
            Self::ErrorRate => "error_rate",
            Self::SaturationCpu => "saturation_cpu",
            Self::SaturationMemory => "saturation_memory",
            Self::SaturationDisk => "saturation_disk",
            Self::Connectivity => "connectivity",
            Self::Certificate => "certificate",
            Self::Backup => "backup",
            Self::Job => "job",
            Self::Unclassified => "unclassified",
        }
    }
}

impl fmt::Display for EventFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventFamily {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EventFamily::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| DomainError::Validation(format!("unknown event family: {s}")))
    }
}

/// A normalized operational fact.
///
/// `occurred_at` is when the condition happened according to the source, and is
/// the only time value correlation may use. It is NOT `RawSignal::received_at`,
/// which is when we ingested it — importing a day of history in one second must
/// not collapse that day into a single correlation window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: EventId,
    pub organization_id: OrganizationId,
    pub source_id: SourceId,
    pub raw_signal_id: RawSignalId,
    pub occurred_at: DateTime<Utc>,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: EventFamily,
    pub severity: Severity,
    pub state: EventState,
    pub title: String,
    pub message: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub external_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_so_incidents_can_take_the_max() {
        assert!(Severity::Critical > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert!(Severity::Info > Severity::Debug);
        let members = [Severity::Warning, Severity::Critical, Severity::Info];
        assert_eq!(members.into_iter().max().unwrap(), Severity::Critical);
    }

    #[test]
    fn only_warning_and_critical_are_actionable() {
        assert!(Severity::Critical.is_actionable());
        assert!(Severity::Warning.is_actionable());
        assert!(!Severity::Info.is_actionable());
        assert!(!Severity::Debug.is_actionable());
    }

    #[test]
    fn every_family_round_trips_through_its_wire_string() {
        for family in EventFamily::ALL {
            assert_eq!(EventFamily::from_str(family.as_str()).unwrap(), family);
        }
        assert!(EventFamily::from_str("saturation").is_err());
    }

    #[test]
    fn severity_and_state_round_trip() {
        for s in [
            Severity::Debug,
            Severity::Info,
            Severity::Warning,
            Severity::Critical,
        ] {
            assert_eq!(Severity::from_str(s.as_str()).unwrap(), s);
        }
        for s in [
            EventState::Firing,
            EventState::Resolved,
            EventState::Informational,
        ] {
            assert_eq!(EventState::from_str(s.as_str()).unwrap(), s);
        }
    }
}
