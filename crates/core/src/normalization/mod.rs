//! Normalization layer — the seam between vendor adapters and core (decision 0002).
//!
//! ```text
//! Source Adapter  ->  extracts raw source facts and hints   (vendor-specific)
//!        |
//! Normalization   ->  maps them to canonical values          (vendor-agnostic, here)
//!        |
//! Core            ->  consumes canonical values only
//! ```
//!
//! Nothing in this module knows what Grafana or Azure Monitor are.

pub mod family;

use crate::domains::raw_signals::ProcessingStatus;
use crate::ports::{Clock, EventRepository, SignalNormalizer};

use crate::domains::events::{Event, EventFamily, EventState, Severity};
use crate::error::DomainError;
use crate::ids::{EventId, OrganizationId, RawSignalId, SourceId};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProcessingReport {
    pub processed: usize,
    pub failed: usize,
}

/// Drain currently available work for one explicit tenant. DB failures are fatal.
pub async fn process_received(
    repository: &dyn EventRepository,
    organization_id: OrganizationId,
    normalizer: &dyn SignalNormalizer,
    clock: &dyn Clock,
) -> Result<ProcessingReport, DomainError> {
    let mut report = ProcessingReport::default();
    while let Some(status) = repository
        .process_next(organization_id, normalizer, clock)
        .await?
    {
        match status {
            ProcessingStatus::Processed => report.processed += 1,
            ProcessingStatus::Failed => report.failed += 1,
            other => {
                return Err(DomainError::Persistence(format!(
                    "unexpected processing outcome: {other}"
                )))
            }
        }
    }
    Ok(report)
}

/// What an adapter passes to the classifier. Deliberately neutral: an adapter
/// supplies evidence, not a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FamilyHints {
    pub title: String,
    pub message: Option<String>,
    pub labels: BTreeMap<String, String>,
    /// Vendor metric or alert-rule name, when the payload carries one.
    pub metric_name: Option<String>,
    /// e.g. an uptime checker's probe type.
    pub monitor_type: Option<String>,
    /// The vendor's own category, verbatim. Never trusted as canonical.
    pub vendor_category: Option<String>,
}

/// Everything an adapter can extract from one source payload.
///
/// `severity_raw` and `state_raw` are the vendor's own dialect, untouched. The
/// adapter does not interpret them; this module does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFacts {
    /// Which originating system the signal came from, used to select config.
    pub origin: String,
    /// The source's own timestamp for the condition. Never ingest time.
    pub occurred_at: DateTime<Utc>,
    pub title: String,
    pub message: Option<String>,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub severity_raw: Option<String>,
    pub state_raw: Option<String>,
    pub external_id: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub hints: FamilyHints,
}

/// Severity to use when a source supplies none at all (uptime checkers commonly
/// do not). Per-source, because "this probe is down" means different things to
/// different tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeverityWhenAbsent {
    pub firing: Severity,
    pub resolved: Severity,
    pub informational: Severity,
}

impl Default for SeverityWhenAbsent {
    fn default() -> Self {
        Self {
            firing: Severity::Warning,
            resolved: Severity::Info,
            informational: Severity::Info,
        }
    }
}

/// Per-source normalization configuration.
///
/// This is DATA, not compiled-in logic (decision 0002). Source inventory 11 says
/// explicitly not to fix severity mappings before seeing real source data, so
/// changing one must never require a recompile.
///
/// ponytail: the built-in `for_origin` table stands in for `sources.config` until
/// sources are configurable through the API in a later slice. Move it to the
/// database then; the shape does not change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceNormalizationConfig {
    /// Raw severity token (compared case-insensitively) -> canonical severity.
    pub severity: BTreeMap<String, Severity>,
    /// Raw state token (compared case-insensitively) -> canonical state.
    pub state: BTreeMap<String, EventState>,
    pub severity_when_absent: SeverityWhenAbsent,
    /// Layer-2 rules: exact vendor rule/metric/category name -> canonical family.
    pub family_rules: BTreeMap<String, EventFamily>,
}

fn map_of<T: Copy>(pairs: &[(&str, T)]) -> BTreeMap<String, T> {
    pairs.iter().map(|(k, v)| (k.to_lowercase(), *v)).collect()
}

