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
    Acknowledged,
    Recovered,
    Resolved,
}
impl IncidentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledged => "acknowledged",
            Self::Recovered => "recovered",
            Self::Resolved => "resolved",
        }
    }

    /// Active incidents still collect evidence. Acknowledging one means an
    /// engineer is looking at it, not that it stopped happening, so correlation
    /// keeps attaching to it.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Open | Self::Acknowledged)
    }

    /// Resolved is terminal. A later matching event opens a NEW incident, which
    /// is precisely what makes the repeat visible as a recurrence.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Resolved)
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
            "acknowledged" => Ok(Self::Acknowledged),
            "recovered" => Ok(Self::Recovered),
            "resolved" => Ok(Self::Resolved),
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
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
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
            acknowledged_at: None,
            resolved_at: None,
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

    /// Manual triage transitions. These are the only places a human changes an
    /// incident, and they never touch evidence or severity.
    pub fn acknowledge(&mut self, at: DateTime<Utc>) -> Result<(), crate::DomainError> {
        if self.status != IncidentStatus::Open {
            return Err(crate::DomainError::Validation(format!(
                "cannot acknowledge an incident that is {}",
                self.status
            )));
        }
        self.status = IncidentStatus::Acknowledged;
        self.acknowledged_at = Some(at);
        Ok(())
    }

    pub fn resolve(&mut self, at: DateTime<Utc>) -> Result<(), crate::DomainError> {
        if self.status.is_terminal() {
            return Err(crate::DomainError::Validation(
                "incident is already resolved".into(),
            ));
        }
        self.status = IncidentStatus::Resolved;
        self.resolved_at = Some(at);
        Ok(())
    }
    /// Only a recovered incident reopens. A resolved one is closed for good:
    /// an engineer said it was handled, so a repeat is a new occurrence.
    pub fn can_reopen(&self, event: &Event, windows: CorrelationWindows) -> bool {
        self.status == IncidentStatus::Recovered
            && self.recovered_at.is_some_and(|at| {
                event.occurred_at >= at && event.occurred_at - at <= windows.reopen
            })
    }
    pub fn reopen(&mut self, event: &Event) {
        self.status = IncidentStatus::Open;
        self.recovered_at = None;
        // A reopened incident needs fresh attention, so a prior acknowledgement
        // does not carry over. The schema enforces this too.
        self.acknowledged_at = None;
        self.reopened_count += 1;
        self.severity = self.severity.max(event.severity);
        self.last_event_at = event.occurred_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::events::{EventState, Severity};
    use crate::ids::{RawSignalId, SourceId};

    fn event(occurred_at: &str, severity: Severity, state: EventState) -> Event {
        Event {
            id: EventId::new(),
            organization_id: OrganizationId::new(),
            source_id: SourceId::new(),
            raw_signal_id: RawSignalId::new(),
            occurred_at: DateTime::parse_from_rfc3339(occurred_at)
                .unwrap()
                .with_timezone(&Utc),
            environment: Some("production".into()),
            service: Some("payment-api".into()),
            resource: Some("api-prod-01".into()),
            event_family: EventFamily::Availability,
            severity,
            state,
            title: "t".into(),
            message: None,
            labels: Default::default(),
            external_id: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn acknowledged_is_active_so_correlation_keeps_attaching() {
        assert!(IncidentStatus::Open.is_active());
        assert!(IncidentStatus::Acknowledged.is_active());
        assert!(!IncidentStatus::Recovered.is_active());
        assert!(!IncidentStatus::Resolved.is_active());
    }

    #[test]
    fn only_resolved_is_terminal() {
        assert!(IncidentStatus::Resolved.is_terminal());
        for s in [
            IncidentStatus::Open,
            IncidentStatus::Acknowledged,
            IncidentStatus::Recovered,
        ] {
            assert!(!s.is_terminal(), "{s}");
        }
    }

    #[test]
    fn acknowledge_then_resolve_records_its_own_timestamps() {
        let e = event(
            "2026-09-02T09:12:04+07:00",
            Severity::Critical,
            EventState::Firing,
        );
        let mut incident = Incident::opened(&e, Utc::now());
        let at = Utc::now();

        incident.acknowledge(at).unwrap();
        assert_eq!(incident.status, IncidentStatus::Acknowledged);
        assert_eq!(incident.acknowledged_at, Some(at));
        // acknowledging must not invent a recovery
        assert!(incident.recovered_at.is_none());

        incident.resolve(at).unwrap();
        assert_eq!(incident.status, IncidentStatus::Resolved);
        assert_eq!(incident.resolved_at, Some(at));
    }

    #[test]
    fn acknowledge_is_rejected_unless_open_and_resolve_is_not_repeatable() {
        let e = event(
            "2026-09-02T09:12:04+07:00",
            Severity::Warning,
            EventState::Firing,
        );
        let mut incident = Incident::opened(&e, Utc::now());
        incident.acknowledge(Utc::now()).unwrap();
        assert!(incident.acknowledge(Utc::now()).is_err());

        incident.resolve(Utc::now()).unwrap();
        assert!(incident.resolve(Utc::now()).is_err());
    }

    #[test]
    fn a_resolved_incident_never_reopens() {
        let fire = event(
            "2026-09-02T11:02:09+07:00",
            Severity::Warning,
            EventState::Firing,
        );
        let mut incident = Incident::opened(&fire, Utc::now());
        let recovery = event(
            "2026-09-02T11:08:44+07:00",
            Severity::Warning,
            EventState::Resolved,
        );
        incident.recover(&recovery);

        // inside the reopen window it would normally reopen...
        let refire = event(
            "2026-09-02T11:19:12+07:00",
            Severity::Warning,
            EventState::Firing,
        );
        assert!(incident.can_reopen(&refire, CorrelationWindows::default()));

        // ...but not once a human has resolved it.
        incident.resolve(Utc::now()).unwrap();
        assert!(!incident.can_reopen(&refire, CorrelationWindows::default()));
    }

    #[test]
    fn fingerprint_separates_families_on_an_identical_resource() {
        let a = event(
            "2026-09-02T09:12:04+07:00",
            Severity::Critical,
            EventState::Firing,
        );
        let mut b = event(
            "2026-09-02T09:13:11+07:00",
            Severity::Critical,
            EventState::Firing,
        );
        b.organization_id = a.organization_id;
        b.event_family = EventFamily::SaturationCpu;

        let fa = IncidentFingerprint::from_event(&a);
        let fb = IncidentFingerprint::from_event(&b);
        assert_ne!(fa.key(), fb.key(), "decision 0003: families never merge");
    }
}
