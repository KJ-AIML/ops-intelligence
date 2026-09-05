use crate::domains::events::{Event, EventFamily, Severity};
use crate::ids::{EventId, IncidentId, OrganizationId};
use chrono::{DateTime, Duration, Utc};
use std::{fmt, str::FromStr};

/// Conservative identity: family is mandatory, so different families never merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentFingerprint {
    pub organization_id: OrganizationId,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: EventFamily,
}
impl IncidentFingerprint {
    pub fn from_event(event: &Event) -> Self {
        Self {
            organization_id: event.organization_id,
            environment: event.environment.clone(),
            service: event.service.clone(),
            resource: event.resource.clone(),
            event_family: event.event_family,
        }
    }
    pub fn key(&self) -> String {
        fn field(v: Option<&str>) -> String {
            v.map_or_else(|| "-".into(), |v| format!("{}:{v}", v.len()))
        }
        format!(
            "{}|{}|{}|{}|{}",
            self.organization_id,
            field(self.environment.as_deref()),
            field(self.service.as_deref()),
            field(self.resource.as_deref()),
            self.event_family
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentStatus {
    Open,
    Recovered,
}
impl IncidentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Recovered => "recovered",
        }
    }
}
impl fmt::Display for IncidentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for IncidentStatus {
    type Err = crate::DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "recovered" => Ok(Self::Recovered),
            _ => Err(crate::DomainError::Validation(format!(
                "unknown incident status: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentEventRelation {
    Trigger,
    Duplicate,
    Recovery,
    Update,
}
impl IncidentEventRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Duplicate => "duplicate",
            Self::Recovery => "recovery",
            Self::Update => "update",
        }
    }
}
impl fmt::Display for IncidentEventRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for IncidentEventRelation {
    type Err = crate::DomainError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "trigger" => Ok(Self::Trigger),
            "duplicate" => Ok(Self::Duplicate),
            "recovery" => Ok(Self::Recovery),
            "update" => Ok(Self::Update),
            _ => Err(crate::DomainError::Validation(format!(
                "unknown incident-event relation: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub id: IncidentId,
    pub organization_id: OrganizationId,
    pub fingerprint: IncidentFingerprint,
    pub status: IncidentStatus,
    pub severity: Severity,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub recovered_at: Option<DateTime<Utc>>,
    pub reopened_count: u32,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentEvent {
    pub incident_id: IncidentId,
    pub event_id: EventId,
    pub relation: IncidentEventRelation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrelationWindows {
    pub duplicate: Duration,
    pub reopen: Duration,
}
impl Default for CorrelationWindows {
    fn default() -> Self {
        Self {
            duplicate: Duration::seconds(600),
            reopen: Duration::seconds(1800),
        }
    }
}
impl Incident {
    pub fn opened(event: &Event, created_at: DateTime<Utc>) -> Self {
        Self {
            id: IncidentId::new(),
            organization_id: event.organization_id,
            fingerprint: IncidentFingerprint::from_event(event),
            status: IncidentStatus::Open,
            severity: event.severity,
            started_at: event.occurred_at,
            last_event_at: event.occurred_at,
            recovered_at: None,
            reopened_count: 0,
            created_at,
        }
    }
    pub fn attach_firing(
        &mut self,
        event: &Event,
        windows: CorrelationWindows,
    ) -> IncidentEventRelation {
        let relation = if event.occurred_at - self.last_event_at <= windows.duplicate {
            IncidentEventRelation::Duplicate
        } else {
            IncidentEventRelation::Update
        };
        self.severity = self.severity.max(event.severity);
        self.last_event_at = event.occurred_at;
        relation
    }
    pub fn recover(&mut self, event: &Event) {
        self.status = IncidentStatus::Recovered;
        self.last_event_at = event.occurred_at;
        self.recovered_at = Some(event.occurred_at);
    }
    pub fn can_reopen(&self, event: &Event, windows: CorrelationWindows) -> bool {
        self.status == IncidentStatus::Recovered
            && self.recovered_at.is_some_and(|at| {
                event.occurred_at >= at && event.occurred_at - at <= windows.reopen
            })
    }
    pub fn reopen(&mut self, event: &Event) {
        self.status = IncidentStatus::Open;
        self.recovered_at = None;
        self.reopened_count += 1;
        self.severity = self.severity.max(event.severity);
        self.last_event_at = event.occurred_at;
    }
}
