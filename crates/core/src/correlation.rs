use crate::domains::{
    events::{Event, EventState},
    incidents::{CorrelationWindows, Incident, IncidentEventRelation},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrelationAction {
    Open,
    AttachFiring,
    Recover,
    AttachRecovery,
    Reopen,
    Ignore,
}
pub fn relation_for(
    event: &Event,
    open: Option<&Incident>,
    recovered: Option<&Incident>,
    windows: CorrelationWindows,
) -> CorrelationAction {
    if event.state == EventState::Informational
        || (event.state == EventState::Firing && !event.severity.is_actionable())
    {
        return CorrelationAction::Ignore;
    }
    match event.state {
        EventState::Firing => match open {
            Some(_) => CorrelationAction::AttachFiring,
            None => match recovered {
                Some(incident) if incident.can_reopen(event, windows) => CorrelationAction::Reopen,
                _ => CorrelationAction::Open,
            },
        },
        EventState::Resolved => {
            if open.is_some() {
                CorrelationAction::Recover
            } else if recovered.is_some() {
                CorrelationAction::AttachRecovery
            } else {
                CorrelationAction::Ignore
            }
        }
        EventState::Informational => CorrelationAction::Ignore,
    }
}
pub fn firing_relation(
    incident: &mut Incident,
    event: &Event,
    windows: CorrelationWindows,
) -> IncidentEventRelation {
    incident.attach_firing(event, windows)
}
