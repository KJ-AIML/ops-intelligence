//! Pilot Lab acceptance: a replay must be repeatable, and it must never touch
//! the captured evidence.
//!
//! Both properties are what make the Lab usable as a regression baseline. If a
//! replay drifted between runs, comparing two engine versions would be
//! measuring noise. If it wrote into the capture tenant, the dataset would stop
//! being a record of what actually arrived.

use chrono::Utc;
use ops_core::normalization::process_received;
use ops_core::ports::{
    IncidentRepository, OrganizationRepository, PilotRepository, RawSignalRepository,
    SourceRepository,
};
use ops_core::{OrganizationId, RawSignal, RunStats, SourceType};
use ops_persistence::PgStore;
use ops_source_csv::{CsvNormalizer, CONTENT_TYPE};
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a dedicated local database ending in _test"]
async fn replay_is_repeatable_and_never_touches_the_capture_tenant() {
    let url =
        std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL to dedicated _test DB");
    let store = PgStore::new(ops_persistence::connect(&url).await.unwrap());
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(database.ends_with("_test"), "refuse a non-test database");
    ops_persistence::run_migrations(store.pool()).await.unwrap();

    let suffix = uuid::Uuid::new_v4();
    let capture = store
        .ensure_by_slug(&format!("capture-{suffix}"), "Replay acceptance")
        .await
        .unwrap();

    // CAPTURE: signals arrive and are stored, nothing else.
    ingest_fixture(&store, capture.id, &suffix.to_string()).await;

    // FREEZE: membership is by reference, so the dataset cannot drift from what
    // was actually received.
    let dataset = store
        .create_dataset(
            capture.id,
            &format!("acceptance-{suffix}"),
            None,
            None,
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(dataset.signal_count, 67);
    assert!(dataset.is_frozen());

    let first = replay(&store, dataset.id, &format!("run-a-{suffix}")).await;

    // The whole point of a per-run tenant: ingestion is idempotent on
    // (source_id, external_id), so a second replay into the SAME tenant would
    // insert nothing and silently report an empty result.
    let second = replay(&store, dataset.id, &format!("run-b-{suffix}")).await;

    assert_eq!(
        first, second,
        "two replays of one frozen dataset must produce identical statistics"
    );
    assert!(
        ops_core::compare(&first, &second).is_empty(),
        "compare() must report no deltas between identical runs"
    );

    // Sanity-check against the verified fixture result, so a regression in the
    // engine cannot pass merely by being consistently wrong twice.
    assert_eq!(first.signals_replayed, 67);
    assert_eq!(first.events, 67);
    assert_eq!(first.incidents, 22);
    assert_eq!((first.open, first.recovered, first.critical), (8, 14, 6));
    assert_eq!(first.duplicate_relations, 10);
    assert_eq!(first.recovery_relations, 16);
    assert_eq!(first.recurrence_patterns, 2);
    assert_eq!(first.events_without_incident, 15);

    // The capture tenant holds evidence and is never processed. Replays are
    // disposable; captures are not.
    let untouched = store.collect_stats(capture.id).await.unwrap();
    assert_eq!(untouched.signals_replayed, 67, "captured signals remain");
    assert_eq!(
        untouched.events, 0,
        "capture tenant must never be normalized"
    );
    assert_eq!(
        untouched.incidents, 0,
        "capture tenant must never be correlated"
    );

    // Both runs are recorded against the dataset and flagged as replay tenants.
    let runs = store.list_runs(dataset.id).await.unwrap();
    assert_eq!(runs.len(), 2);
    for run in &runs {
        let is_replay: bool =
            sqlx::query_scalar("SELECT is_replay FROM organizations WHERE id = $1")
                .bind(run.target_organization_id.as_uuid())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(
            is_replay,
            "a replay tenant must be flagged, never mistaken for the real one"
        );
        assert_ne!(run.target_organization_id, capture.id);
    }
}

/// One full replay: copy the frozen signals into the run's own tenant, then run
/// the real normalizer and correlator over them.
async fn replay(store: &PgStore, dataset_id: ops_core::DatasetId, label: &str) -> RunStats {
    let run = store
        .start_run(dataset_id, label, "test", None, Utc::now())
        .await
        .unwrap();

    let (replayed, suppressed) = store
        .replay_signals_into_run(&run, Utc::now())
        .await
        .unwrap();

    let normalization = process_received(
        store,
        run.target_organization_id,
        &CsvNormalizer,
        &ops_core::SystemClock,
    )
    .await
    .unwrap();

    while IncidentRepository::correlate_next(store, run.target_organization_id)
        .await
        .unwrap()
    {}

    let mut stats = store
        .collect_stats(run.target_organization_id)
        .await
        .unwrap();
    stats.signals_replayed = replayed;
    stats.signals_suppressed = suppressed;
    stats.events_failed = normalization.failed as i64;

    store
        .finish_run(run.id, &stats, None, Utc::now())
        .await
        .unwrap();
    stats
}

async fn ingest_fixture(store: &PgStore, organization_id: OrganizationId, prefix: &str) {
    let rows = ops_source_csv::read_signals(
        include_bytes!("../../../pilot-data/synthetic-alerts-v1.csv").as_slice(),
    )
    .unwrap();
    let mut sources = BTreeMap::new();
    for row in rows {
        let source = if let Some(source) = sources.get(&row.origin) {
            *source
        } else {
            let source = store
                .ensure(
                    organization_id,
                    SourceType::CsvImport,
                    &format!("{prefix}:{}", row.origin),
                )
                .await
                .unwrap();
            sources.insert(row.origin.clone(), source.id);
            source.id
        };
        let raw = RawSignal::received(
            organization_id,
            source,
            row.external_id,
            CONTENT_TYPE,
            row.payload,
            Utc::now(),
        );
        let _ = store.insert_if_new(&raw).await.unwrap();
    }
}
