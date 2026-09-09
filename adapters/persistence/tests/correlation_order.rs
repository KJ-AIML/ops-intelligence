//! Replay determinism must not depend on luck. The correlator draws events in
//! `occurred_at` order and breaks ties on the event id, which is a fresh random
//! UUID on every run; two events that share an exact timestamp under one
//! fingerprint then correlate in random order, and a firing/recovery pair at
//! the same instant can come out open or recovered depending on the draw.
//! `pilot compare` would report deltas with nothing changed.

use chrono::Utc;
use ops_core::normalization::process_received;
use ops_core::ports::{
    IncidentRepository, OrganizationRepository, RawSignalRepository, SourceRepository,
};
use ops_core::{IncidentStatus, RawSignal, SourceType};
use ops_persistence::PgStore;
use ops_source_csv::{CsvNormalizer, CONTENT_TYPE};

const ATTEMPTS: usize = 6;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a dedicated local database ending in _test"]
async fn same_instant_events_correlate_the_same_way_every_time() {
    let url =
        std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL to dedicated _test DB");
    let store = PgStore::new(ops_persistence::connect(&url).await.unwrap());
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(database.ends_with("_test"), "refuse a non-test database");
    ops_persistence::run_migrations(store.pool()).await.unwrap();

    let mut outcomes = Vec::new();
    for attempt in 0..ATTEMPTS {
        let suffix = uuid::Uuid::new_v4();
        let org = store
            .ensure_by_slug(&format!("order-{suffix}-{attempt}"), "Order acceptance")
            .await
            .unwrap();
        let source = store
            .ensure(org.id, SourceType::CsvImport, "csv:order")
            .await
            .unwrap();

        // Same instant, same fingerprint. The external ids are chosen so that
        // the recovery sorts FIRST once the tie-break is content-derived.
        for (external_id, state, severity) in [
            ("a-recovery", "ok", "info"),
            ("b-firing", "alerting", "warning"),
        ] {
            let payload = serde_json::json!({
                "timestamp": "2026-09-02T01:00:00+07:00",
                "source": "grafana",
                "external_id": external_id,
                "title": "api latency",
                "severity": severity,
                "state": state,
                "environment": "production",
                "service": "payment-api",
                "resource": "api-prod-01"
            });
            let raw = RawSignal::received(
                org.id,
                source.id,
                Some(external_id.into()),
                CONTENT_TYPE,
                payload,
                Utc::now(),
            );
            store.insert_if_new(&raw).await.unwrap();
        }

        process_received(&store, org.id, &CsvNormalizer, &ops_core::SystemClock)
            .await
            .unwrap();
        while store.correlate_next(org.id).await.unwrap() {}

        let incidents = store.list_incidents(org.id).await.unwrap();
        assert_eq!(incidents.len(), 1, "attempt {attempt}");
        outcomes.push(incidents[0].status);
    }

    assert!(
        outcomes.iter().all(|s| *s == outcomes[0]),
        "correlation order was not deterministic: {outcomes:?}"
    );
    // With the recovery drawn first it is an orphan and is ignored; the firing
    // then opens the incident. If this assertion fails while the six attempts
    // agree, the engine's same-instant semantics differ from README "Running
    // it"; report that rather than change the expected status.
    assert_eq!(outcomes[0], IncidentStatus::Open);
}
