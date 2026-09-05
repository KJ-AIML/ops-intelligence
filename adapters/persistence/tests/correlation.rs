use chrono::{DateTime, Utc};
use ops_core::normalization::process_received;
use ops_core::ports::{
    IncidentRepository, InsertOutcome, OrganizationRepository, RawSignalRepository,
    SourceRepository,
};
use ops_core::{IncidentStatus, RawSignal, SourceType};
use ops_persistence::PgStore;
use ops_source_csv::{CsvNormalizer, CONTENT_TYPE};
use serde_json::Value;
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a dedicated local database ending in _test"]
async fn pilot_fixture_correlation_acceptance() {
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
    let primary = store
        .ensure_by_slug(&format!("correlation-{suffix}"), "Correlation acceptance")
        .await
        .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../../../pilot-data/expected-outcomes.json")).unwrap();
    ingest_fixture(&store, primary.id, &suffix.to_string()).await;
    let normalization =
        process_received(&store, primary.id, &CsvNormalizer, &ops_core::SystemClock)
            .await
            .unwrap();
    assert_eq!((normalization.processed, normalization.failed), (67, 0));

    let mut handled = 0;
    while store.correlate_next(primary.id).await.unwrap() {
        handled += 1;
    }
    assert_eq!(handled, 67);
    assert_eq!(
        store.count_correlation_status(primary.id).await.unwrap(),
        vec![("correlated".into(), 52), ("ignored".into(), 15)]
    );

    let incidents = store.list_incidents(primary.id).await.unwrap();
    assert_eq!(incidents.len(), 22);
    assert_eq!(
        incidents
            .iter()
            .filter(|i| i.status == IncidentStatus::Open)
            .count(),
        8
    );
    assert_eq!(
        incidents
            .iter()
            .filter(|i| i.status == IncidentStatus::Recovered)
            .count(),
        14
    );
    assert_eq!(
        incidents.iter().filter(|i| i.reopened_count == 1).count(),
        1
    );
    assert!(incidents.iter().all(|i| i.reopened_count <= 1));

    let row_for_event: BTreeMap<_, _> = sqlx::query("SELECT e.id, r.payload->>'row_id' AS row_id FROM events e JOIN raw_signals r ON r.id = e.raw_signal_id WHERE e.organization_id = $1")
        .bind(primary.id.as_uuid()).fetch_all(store.pool()).await.unwrap().into_iter()
        .map(|row| (row.get::<uuid::Uuid, _>("id"), row.get::<String, _>("row_id"))).collect();
    let actual_by_incident: BTreeMap<_, Vec<_>> = store
        .list_incident_events(primary.id)
        .await
        .unwrap()
        .into_iter()
        .map(|member| {
            (
                member.incident_id.as_uuid(),
                (
                    row_for_event[&member.event_id.as_uuid()].clone(),
                    member.relation.as_str().to_string(),
                ),
            )
        })
        .fold(BTreeMap::new(), |mut out, (id, member)| {
            out.entry(id).or_insert_with(Vec::new).push(member);
            out
        });
    assert_eq!(actual_by_incident.values().map(Vec::len).sum::<usize>(), 52);
    let linked: BTreeSet<_> = actual_by_incident
        .values()
        .flatten()
        .map(|(row, _)| row.clone())
        .collect();
    assert_eq!(
        linked.len(),
        52,
        "every linked Event has exactly one relation"
    );

    let expected = fixture["incidents"].as_array().unwrap();
    let mut actual_keys = BTreeSet::new();
    let mut row_to_incident = BTreeMap::new();
    for expected_incident in expected {
        let fingerprint = &expected_incident["fingerprint"];
        let started: DateTime<Utc> = expected_incident["started_at"]
            .as_str()
            .unwrap()
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc);
        let incident = incidents
            .iter()
            .find(|incident| {
                incident.started_at == started
                    && incident.fingerprint.environment.as_deref()
                        == fingerprint["environment"].as_str()
                    && incident.fingerprint.service.as_deref() == fingerprint["service"].as_str()
                    && incident.fingerprint.resource.as_deref() == fingerprint["resource"].as_str()
                    && incident.fingerprint.event_family.as_str()
                        == fingerprint["event_family"].as_str().unwrap()
            })
            .unwrap_or_else(|| panic!("missing expected {}", expected_incident["key"]));
        actual_keys.insert(incident.id.as_uuid());
        assert_eq!(
            incident.status.as_str(),
            expected_incident["expected_status"].as_str().unwrap()
        );
        assert_eq!(
            incident.severity.as_str(),
            expected_incident["expected_severity"].as_str().unwrap()
        );
        assert_eq!(
            incident.reopened_count > 0,
            expected_incident["reopened"].as_bool().unwrap()
        );
        assert_eq!(
            incident.last_event_at,
            expected_incident["last_event_at"]
                .as_str()
                .unwrap()
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(
            incident.recovered_at,
            expected_incident["recovered_at"].as_str().map(|s| s
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc))
        );
        let mut actual_members = actual_by_incident[&incident.id.as_uuid()].clone();
        actual_members.sort();
        let mut expected_members: Vec<_> = expected_incident["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["row_id"].as_str().unwrap().to_string(),
                    m["relation"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        expected_members.sort();
        assert_eq!(
            actual_members, expected_members,
            "{}",
            expected_incident["key"]
        );
        for (row, _) in actual_members {
            row_to_incident.insert(row, incident.id.as_uuid());
        }
    }
    assert_eq!(actual_keys.len(), 22);
    let relation_counts = store
        .list_incident_events(primary.id)
        .await
        .unwrap()
        .into_iter()
        .fold(BTreeMap::<String, usize>::new(), |mut counts, m| {
            *counts.entry(m.relation.as_str().into()).or_default() += 1;
            counts
        });
    assert_eq!(relation_counts["duplicate"], 10);
    assert_eq!(relation_counts["recovery"], 16);
    assert_eq!(relation_counts["update"], 3);
    assert_eq!(relation_counts["trigger"], 23);
    for pair in fixture["must_not_correlate"].as_array().unwrap() {
        assert_ne!(
            row_to_incident[pair["a"].as_str().unwrap()],
            row_to_incident[pair["b"].as_str().unwrap()],
            "{}",
            pair["reason"]
        );
    }
    // R054/R055/R056 is the severity escalation case: one incident, critical final severity.
    assert_eq!(row_to_incident["R054"], row_to_incident["R056"]);
    assert_eq!(
        incidents
            .iter()
            .find(|i| i.id.as_uuid() == row_to_incident["R054"])
            .unwrap()
            .severity
            .as_str(),
        "critical"
    );
    assert_eq!(
        incidents
            .iter()
            .filter(|i| i.fingerprint.service.as_deref() == Some("payment-api")
                && i.fingerprint.environment.as_deref() == Some("production")
                && i.fingerprint.event_family.as_str() == "latency")
            .count(),
        4,
        "recurrence records remain separate incidents"
    );

    assert!(
        !store.correlate_next(primary.id).await.unwrap(),
        "rerun is idempotent"
    );
    assert_eq!(store.list_incidents(primary.id).await.unwrap(), incidents);
    assert_eq!(
        store.list_incident_events(primary.id).await.unwrap().len(),
        52
    );

    let secondary = store
        .ensure_by_slug(&format!("correlation-tenant-{suffix}"), "Tenant isolation")
        .await
        .unwrap();
    ingest_one(&store, secondary.id, "tenant-source", "tenant-row").await;
    assert_eq!(
        process_received(&store, secondary.id, &CsvNormalizer, &ops_core::SystemClock)
            .await
            .unwrap()
            .processed,
        1
    );
    assert!(!store.correlate_next(primary.id).await.unwrap());
    assert_eq!(
        store.count_correlation_status(secondary.id).await.unwrap(),
        vec![("received".into(), 1)]
    );
    assert!(store.list_incidents(secondary.id).await.unwrap().is_empty());
}

async fn ingest_fixture(store: &PgStore, organization_id: ops_core::OrganizationId, prefix: &str) {
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
async fn ingest_one(
    store: &PgStore,
    organization_id: ops_core::OrganizationId,
    source_name: &str,
    external_id: &str,
) {
    let source = store
        .ensure(organization_id, SourceType::CsvImport, source_name)
        .await
        .unwrap();
    let payload = serde_json::json!({"row_id":"TENANT","timestamp":"2026-09-02T01:00:00+07:00","source":"grafana","external_id":external_id,"title":"tenant latency","severity":"warning","state":"alerting","environment":"production","service":"tenant","resource":"one"});
    let raw = RawSignal::received(
        organization_id,
        source.id,
        Some(external_id.into()),
        CONTENT_TYPE,
        payload,
        Utc::now(),
    );
    assert!(matches!(
        store.insert_if_new(&raw).await.unwrap(),
        InsertOutcome::Inserted(_)
    ));
}
