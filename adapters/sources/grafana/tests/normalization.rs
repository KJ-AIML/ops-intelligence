use chrono::{DateTime, Utc};
use ops_core::domains::events::{EventFamily, EventState, Severity};
use ops_core::ports::SignalNormalizer;
use ops_core::{OrganizationId, RawSignal, SourceId, SourceType};
use ops_source_grafana::{split_batch, GrafanaNormalizer, CONTENT_TYPE};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/grafana-webhook-v1.json")).unwrap()
}

#[test]
fn a_native_grafana_notification_becomes_canonical_events() {
    let org = OrganizationId::new();
    let source = SourceId::new();
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();

    let raws: Vec<RawSignal> = split_batch(&fixture())
        .unwrap()
        .into_iter()
        .map(|s| {
            RawSignal::received(
                org,
                source,
                Some(s.external_id),
                CONTENT_TYPE,
                s.payload,
                now,
            )
        })
        .collect();

    let latency = GrafanaNormalizer
        .normalize(&raws[0], SourceType::Grafana, now)
        .unwrap();
    assert_eq!(latency.event_family, EventFamily::Latency);
    assert_eq!(latency.severity, Severity::Warning);
    assert_eq!(latency.state, EventState::Firing);
    assert_eq!(latency.service.as_deref(), Some("payment-api"));
    assert_eq!(latency.resource.as_deref(), Some("api-prod-01"));
    assert_eq!(latency.environment.as_deref(), Some("production"));
    assert_eq!(
        latency.occurred_at.to_rfc3339(),
        "2026-09-08T09:42:10+00:00"
    );
    assert_ne!(
        latency.occurred_at, raws[0].received_at,
        "never ingest time"
    );
    assert_eq!(latency.raw_signal_id, raws[0].id);
    assert_eq!(latency.organization_id, org);
    assert_eq!(latency.created_at, now);

    let disk = GrafanaNormalizer
        .normalize(&raws[1], SourceType::Grafana, now)
        .unwrap();
    assert_eq!(disk.severity, Severity::Critical);
    assert_eq!(disk.state, EventState::Firing);
    assert_eq!(disk.resource.as_deref(), Some("db-prod-02"));
    assert!(
        disk.service.is_none(),
        "no service label means no service, not a guess"
    );
}

#[test]
fn the_wrong_source_type_is_refused() {
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    let signal = split_batch(&fixture()).unwrap().remove(0);
    let raw = RawSignal::received(
        OrganizationId::new(),
        SourceId::new(),
        Some(signal.external_id),
        CONTENT_TYPE,
        signal.payload,
        now,
    );
    assert!(GrafanaNormalizer
        .normalize(&raw, SourceType::GenericWebhook, now)
        .is_err());
}