/// State vocabulary is shared across sources; severity vocabulary is not.
fn default_state_map() -> BTreeMap<String, EventState> {
    map_of(&[
        ("alerting", EventState::Firing),
        ("firing", EventState::Firing),
        ("fired", EventState::Firing),
        ("down", EventState::Firing),
        ("failed", EventState::Firing),
        ("ok", EventState::Resolved),
        ("resolved", EventState::Resolved),
        ("up", EventState::Resolved),
        ("success", EventState::Resolved),
    ])
}

impl Default for SourceNormalizationConfig {
    fn default() -> Self {
        Self {
            severity: map_of(&[
                ("critical", Severity::Critical),
                ("error", Severity::Critical),
                ("warning", Severity::Warning),
                ("warn", Severity::Warning),
                ("info", Severity::Info),
                ("informational", Severity::Info),
                ("debug", Severity::Debug),
            ]),
            state: default_state_map(),
            severity_when_absent: SeverityWhenAbsent::default(),
            family_rules: BTreeMap::new(),
        }
    }
}

impl SourceNormalizationConfig {
    /// Built-in defaults for the origins present in the pilot dataset.
    ///
    /// These mappings are provisional. Source inventory 11 requires them to be
    /// re-derived from real source data before the pilot.
    pub fn for_origin(origin: &str) -> Self {
        let base = Self::default();
        match origin {
            "azure_monitor" => Self {
                severity: map_of(&[
                    ("sev0", Severity::Critical),
                    ("sev1", Severity::Critical),
                    ("sev2", Severity::Warning),
                    ("sev3", Severity::Info),
                    ("sev4", Severity::Info),
                ]),
                ..base
            },
            // Uptime checkers report reachability, not severity. Down => critical
            // is a blunt default that makes an internal wiki outage "critical";
            // that is a tuning problem for the pilot, which is exactly why this
            // lives in configuration.
            "uptime_kuma" => Self {
                severity_when_absent: SeverityWhenAbsent {
                    firing: Severity::Critical,
                    resolved: Severity::Info,
                    informational: Severity::Info,
                },
                ..base
            },
            _ => base,
        }
    }
}

/// Map one adapter's extracted facts into a canonical Event.
///
/// Returns `Err` rather than guessing. A signal that cannot be normalized is
/// recorded as failed with its reason — it is never silently dropped.
#[allow(clippy::too_many_arguments)]
pub fn normalize(
    organization_id: OrganizationId,
    source_id: SourceId,
    raw_signal_id: RawSignalId,
    facts: &SourceFacts,
    config: &SourceNormalizationConfig,
    created_at: DateTime<Utc>,
) -> Result<Event, DomainError> {
    let title = facts.title.trim();
    if title.is_empty() {
        return Err(DomainError::Validation("event title is empty".into()));
    }

    let state = normalize_state(facts.state_raw.as_deref(), config)?;
    let severity = normalize_severity(facts.severity_raw.as_deref(), state, config)?;
    let event_family = family::classify(&facts.hints, state, &config.family_rules);

    Ok(Event {
        id: EventId::new(),
        organization_id,
        source_id,
        raw_signal_id,
        occurred_at: facts.occurred_at,
        environment: facts.environment.clone(),
        service: facts.service.clone(),
        resource: facts.resource.clone(),
        event_family,
        severity,
        state,
        title: title.to_string(),
        message: facts.message.clone(),
        labels: facts.labels.clone(),
        external_id: facts.external_id.clone(),
        created_at,
    })
}

pub fn normalize_state(
    raw: Option<&str>,
    config: &SourceNormalizationConfig,
) -> Result<EventState, DomainError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        // A source that says nothing about state is reporting a notice, not a
        // condition.
        None => Ok(EventState::Informational),
        Some(token) => config
            .state
            .get(&token.to_lowercase())
            .copied()
            .ok_or_else(|| DomainError::Validation(format!("unmapped state token: {token:?}"))),
    }
}

