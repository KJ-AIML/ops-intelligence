use chrono::{DateTime, Utc};
use ops_core::ports::SignalNormalizer;
use ops_core::{OrganizationId, RawSignal, SourceId, SourceType};
use ops_source_csv::{read_signals, CsvNormalizer, CONTENT_TYPE};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn pilot_matches_every_normalization_expectation() {
    let rows =
        read_signals(include_bytes!("../../../../pilot-data/synthetic-alerts-v1.csv").as_slice())
            .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../pilot-data/expected-outcomes.json"
    ))
    .unwrap();
    let expected = fixture["normalization_expectations"].as_object().unwrap();
    assert_eq!(rows.len(), 69);
    assert_eq!(expected.len(), 67);
    let org = OrganizationId::new();
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    let mut sources = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut checked = BTreeSet::new();
    for row in rows {
        let source_id = *sources.entry(row.origin).or_insert_with(SourceId::new);
        let row_id = row.payload["row_id"].as_str().unwrap().to_owned();
        let raw = RawSignal::received(
            org,
            source_id,
            row.external_id,
            CONTENT_TYPE,
            row.payload,
            now,
        );
        let key = (
            source_id.to_string(),
            raw.external_id
                .clone()
                .unwrap_or_else(|| raw.payload_hash.clone()),
        );
        if !seen.insert(key) {
            assert!(matches!(row_id.as_str(), "R004" | "R005"));
            continue;
        }
        let event = CsvNormalizer
            .normalize(&raw, SourceType::CsvImport, now)
            .unwrap();
        assert_eq!(
            json!({"event_family": event.event_family.as_str(), "severity": event.severity.as_str(), "state": event.state.as_str()}),
            expected[&row_id],
            "{row_id}"
        );
        assert_eq!(event.raw_signal_id, raw.id);
        assert_eq!(event.organization_id, raw.organization_id);
        assert_eq!(event.source_id, raw.source_id);
        let source_time = DateTime::parse_from_rfc3339(raw.payload["timestamp"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(event.occurred_at, source_time, "{row_id}");
        assert_ne!(event.occurred_at, raw.received_at);
        assert_eq!(event.created_at, now);
        checked.insert(row_id);
    }
    assert_eq!(checked, expected.keys().cloned().collect());
}

#[test]
fn malformed_source_facts_fail_and_never_fall_back_to_receive_time() {
    let good = json!({"timestamp":"2026-09-02T02:40:12+07:00", "source":"grafana", "title":"CPU high", "state":"alerting", "severity":"warning"});
    for (key, bad) in [
        ("timestamp", json!("")),
        ("timestamp", json!("not-a-time")),
        ("timestamp", json!("2026-09-02T02:40:12")),
        ("title", json!("")),
        ("state", json!("wobbling")),
        ("severity", json!("Sev9")),
        ("labels", json!("{broken")),
        ("source", json!(12)),
    ] {
        let mut payload = good.clone();
        payload[key] = bad;
        let raw = RawSignal::received(
            OrganizationId::new(),
            SourceId::new(),
            None,
            CONTENT_TYPE,
            payload,
            Utc::now(),
        );
        assert!(
            CsvNormalizer
                .normalize(&raw, SourceType::CsvImport, Utc::now())
                .is_err(),
            "{key}"
        );
    }
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("timestamp");
    let raw = RawSignal::received(
        OrganizationId::new(),
        SourceId::new(),
        None,
        CONTENT_TYPE,
        missing,
        Utc::now(),
    );
    assert!(CsvNormalizer
        .normalize(&raw, SourceType::CsvImport, Utc::now())
        .is_err());
    let raw = RawSignal::received(
        OrganizationId::new(),
        SourceId::new(),
        None,
        CONTENT_TYPE,
        good,
        Utc::now(),
    );
    assert!(CsvNormalizer
        .normalize(&raw, SourceType::Grafana, Utc::now())
        .is_err());
}
