use chrono::{DateTime, Utc};
use ops_core::normalization::process_received;
use ops_core::ports::{
    Clock, EventRepository, InsertOutcome, OrganizationRepository, RawSignalRepository,
    SignalNormalizer, SourceRepository,
};
use ops_core::{DomainError, Event, OrganizationId, ProcessingStatus, RawSignal, SourceType};
use ops_persistence::PgStore;
use ops_source_csv::{CsvNormalizer, CONTENT_TYPE};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};

struct FixedClock(DateTime<Utc>);
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

// Explicit opt-in prevents a normal unit run from modifying a developer database.
// Test creates isolated tenants and retains them for inspection; no deletion.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a dedicated local database ending in _test"]
async fn postgres_normalization_acceptance() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to the dedicated local test DB");
    let store = PgStore::new(ops_persistence::connect(&url).await.unwrap());
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(
        database.ends_with("_test"),
        "integration test requires dedicated _test database"
    );
    ops_persistence::run_migrations(store.pool()).await.unwrap();
    let suffix = uuid::Uuid::new_v4();
    let a = store
        .ensure_by_slug(&format!("normalization-a-{suffix}"), "Acceptance A")
        .await
        .unwrap();
    let b = store
        .ensure_by_slug(&format!("normalization-b-{suffix}"), "Acceptance B")
        .await
        .unwrap();
    let clock = FixedClock("2030-01-01T00:00:00Z".parse().unwrap());
    let rows = ops_source_csv::read_signals(
        include_bytes!("../../../pilot-data/synthetic-alerts-v1.csv").as_slice(),
    )
    .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../../../pilot-data/expected-outcomes.json")).unwrap();
    let mut sources = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    let mut duplicates = 0;
    for row in &rows {
        if !sources.contains_key(&row.origin) {
            let source = store
                .ensure(a.id, SourceType::CsvImport, &format!("csv:{}", row.origin))
                .await
                .unwrap();
            sources.insert(row.origin.clone(), source.id);
        }
        let raw = RawSignal::received(
            a.id,
            sources[&row.origin],
            row.external_id.clone(),
            CONTENT_TYPE,
            row.payload.clone(),
            clock.now(),
        );
        match store.insert_if_new(&raw).await.unwrap() {
            InsertOutcome::Inserted(id) => {
                evidence.insert(id.to_string(), raw);
            }
            InsertOutcome::Duplicate => duplicates += 1,
        }
    }
    assert_eq!((rows.len(), evidence.len(), duplicates), (69, 67, 2));
    let b_source = store
        .ensure(b.id, SourceType::CsvImport, "csv:grafana")
        .await
        .unwrap();
    let b_raw = RawSignal::received(
        b.id,
        b_source.id,
        rows[0].external_id.clone(),
        CONTENT_TYPE,
        rows[0].payload.clone(),
        clock.now(),
    );
    assert!(matches!(
        store.insert_if_new(&b_raw).await.unwrap(),
        InsertOutcome::Inserted(_)
    ));
    // Two workers compete for the same tenant; row locks keep each input singular.
    let (one, two) = tokio::join!(
        process_received(&store, a.id, &CsvNormalizer, &clock),
        process_received(&store, a.id, &CsvNormalizer, &clock)
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.processed + two.processed, 67);
    assert_eq!(one.failed + two.failed, 0);
    assert_eq!(
        store.count_by_status(a.id).await.unwrap(),
        vec![("processed".into(), 67)]
    );
    assert_eq!(
        store.count_by_status(b.id).await.unwrap(),
        vec![("received".into(), 1)]
    );
    assert!(store.list(b.id).await.unwrap().is_empty());
    let events = store.list(a.id).await.unwrap();
    assert_eq!(events.len(), 67);
    let mut linked = BTreeSet::new();
    for event in &events {
        let raw = &evidence[&event.raw_signal_id.to_string()];
        assert!(linked.insert(event.raw_signal_id.to_string()));
        let row_id = raw.payload["row_id"].as_str().unwrap();
        assert_eq!(
            json!({"event_family":event.event_family.as_str(),"severity":event.severity.as_str(),"state":event.state.as_str()}),
            fixture["normalization_expectations"][row_id],
            "{row_id}"
        );
        assert_eq!(event.organization_id, a.id);
        assert_eq!(event.source_id, raw.source_id);
        assert_eq!(event.external_id, raw.external_id);
        assert_eq!(
            event.occurred_at,
            DateTime::parse_from_rfc3339(raw.payload["timestamp"].as_str().unwrap())
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_ne!(event.occurred_at, raw.received_at);
        assert_eq!(event.created_at, clock.now());
    }
    assert_eq!(linked, evidence.keys().cloned().collect());
    assert_eq!(
        process_received(&store, a.id, &CsvNormalizer, &clock)
            .await
            .unwrap()
            .processed,
        0
    );
    assert_eq!(store.list(a.id).await.unwrap(), events);
    // Re-import is also a no-op, and the other tenant can use the same external ID.
    for row in &rows {
        let raw = RawSignal::received(
            a.id,
            sources[&row.origin],
            row.external_id.clone(),
            CONTENT_TYPE,
            row.payload.clone(),
            clock.now(),
        );
        assert_eq!(
            store.insert_if_new(&raw).await.unwrap(),
            InsertOutcome::Duplicate
        );
    }
    assert_eq!(
        process_received(&store, b.id, &CsvNormalizer, &clock)
            .await
            .unwrap()
            .processed,
        1
    );
    assert_eq!(store.list(b.id).await.unwrap().len(), 1);

    // A forged tenant/source pairing is rejected even at ingestion.
    let forged = RawSignal::received(
        a.id,
        b_source.id,
        Some("forged".into()),
        CONTENT_TYPE,
        json!({}),
        clock.now(),
    );
    assert!(store.insert_if_new(&forged).await.is_err());
    // Event SQL cannot rewrite either tenant or source away from its evidence.
    assert!(
        sqlx::query("UPDATE events SET organization_id = $1 WHERE id = $2")
            .bind(b.id.as_uuid())
            .bind(events[0].id.as_uuid())
            .execute(store.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE events SET source_id = $1 WHERE id = $2")
            .bind(b_source.id.as_uuid())
            .bind(events[0].id.as_uuid())
            .execute(store.pool())
            .await
            .is_err()
    );

    // Invalid payloads and unsupported sources persist failures, and draining continues.
    let failures = store
        .ensure_by_slug(&format!("failures-{suffix}"), "Failure acceptance")
        .await
        .unwrap();
    let source = store
        .ensure(failures.id, SourceType::CsvImport, "csv")
        .await
        .unwrap();
    for (index, (key, value)) in [
        ("timestamp", "invalid"),
        ("timestamp", ""),
        ("severity", "Sev9"),
        ("state", "unknown"),
        ("title", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let mut payload = rows[0].payload.clone();
        let external_id = format!("bad-{index}");
        payload["external_id"] = json!(external_id);
        payload[key] = json!(value);
        let raw = RawSignal::received(
            failures.id,
            source.id,
            Some(external_id),
            CONTENT_TYPE,
            payload,
            clock.now(),
        );
        store.insert_if_new(&raw).await.unwrap();
    }
    let live = store
        .ensure(failures.id, SourceType::Grafana, "unsupported-live")
        .await
        .unwrap();
    let unsupported = RawSignal::received(
        failures.id,
        live.id,
        rows[0].external_id.clone(),
        CONTENT_TYPE,
        rows[0].payload.clone(),
        clock.now(),
    );
    store.insert_if_new(&unsupported).await.unwrap();
    let valid = RawSignal::received(
        failures.id,
        source.id,
        rows[0].external_id.clone(),
        CONTENT_TYPE,
        rows[0].payload.clone(),
        clock.now(),
    );
    store.insert_if_new(&valid).await.unwrap();
    let report = process_received(&store, failures.id, &CsvNormalizer, &clock)
        .await
        .unwrap();
    assert_eq!((report.processed, report.failed), (1, 6));
    let failed_rows = sqlx::query("SELECT r.processing_error, e.id AS event_id FROM raw_signals r LEFT JOIN events e ON e.raw_signal_id = r.id WHERE r.organization_id = $1 AND r.processing_status = 'failed'").bind(failures.id.as_uuid()).fetch_all(store.pool()).await.unwrap();
    assert_eq!(failed_rows.len(), 6);
    for row in failed_rows {
        assert!(!row.get::<String, _>("processing_error").is_empty());
        assert!(row.get::<Option<uuid::Uuid>, _>("event_id").is_none());
    }
    assert_eq!(
        store.count_by_status(failures.id).await.unwrap(),
        vec![("failed".into(), 6), ("processed".into(), 1)]
    );
    // The failed count must be visible where the operator looks, not only in SQL.
    let summary =
        ops_core::ports::ProductQueries::operations_summary(&store, failures.id, None, None)
            .await
            .unwrap();
    assert_eq!(summary.failed_signals, 6);
    assert_eq!(
        process_received(&store, failures.id, &CsvNormalizer, &clock)
            .await
            .unwrap()
            .failed,
        0
    );

    // A DB write failure rolls the claim back; it must not be counted as normalized.
    let rollback = store
        .ensure_by_slug(&format!("rollback-{suffix}"), "Rollback acceptance")
        .await
        .unwrap();
    let source = store
        .ensure(rollback.id, SourceType::CsvImport, "csv")
        .await
        .unwrap();
    let raw = RawSignal::received(
        rollback.id,
        source.id,
        rows[0].external_id.clone(),
        CONTENT_TYPE,
        rows[0].payload.clone(),
        clock.now(),
    );
    store.insert_if_new(&raw).await.unwrap();
    assert!(
        process_received(&store, rollback.id, &InvalidDatabaseEvent, &clock)
            .await
            .is_err()
    );
    assert_eq!(
        store.count_by_status(rollback.id).await.unwrap(),
        vec![("received".into(), 1)]
    );
    assert!(store.list(rollback.id).await.unwrap().is_empty());
    assert_eq!(
        process_received(&store, rollback.id, &CsvNormalizer, &clock)
            .await
            .unwrap()
            .processed,
        1
    );

    // The application adapter rejects a normalizer that tries to change evidence ownership.
    let mismatch = RawSignal::received(
        rollback.id,
        source.id,
        None,
        CONTENT_TYPE,
        json!({}),
        clock.now(),
    );
    store.insert_if_new(&mismatch).await.unwrap();
    assert_eq!(
        store
            .process_next(rollback.id, &WrongTenant(b.id), &clock)
            .await
            .unwrap(),
        Some(ProcessingStatus::Failed)
    );
    assert_eq!(store.list(rollback.id).await.unwrap().len(), 1);
}

struct InvalidDatabaseEvent;
impl SignalNormalizer for InvalidDatabaseEvent {
    fn normalize(
        &self,
        raw: &RawSignal,
        source: SourceType,
        at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        let mut event = CsvNormalizer.normalize(raw, source, at)?;
        event.title.clear(); // DB CHECK violation, exercising transaction rollback.
        Ok(event)
    }
}
struct WrongTenant(OrganizationId);
impl SignalNormalizer for WrongTenant {
    fn normalize(
        &self,
        raw: &RawSignal,
        _: SourceType,
        at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        Ok(Event {
            id: ops_core::EventId::new(),
            organization_id: self.0,
            source_id: raw.source_id,
            raw_signal_id: raw.id,
            occurred_at: at,
            environment: None,
            service: None,
            resource: None,
            event_family: ops_core::EventFamily::Unclassified,
            severity: ops_core::Severity::Info,
            state: ops_core::EventState::Informational,
            title: "mismatch".into(),
            message: None,
            labels: BTreeMap::new(),
            external_id: raw.external_id.clone(),
            created_at: at,
        })
    }
}
