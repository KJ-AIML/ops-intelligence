//! Grafana Alerting source adapter (tech sheet 15).
//!
//! Grafana's webhook contact point posts one JSON body per notification group,
//! carrying an `alerts` array. The engine's evidence unit is one RawSignal per
//! alert, so the batch is split at the ingestion boundary and every alert keeps
//! the group-level context it arrived with. Like every source adapter, this
//! crate extracts facts and hints only; severity, state and family mapping stay
//! in core normalization (decision 0002).

use chrono::{DateTime, Utc};
use ops_core::normalization::{normalize, FamilyHints, SourceFacts, SourceNormalizationConfig};
use ops_core::ports::SignalNormalizer;
use ops_core::{DomainError, Event, RawSignal, SourceType};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const CONTENT_TYPE: &str = "application/json";
pub const ORIGIN: &str = "grafana";
/// Grafana's "this alert has not ended" sentinel.
const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

// There is deliberately no alert-count cap. The request body limit (1 MiB,
// apps/server) bounds how many alerts one POST can carry, and this budget
// bounds how much they can become once the group is attached to each. A count
// cap on top of those only ever refused real fleet-wide outages whole, which
// is the one notification that must not be lost.
//
// `split_batch` clones the group into every alert's payload. With the rendered
// `message` digest excluded, a real group is labels, annotations, a title and
// a few URLs: a few KB regardless of alert count, so even a thousand-alert
// batch multiplies out to a few MB. 32 MiB is a safety net for bodies that are
// not shaped like Grafana's, not a limit a real notification should ever meet.
// If the server log ever shows "grafana batch rejected" for a real group, the
// model above is wrong and this needs real numbers, not a bigger constant.
const MAX_BATCH_EXPANSION_BYTES: usize = 32 * 1024 * 1024;

/// One alert lifted out of a Grafana notification batch, ready to become a RawSignal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertSignal {
    /// `fingerprint:status:startsAt`. Identical across Grafana's repeat
    /// notifications of one firing alert; distinct for its resolution and for
    /// a later re-fire.
    pub external_id: String,
    /// `{ "alert": <alert object>, "group": <every top-level field except alerts and message> }`.
    pub payload: Value,
}

pub fn split_batch(body: &Value) -> Result<Vec<AlertSignal>, DomainError> {
    let object = body
        .as_object()
        .ok_or_else(|| DomainError::Validation("grafana payload must be a JSON object".into()))?;
    let alerts = match object.get("alerts") {
        Some(Value::Array(alerts)) if !alerts.is_empty() => alerts,
        _ => {
            return Err(DomainError::Validation(
                "grafana payload must carry a non-empty alerts array".into(),
            ))
        }
    };
    // Everything Grafana said about the group travels with each alert, so the
    // stored evidence is complete without the other alerts in the batch. The
    // one exception is `message`: Grafana renders every alert of the batch
    // into it, so it grows with the alert count and is fully derivable from
    // `alerts`. Duplicating it into each alert would make storage quadratic in
    // the group size for no evidence gain.
    let group: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "alerts" | "message"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    // Measure the group once, not per alert: the clone loop below is what
    // actually pays this cost `alerts.len()` times, so the product must be
    // bounded before that loop runs at all.
    let group_size = serde_json::to_string(&group)
        .map(|s| s.len())
        .map_err(|e| {
            DomainError::Validation(format!("grafana payload group could not be measured: {e}"))
        })?;
    let expansion = group_size.saturating_mul(alerts.len());
    if expansion > MAX_BATCH_EXPANSION_BYTES {
        return Err(DomainError::Validation(format!(
            "grafana payload would expand to about {expansion} bytes ({group_size}-byte group \
             cloned across {} alerts), exceeding the expansion budget of {MAX_BATCH_EXPANSION_BYTES} bytes",
            alerts.len()
        )));
    }

    alerts
        .iter()
        .map(|alert| {
            let fingerprint = required_str(alert, "fingerprint")?;
            let status = required_str(alert, "status")?;
            let starts_at = required_str(alert, "startsAt")?;
            Ok(AlertSignal {
                external_id: format!("{fingerprint}:{status}:{starts_at}"),
                payload: serde_json::json!({
                    "alert": alert,
                    "group": Value::Object(group.clone()),
                }),
            })
        })
        .collect()
}

