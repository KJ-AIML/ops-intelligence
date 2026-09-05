//! Deterministic operational insights.
//!
//! Everything here is computed with arithmetic and sorting. Per principle P1 and
//! guardrail 34.4, "what is unresolved", "what keeps happening" and "what is
//! noisiest" are counting problems, not reasoning problems — an LLM must never
//! be asked to answer them.
//!
//! The bounded AI reasoner arrives later and adds *interpretation* on top of
//! these facts. It does not replace them, and it never changes a number.

use crate::domains::events::{EventFamily, Severity};
use crate::domains::incidents::IncidentStatus;
use crate::ids::{IncidentId, SourceId};
use chrono::{DateTime, Utc};

/// Answers the Operations View's five questions in one payload.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationsSummary {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub raw_signals: i64,
    pub events: i64,
    pub events_without_incident: i64,
    pub incidents: i64,
    pub open: i64,
    pub acknowledged: i64,
    pub recovered: i64,
    pub resolved: i64,
    pub critical: i64,
    /// Active and nobody has looked at it yet.
    pub needs_attention: i64,
    pub noisiest_sources: Vec<SourceNoise>,
    pub recurring: Vec<RecurringPattern>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceNoise {
    pub source_id: SourceId,
    pub name: String,
    pub source_type: String,
    pub signals: i64,
    pub events: i64,
    pub incidents: i64,
    /// Share of total signal volume, one decimal place.
    pub share_percent: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurringPattern {
    pub fingerprint: String,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: EventFamily,
    pub occurrences: i64,
    pub last_seen_at: DateTime<Utc>,
    pub currently_active: bool,
}

/// One row of the incident list, with the counts the UI needs to rank it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentSummary {
    pub id: IncidentId,
    pub title: String,
    pub status: IncidentStatus,
    pub severity: Severity,
    pub environment: Option<String>,
    pub service: Option<String>,
    pub resource: Option<String>,
    pub event_family: EventFamily,
    pub event_count: i64,
    pub source_count: i64,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub recovered_at: Option<DateTime<Utc>>,
    pub reopened_count: i64,
    /// How many incidents share this fingerprint, this one included.
    pub occurrences: i64,
}

/// "What should I investigate first?" — deterministic, explainable, and stable.
///
/// Higher sorts first. The weights encode a triage order an engineer would
/// recognise rather than anything learned:
///   still happening   > already recovered
///   unacknowledged    > someone is on it
///   critical          > warning
///   recurring         > one-off
/// Recency breaks ties, so a fresh warning outranks a stale one.
pub fn attention_score(incident: &IncidentSummary) -> i64 {
    let mut score = 0;

    score += match incident.status {
        IncidentStatus::Open => 1_000_000,
        IncidentStatus::Acknowledged => 600_000,
        IncidentStatus::Recovered => 100_000,
        IncidentStatus::Resolved => 0,
    };

    score += match incident.severity {
        Severity::Critical => 200_000,
        Severity::Warning => 80_000,
        Severity::Info => 5_000,
        Severity::Debug => 0,
    };

    // A repeat is worth attention even at lower severity, but must not outrank
    // an active critical: capped well below one severity step.
    score += (incident.occurrences.saturating_sub(1)).min(10) * 5_000;

    // Correlating across two tools is weak evidence that something real is
    // happening, rather than one chatty source.
    if incident.source_count > 1 {
        score += 10_000;
    }

    score
}

/// Rank incidents for the "needs attention" panel. Ties fall back to most recent
/// activity, so the order is total and stable across requests.
pub fn rank_by_attention(incidents: &mut [IncidentSummary]) {
    incidents.sort_by(|a, b| {
        attention_score(b)
            .cmp(&attention_score(a))
            .then(b.last_event_at.cmp(&a.last_event_at))
            .then(a.id.as_uuid().cmp(&b.id.as_uuid()))
    });
}

/// Share of total volume per source, to one decimal place. Returns 0.0 rather
/// than NaN when there is no traffic at all.
pub fn share_percent(count: i64, total: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    ((count as f64 * 1000.0 / total as f64).round()) / 10.0
}

