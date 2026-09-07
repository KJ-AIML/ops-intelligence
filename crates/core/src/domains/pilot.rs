//! Pilot Lab domain — datasets and replay runs.
//!
//! The synthetic fixture answers "does the engine do what we designed?".
//! This answers "does it do something useful to real traffic?" — which needs
//! real signals, captured once and replayable many times.

use crate::error::DomainError;
use crate::ids::{DatasetId, OrganizationId, RunId};
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;

/// A frozen, named set of captured RawSignals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotDataset {
    pub id: DatasetId,
    pub organization_id: OrganizationId,
    pub name: String,
    pub description: Option<String>,
    pub frozen_at: Option<DateTime<Utc>>,
    pub signal_count: i64,
    pub created_at: DateTime<Utc>,
}

impl PilotDataset {
    /// A frozen dataset is a regression baseline. One that can still change is
    /// not, so freezing is one-way.
    pub const fn is_frozen(&self) -> bool {
        self.frozen_at.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RunStatus {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(DomainError::Validation(format!(
                "unknown run status: {other}"
            ))),
        }
    }
}

/// What a replay produced. Comparing two runs is comparing two of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunStats {
    pub signals_replayed: i64,
    pub signals_suppressed: i64,
    pub events: i64,
    pub events_failed: i64,
    pub events_without_incident: i64,
    pub incidents: i64,
    pub open: i64,
    pub recovered: i64,
    pub critical: i64,
    pub duplicate_relations: i64,
    pub recovery_relations: i64,
    pub recurrence_patterns: i64,
    pub insights_ok: i64,
    pub insights_failed: i64,
}

impl RunStats {
    /// How hard the engine compressed the stream: events per incident.
    ///
    /// Higher is not automatically better. A correlator that merges everything
    /// scores brilliantly here and is useless, which is exactly why compression
    /// is reported next to correctness rather than instead of it.
    pub fn compression(&self) -> Option<f64> {
        (self.incidents > 0)
            .then(|| ((self.events as f64 / self.incidents as f64) * 100.0).round() / 100.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotRun {
    pub id: RunId,
    pub dataset_id: DatasetId,
    pub target_organization_id: OrganizationId,
    pub label: String,
    pub engine_version: String,
    pub ai_enabled: bool,
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub status: RunStatus,
    pub stats: RunStats,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// One field's worth of difference between two runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatDelta {
    pub field: &'static str,
    pub before: i64,
    pub after: i64,
}

impl StatDelta {
    pub const fn change(&self) -> i64 {
        self.after - self.before
    }
}

/// Compare two runs field by field, reporting only what moved.
///
/// Deliberately dumb arithmetic: the point of a comparison is to be obviously
/// correct, so an engineer trusts a surprising number instead of the tool.
pub fn compare(before: &RunStats, after: &RunStats) -> Vec<StatDelta> {
    let fields: [(&'static str, i64, i64); 14] = [
        (
            "signals_replayed",
            before.signals_replayed,
            after.signals_replayed,
        ),
        (
            "signals_suppressed",
            before.signals_suppressed,
            after.signals_suppressed,
        ),
        ("events", before.events, after.events),
        ("events_failed", before.events_failed, after.events_failed),
        (
            "events_without_incident",
            before.events_without_incident,
            after.events_without_incident,
        ),
        ("incidents", before.incidents, after.incidents),
        ("open", before.open, after.open),
        ("recovered", before.recovered, after.recovered),
        ("critical", before.critical, after.critical),
        (
            "duplicate_relations",
            before.duplicate_relations,
            after.duplicate_relations,
        ),
        (
            "recovery_relations",
            before.recovery_relations,
            after.recovery_relations,
        ),
        (
            "recurrence_patterns",
            before.recurrence_patterns,
            after.recurrence_patterns,
        ),
        ("insights_ok", before.insights_ok, after.insights_ok),
        (
            "insights_failed",
            before.insights_failed,
            after.insights_failed,
        ),
    ];

    fields
        .into_iter()
        .filter(|(_, b, a)| b != a)
        .map(|(field, before, after)| StatDelta {
            field,
            before,
            after,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_reports_only_what_moved() {
        let before = RunStats {
            events: 300,
            incidents: 42,
            open: 11,
            ..Default::default()
        };
        let after = RunStats {
            events: 300,
            incidents: 35,
            open: 11,
            ..Default::default()
        };

        let deltas = compare(&before, &after);
        assert_eq!(deltas.len(), 1, "only incidents changed: {deltas:?}");
        assert_eq!(deltas[0].field, "incidents");
        assert_eq!(deltas[0].change(), -7);
    }

    #[test]
    fn identical_runs_produce_no_deltas() {
        let stats = RunStats {
            events: 67,
            incidents: 22,
            ..Default::default()
        };
        assert!(compare(&stats, &stats).is_empty());
    }

    #[test]
    fn compression_is_reported_but_is_not_a_score() {
        let honest = RunStats {
            events: 300,
            incidents: 42,
            ..Default::default()
        };
        let over_merging = RunStats {
            events: 300,
            incidents: 1,
            ..Default::default()
        };
        // The useless correlator "wins" on compression alone. That is the trap
        // this number exists to make visible, not to reward.
        assert!(over_merging.compression().unwrap() > honest.compression().unwrap());
        assert_eq!(honest.compression(), Some(7.14));
    }

    #[test]
    fn compression_is_undefined_rather_than_infinite_with_no_incidents() {
        assert_eq!(RunStats::default().compression(), None);
    }

    #[test]
    fn a_dataset_is_a_baseline_only_once_frozen() {
        let mut dataset = PilotDataset {
            id: DatasetId::new(),
            organization_id: OrganizationId::new(),
            name: "infra-week-01".into(),
            description: None,
            frozen_at: None,
            signal_count: 384,
            created_at: Utc::now(),
        };
        assert!(!dataset.is_frozen());
        dataset.frozen_at = Some(Utc::now());
        assert!(dataset.is_frozen());
    }

    #[test]
    fn run_status_round_trips() {
        for s in [RunStatus::Running, RunStatus::Completed, RunStatus::Failed] {
            assert_eq!(RunStatus::from_str(s.as_str()).unwrap(), s);
        }
        assert!(RunStatus::from_str("probably").is_err());
    }
}
