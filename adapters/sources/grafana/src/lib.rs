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

// ponytail: deliberate bound, not derived from any Grafana limit. Grafana
// groups alerts by folder/label before notifying, so a real notification
// rarely carries more than a few dozen alerts even for a wide incident.
// 500 is comfortably above any plausible real group while still bounding the
// damage of `split_batch` cloning the group into every alert's payload: a
// crafted body with a large group and many tiny alerts can otherwise turn one
// bounded-size POST into hundreds of megabytes of stored payloads. Revisit
// with real numbers once a design partner's Grafana is wired up (tech sheet
// 35).
const MAX_ALERTS_PER_BATCH: usize = 500;

/// One alert lifted out of a Grafana notification batch, ready to become a RawSignal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertSignal {
    /// `fingerprint:status:startsAt`. Identical across Grafana's repeat
    /// notifications of one firing alert; distinct for its resolution and for
    /// a later re-fire.
    pub external_id: String,
    /// `{ "alert": <alert object>, "group": <every top-level field except alerts> }`.
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
    if alerts.len() > MAX_ALERTS_PER_BATCH {
        return Err(DomainError::Validation(format!(
            "grafana payload carries {} alerts, exceeding the cap of {MAX_ALERTS_PER_BATCH}",
            alerts.len()
        )));
    }
    // Everything Grafana said about the group travels with each alert, so the
    // stored evidence is complete without the other alerts in the batch.
    let group: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| key.as_str() != "alerts")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

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

    /// A minimal batch with `n` distinct, individually-valid alerts, used only
    /// to probe the `MAX_ALERTS_PER_BATCH` cap.
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
    fn a_batch_at_the_cap_is_accepted_but_one_over_is_rejected() {
        let at_cap = split_batch(&batch_of(MAX_ALERTS_PER_BATCH));
        assert!(at_cap.is_ok(), "{at_cap:?}");
        assert_eq!(at_cap.unwrap().len(), MAX_ALERTS_PER_BATCH);

        let err = split_batch(&batch_of(MAX_ALERTS_PER_BATCH + 1)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&(MAX_ALERTS_PER_BATCH + 1).to_string()),
            "got: {message}"
        );
        assert!(
            message.contains(&MAX_ALERTS_PER_BATCH.to_string()),
            "got: {message}"
        );
    }
}