/// A source is "noisy" when it contributes far more volume than it does
/// incidents. This is the ratio behind "Grafana generated 38% of today's signal
/// volume but only 12% of incidents".
pub fn noise_ratio(source: &SourceNoise, total_incidents: i64) -> Option<f64> {
    if total_incidents <= 0 || source.incidents <= 0 {
        return None;
    }
    let incident_share = share_percent(source.incidents, total_incidents);
    (incident_share > 0.0).then(|| (source.share_percent / incident_share * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incident(
        status: IncidentStatus,
        severity: Severity,
        occurrences: i64,
        last_event_at: &str,
    ) -> IncidentSummary {
        IncidentSummary {
            id: IncidentId::new(),
            title: "t".into(),
            status,
            severity,
            environment: None,
            service: None,
            resource: None,
            event_family: EventFamily::Availability,
            event_count: 1,
            source_count: 1,
            started_at: Utc::now(),
            last_event_at: DateTime::parse_from_rfc3339(last_event_at)
                .unwrap()
                .with_timezone(&Utc),
            recovered_at: None,
            reopened_count: 0,
            occurrences,
        }
    }

    const T: &str = "2026-09-02T12:00:00+00:00";

    #[test]
    fn active_incidents_outrank_recovered_ones_regardless_of_severity() {
        let open_warning = incident(IncidentStatus::Open, Severity::Warning, 1, T);
        let recovered_critical = incident(IncidentStatus::Recovered, Severity::Critical, 1, T);
        assert!(attention_score(&open_warning) > attention_score(&recovered_critical));
    }

    #[test]
    fn unacknowledged_outranks_acknowledged_at_equal_severity() {
        let open = incident(IncidentStatus::Open, Severity::Critical, 1, T);
        let ack = incident(IncidentStatus::Acknowledged, Severity::Critical, 1, T);
        assert!(attention_score(&open) > attention_score(&ack));
    }

    #[test]
    fn recurrence_lifts_an_incident_but_never_past_an_active_critical() {
        let recurring_warning = incident(IncidentStatus::Open, Severity::Warning, 8, T);
        let one_off_warning = incident(IncidentStatus::Open, Severity::Warning, 1, T);
        let critical = incident(IncidentStatus::Open, Severity::Critical, 1, T);

        assert!(attention_score(&recurring_warning) > attention_score(&one_off_warning));
        assert!(
            attention_score(&critical) > attention_score(&recurring_warning),
            "a recurring warning must not drown out an active critical"
        );
    }

    #[test]
    fn resolved_incidents_sink_to_the_bottom() {
        let resolved = incident(IncidentStatus::Resolved, Severity::Critical, 10, T);
        let open_info = incident(IncidentStatus::Open, Severity::Info, 1, T);
        assert!(attention_score(&open_info) > attention_score(&resolved));
    }

    #[test]
    fn ranking_is_total_and_recency_breaks_ties() {
        let mut list = vec![
            incident(
                IncidentStatus::Open,
                Severity::Warning,
                1,
                "2026-09-02T08:00:00+00:00",
            ),
            incident(
                IncidentStatus::Open,
                Severity::Critical,
                1,
                "2026-09-02T07:00:00+00:00",
            ),
            incident(IncidentStatus::Recovered, Severity::Critical, 1, T),
            incident(
                IncidentStatus::Open,
                Severity::Warning,
                1,
                "2026-09-02T09:00:00+00:00",
            ),
        ];
        rank_by_attention(&mut list);

        assert_eq!(list[0].severity, Severity::Critical);
        assert_eq!(list[0].status, IncidentStatus::Open);
        // the two open warnings, most recent first
        assert_eq!(
            list[1].last_event_at.to_rfc3339(),
            "2026-09-02T09:00:00+00:00"
        );
        assert_eq!(
            list[2].last_event_at.to_rfc3339(),
            "2026-09-02T08:00:00+00:00"
        );
        assert_eq!(list[3].status, IncidentStatus::Recovered);
    }

    #[test]
    fn share_percent_matches_the_fixture_and_survives_an_empty_day() {
        // grafana contributes 31 of 69 rows in the pilot dataset
        assert_eq!(share_percent(31, 69), 44.9);
        assert_eq!(share_percent(21, 69), 30.4);
        assert_eq!(share_percent(0, 69), 0.0);
        assert_eq!(share_percent(5, 0), 0.0, "no traffic must not produce NaN");
    }

    #[test]
    fn noise_ratio_flags_a_source_that_shouts_more_than_it_matters() {
        let noisy = SourceNoise {
            source_id: SourceId::new(),
            name: "csv:grafana".into(),
            source_type: "csv_import".into(),
            signals: 38,
            events: 38,
            incidents: 3,
            share_percent: 38.0,
        };
        // 38% of volume, 12% of incidents => ratio above 1 means over-reporting
        let ratio = noise_ratio(&noisy, 25).unwrap();
        assert!(ratio > 1.0, "got {ratio}");

        let silent = SourceNoise {
            incidents: 0,
            ..noisy
        };
        assert_eq!(noise_ratio(&silent, 25), None);
    }
}