fn required_str<'a>(alert: &'a Value, key: &str) -> Result<&'a str, DomainError> {
    alert
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DomainError::Validation(format!("grafana alert is missing {key}")))
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| DomainError::Validation(format!("grafana time {s:?} is not RFC3339")))
}

fn first_label<'a>(labels: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| labels.get(*key).map(String::as_str))
}

pub fn extract_facts(payload: &Value) -> Result<SourceFacts, DomainError> {
    let alert = payload
        .get("alert")
        .filter(|a| a.is_object())
        .ok_or_else(|| {
            DomainError::Validation("grafana signal payload must carry an alert object".into())
        })?;
    let group = payload.get("group").cloned().unwrap_or(Value::Null);

    let fingerprint = required_str(alert, "fingerprint")?;
    let status = required_str(alert, "status")?;
    let starts_at_raw = required_str(alert, "startsAt")?;
    let starts_at = parse_time(starts_at_raw)?;
    let ends_at = alert
        .get("endsAt")
        .and_then(Value::as_str)
        .filter(|s| *s != ZERO_TIME)
        .map(parse_time)
        .transpose()?;
    // A resolution happened when the alert ended; everything else happened
    // when it started. Never ingest time.
    let occurred_at = match (status, ends_at) {
        ("resolved", Some(ended)) => ended,
        _ => starts_at,
    };

    let labels: BTreeMap<String, String> = alert
        .get("labels")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default();
    let annotation = |key: &str| -> Option<String> {
        alert
            .get("annotations")
            .and_then(|a| a.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };

    let title = annotation("summary")
        .or_else(|| labels.get("alertname").cloned())
        .or_else(|| {
            group
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            DomainError::Validation(
                "grafana alert has neither a summary annotation nor an alertname".into(),
            )
        })?;
    let message = annotation("description");

    // ponytail: which label carries environment/service/resource is a constant
    // until real Grafana data says otherwise; move it to sources.config with
    // the other per-source mappings.
    let environment = first_label(&labels, &["environment", "env"]).map(str::to_owned);
    let service = first_label(&labels, &["service", "job", "app"]).map(str::to_owned);
    let resource =
        first_label(&labels, &["instance", "host", "resource", "pod"]).map(str::to_owned);

    let hints = FamilyHints {
        title: title.clone(),
        message: message.clone(),
        labels: labels.clone(),
        metric_name: labels.get("alertname").cloned(),
        monitor_type: None,
        vendor_category: None,
    };

    Ok(SourceFacts {
        origin: ORIGIN.to_string(),
        occurred_at,
        title,
        message,
        environment,
        service,
        resource,
        severity_raw: labels.get("severity").cloned(),
        state_raw: Some(status.to_string()),
        external_id: Some(format!("{fingerprint}:{status}:{starts_at_raw}")),
        labels,
        hints,
    })
}

pub struct GrafanaNormalizer;

impl SignalNormalizer for GrafanaNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        if source_type != SourceType::Grafana || raw.content_type != CONTENT_TYPE {
            return Err(DomainError::Source(
                "unsupported normalization source/content type".into(),
            ));
        }
        let facts = extract_facts(&raw.payload)?;
        if facts.external_id != raw.external_id {
            return Err(DomainError::Validation(
                "payload external_id differs from RawSignal".into(),
            ));
        }
        let config = SourceNormalizationConfig::for_origin(&facts.origin);
        normalize(
            raw.organization_id,
            raw.source_id,
            raw.id,
            &facts,
            &config,
            created_at,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A batch the size of a large real notification group. Not a limit;
    /// a probe size shared by the expansion and digest tests.
    const FULL_BATCH: usize = 500;

    fn batch() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/grafana-webhook-v1.json")).unwrap()
    }

    #[test]
    fn splits_a_batch_into_one_signal_per_alert_with_stable_ids() {
        let signals = split_batch(&batch()).unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(
            signals[0].external_id,
            "3f2a9c1d8e7b6a50:firing:2026-09-08T09:42:10Z"
        );
        assert_eq!(
            signals[1].external_id,
            "9b8c7d6e5f4a3210:firing:2026-09-08T09:40:00Z"
        );
    }

    #[test]
    fn every_alert_keeps_the_group_context_it_arrived_with() {
        for signal in split_batch(&batch()).unwrap() {
            assert_eq!(signal.payload["group"]["receiver"], "ops-intelligence");
            assert_eq!(
                signal.payload["group"]["groupKey"],
                "{}:{grafana_folder=\"payments\"}"
            );
            assert!(
                signal.payload["group"].get("alerts").is_none(),
                "the alerts array must not be nested inside every alert"
            );
            assert!(signal.payload["alert"].is_object());
            // Load-bearing: the argument that dropping `message` loses nothing
            // relies on Grafana truncating alerts[] BEFORE rendering the digest,
            // and on the truncation count surviving into stored evidence.
            assert_eq!(signal.payload["group"]["truncatedAlerts"], 0);
        }
    }

    #[test]
    fn a_repeat_notification_of_the_same_firing_alert_has_the_same_id() {
        let first = split_batch(&batch()).unwrap();
        let again = split_batch(&batch()).unwrap();
        assert_eq!(first[0].external_id, again[0].external_id);
    }

    #[test]
    fn resolution_and_refire_get_distinct_ids() {
        let mut resolved = batch();
        resolved["alerts"][0]["status"] = json!("resolved");
        resolved["alerts"][0]["endsAt"] = json!("2026-09-08T10:05:00Z");
        let mut refired = batch();
        refired["alerts"][0]["startsAt"] = json!("2026-09-08T11:00:00Z");
        let ids: Vec<String> = [batch(), resolved, refired]
            .iter()
            .map(|b| split_batch(b).unwrap()[0].external_id.clone())
            .collect();
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn facts_come_from_the_alert_not_the_group() {
        let signal = split_batch(&batch()).unwrap().remove(0);
        let facts = extract_facts(&signal.payload).unwrap();
        assert_eq!(facts.origin, ORIGIN);
        assert_eq!(facts.title, "p95 latency above 800ms on payment-api");
        assert_eq!(
            facts.message.as_deref(),
            Some("p95 latency has been above 800ms for 5 minutes")
        );
        assert_eq!(facts.service.as_deref(), Some("payment-api"));
        assert_eq!(facts.resource.as_deref(), Some("api-prod-01"));
        assert_eq!(facts.environment.as_deref(), Some("production"));
        assert_eq!(facts.severity_raw.as_deref(), Some("warning"));
        assert_eq!(facts.state_raw.as_deref(), Some("firing"));
        assert_eq!(facts.occurred_at.to_rfc3339(), "2026-09-08T09:42:10+00:00");
        assert_eq!(
            facts.external_id.as_deref(),
            Some(signal.external_id.as_str())
        );
        assert_eq!(facts.hints.metric_name.as_deref(), Some("HighAPILatency"));
    }

    #[test]
    fn a_resolved_alert_occurs_when_it_ended() {
        let mut body = batch();
        body["alerts"][0]["status"] = json!("resolved");
        body["alerts"][0]["endsAt"] = json!("2026-09-08T10:05:00Z");
        let signal = split_batch(&body).unwrap().remove(0);
        let facts = extract_facts(&signal.payload).unwrap();
        assert_eq!(facts.state_raw.as_deref(), Some("resolved"));
        assert_eq!(facts.occurred_at.to_rfc3339(), "2026-09-08T10:05:00+00:00");
    }

    #[test]
    fn title_falls_back_from_summary_to_alertname() {
        let mut body = batch();
        body["alerts"][1]["annotations"] = json!({});
        let signal = split_batch(&body).unwrap().remove(1);
        assert_eq!(
            extract_facts(&signal.payload).unwrap().title,
            "DiskAlmostFull"
        );
    }

    #[test]
    fn malformed_batches_are_rejected_at_the_door() {
        assert!(split_batch(&json!([])).is_err());
        assert!(split_batch(&json!({ "alerts": [] })).is_err());
        assert!(split_batch(&json!({ "status": "firing" })).is_err());

        let mut no_fingerprint = batch();
        no_fingerprint["alerts"][0]
            .as_object_mut()
            .unwrap()
            .remove("fingerprint");
        assert!(split_batch(&no_fingerprint).is_err());

        let mut bad_time = batch();
        bad_time["alerts"][0]["startsAt"] = json!("yesterday");
        let signal = split_batch(&bad_time).unwrap().remove(0);
        let err = extract_facts(&signal.payload).unwrap_err().to_string();
        assert!(err.contains("RFC3339"), "got: {err}");
    }

    /// A minimal batch with `n` distinct, individually-valid alerts, used to
    /// probe batch-size behaviour.
    fn batch_of(n: usize) -> Value {
        let alerts: Vec<Value> = (0..n)
            .map(|i| {
                json!({
                    "status": "firing",
                    "labels": { "alertname": "Probe" },
                    "startsAt": "2026-09-08T09:42:10Z",
                    "endsAt": "0001-01-01T00:00:00Z",
                    "fingerprint": format!("fp-{i}"),
                })
            })
            .collect();
        json!({ "receiver": "ops-intelligence", "alerts": alerts })
    }

    #[test]
    fn a_fleet_wide_group_is_accepted_whole() {
        // 1,200 alerts is about what a 1 MiB body holds at realistic alert
        // sizes. A fleet-wide outage must arrive whole, not be refused for
        // being large: it is the one notification that must not be lost.
        let signals = split_batch(&batch_of(1_200)).unwrap();
        assert_eq!(signals.len(), 1_200);
        assert_eq!(
            signals[1_199].external_id,
            "fp-1199:firing:2026-09-08T09:42:10Z"
        );
    }

    /// `batch_of(n)` plus one group-level field padded to roughly `group_bytes`,
    /// used to probe the expansion-budget check independently of alert count
    /// (held at `FULL_BATCH` in these tests).
    fn batch_with_padded_group(n: usize, group_bytes: usize) -> Value {
        let mut body = batch_of(n);
        body["padding"] = json!("x".repeat(group_bytes));
        body
    }

    #[test]
    fn a_batch_whose_group_times_alert_count_exceeds_the_expansion_budget_is_rejected() {
        // A full-cap batch (FULL_BATCH alerts) with a group padded past 100 KB
        // multiplies out to ~50 MB, comfortably clearing the 32 MiB budget.
        let huge = batch_with_padded_group(FULL_BATCH, 100_000);
        let err = split_batch(&huge).unwrap_err().to_string();
        assert!(err.contains("expansion budget"), "got: {err}");
    }

    #[test]
    fn a_realistic_batch_stays_within_the_expansion_budget() {
        // A few KB of group context (labels, annotations, URLs) across a
        // full-cap batch of alerts is the shape of a real Grafana
        // notification, and it multiplies out to about 1 MB — well inside
        // the budget.
        let realistic = batch_with_padded_group(FULL_BATCH, 2_000);
        assert!(split_batch(&realistic).is_ok());
    }

    #[test]
    fn the_rendered_digest_is_not_duplicated_into_every_alert() {
        let mut body = batch();
        body["message"] = json!("x".repeat(600 * FULL_BATCH));
        for signal in split_batch(&body).unwrap() {
            assert!(
                signal.payload["group"].get("message").is_none(),
                "message is a rendering of alerts[], not evidence"
            );
            assert_eq!(
                signal.payload["group"]["title"],
                "[FIRING:2]  (payments production)"
            );
        }
    }

    #[test]
    fn a_large_batch_with_a_grafana_sized_digest_is_accepted() {
        // Grafana's default template renders roughly 600 bytes per alert into
        // `message`, so a 500-alert group carries a digest of about 300 KB.
        // Multiplied across 500 alerts that would be 150 MB; it must not trip
        // the budget, because the digest is not stored.
        let mut body = batch_of(FULL_BATCH);
        body["message"] = json!("x".repeat(600 * FULL_BATCH));
        assert!(split_batch(&body).is_ok());
    }
}