pub fn normalize_severity(
    raw: Option<&str>,
    state: EventState,
    config: &SourceNormalizationConfig,
) -> Result<Severity, DomainError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(match state {
            EventState::Firing => config.severity_when_absent.firing,
            EventState::Resolved => config.severity_when_absent.resolved,
            EventState::Informational => config.severity_when_absent.informational,
        }),
        Some(token) => config
            .severity
            .get(&token.to_lowercase())
            .copied()
            .ok_or_else(|| DomainError::Validation(format!("unmapped severity token: {token:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(origin: &str, severity: Option<&str>, state: Option<&str>) -> SourceFacts {
        SourceFacts {
            origin: origin.to_string(),
            occurred_at: "2026-09-02T09:12:04+07:00"
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc),
            title: "CPU percentage greater than 90 (api-prod-01)".into(),
            message: None,
            environment: Some("production".into()),
            service: Some("payment-api".into()),
            resource: Some("api-prod-01".into()),
            severity_raw: severity.map(str::to_string),
            state_raw: state.map(str::to_string),
            external_id: Some("AZ-1".into()),
            labels: BTreeMap::new(),
            hints: FamilyHints {
                title: "CPU percentage greater than 90 (api-prod-01)".into(),
                ..Default::default()
            },
        }
    }

    fn normalized(f: &SourceFacts) -> Event {
        let cfg = SourceNormalizationConfig::for_origin(&f.origin);
        normalize(
            OrganizationId::new(),
            SourceId::new(),
            RawSignalId::new(),
            f,
            &cfg,
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn azure_severity_dialect_maps_to_canonical() {
        assert_eq!(
            normalized(&facts("azure_monitor", Some("Sev1"), Some("Fired"))).severity,
            Severity::Critical
        );
        assert_eq!(
            normalized(&facts("azure_monitor", Some("Sev2"), Some("Fired"))).severity,
            Severity::Warning
        );
        assert_eq!(
            normalized(&facts("azure_monitor", Some("Sev4"), Some("Fired"))).severity,
            Severity::Info
        );
    }

    #[test]
    fn state_dialects_all_collapse_to_three_canonical_values() {
        for token in ["alerting", "Fired", "Down", "FAILED"] {
            assert_eq!(
                normalized(&facts("grafana", Some("warning"), Some(token))).state,
                EventState::Firing,
                "{token}"
            );
        }
        for token in ["ok", "Resolved", "Up", "OK"] {
            assert_eq!(
                normalized(&facts("grafana", Some("warning"), Some(token))).state,
                EventState::Resolved,
                "{token}"
            );
        }
        assert_eq!(
            normalized(&facts("grafana", Some("info"), None)).state,
            EventState::Informational
        );
    }

    #[test]
    fn source_without_severity_falls_back_per_source() {
        // uptime_kuma sends no severity at all
        assert_eq!(
            normalized(&facts("uptime_kuma", None, Some("Down"))).severity,
            Severity::Critical
        );
        assert_eq!(
            normalized(&facts("uptime_kuma", None, Some("Up"))).severity,
            Severity::Info
        );
        // a source with no special config uses the conservative default
        assert_eq!(
            normalized(&facts("something_new", None, Some("Down"))).severity,
            Severity::Warning
        );
    }

    #[test]
    fn unmapped_tokens_fail_loudly_rather_than_defaulting() {
        let cfg = SourceNormalizationConfig::default();
        assert!(normalize_state(Some("wobbling"), &cfg).is_err());
        assert!(normalize_severity(Some("Sev9"), EventState::Firing, &cfg).is_err());
    }

    #[test]
    fn empty_title_is_rejected() {
        let mut f = facts("grafana", Some("warning"), Some("alerting"));
        f.title = "   ".into();
        let cfg = SourceNormalizationConfig::default();
        assert!(normalize(
            OrganizationId::new(),
            SourceId::new(),
            RawSignalId::new(),
            &f,
            &cfg,
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn occurred_at_comes_from_the_source_not_the_clock() {
        let f = facts("grafana", Some("warning"), Some("alerting"));
        let event = normalized(&f);
        assert_eq!(event.occurred_at, f.occurred_at);
        assert_ne!(event.occurred_at, event.created_at);
    }
}
