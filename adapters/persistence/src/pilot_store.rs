//! Pilot Lab persistence: datasets, replay runs, and run statistics.

use crate::{persistence, PgStore};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ops_core::domains::pilot::{PilotDataset, PilotRun, RunStats, RunStatus};
use ops_core::error::DomainError;
use ops_core::ids::{DatasetId, OrganizationId, RunId};
use ops_core::ports::PilotRepository;
use sqlx::Row;
use std::str::FromStr;

const DATASET_SELECT: &str = r#"
    SELECT d.id, d.organization_id, d.name, d.description, d.frozen_at, d.created_at,
           COALESCE(c.signal_count, 0) AS signal_count
      FROM pilot_datasets d
      LEFT JOIN LATERAL (
          SELECT COUNT(*) AS signal_count FROM pilot_dataset_signals s
           WHERE s.dataset_id = d.id
      ) c ON TRUE
"#;

const RUN_SELECT: &str = "SELECT id, dataset_id, target_organization_id, label, engine_version, \
     ai_enabled, ai_provider, ai_model, status, stats, error, started_at, finished_at \
     FROM pilot_runs";

fn dataset_from_row(row: &sqlx::postgres::PgRow) -> Result<PilotDataset, DomainError> {
    Ok(PilotDataset {
        id: DatasetId::from_uuid(row.try_get("id").map_err(persistence)?),
        organization_id: OrganizationId::from_uuid(
            row.try_get("organization_id").map_err(persistence)?,
        ),
        name: row.try_get("name").map_err(persistence)?,
        description: row.try_get("description").map_err(persistence)?,
        frozen_at: row.try_get("frozen_at").map_err(persistence)?,
        signal_count: row.try_get("signal_count").map_err(persistence)?,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}

fn stats_to_json(s: &RunStats) -> serde_json::Value {
    serde_json::json!({
        "signals_replayed": s.signals_replayed,
        "signals_suppressed": s.signals_suppressed,
        "events": s.events,
        "events_failed": s.events_failed,
        "events_without_incident": s.events_without_incident,
        "incidents": s.incidents,
        "open": s.open,
        "recovered": s.recovered,
        "critical": s.critical,
        "duplicate_relations": s.duplicate_relations,
        "recovery_relations": s.recovery_relations,
        "recurrence_patterns": s.recurrence_patterns,
        "insights_ok": s.insights_ok,
        "insights_failed": s.insights_failed,
    })
}

fn stats_from_json(v: &serde_json::Value) -> RunStats {
    let n = |k: &str| v.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
    RunStats {
        signals_replayed: n("signals_replayed"),
        signals_suppressed: n("signals_suppressed"),
        events: n("events"),
        events_failed: n("events_failed"),
        events_without_incident: n("events_without_incident"),
        incidents: n("incidents"),
        open: n("open"),
        recovered: n("recovered"),
        critical: n("critical"),
        duplicate_relations: n("duplicate_relations"),
        recovery_relations: n("recovery_relations"),
        recurrence_patterns: n("recurrence_patterns"),
        insights_ok: n("insights_ok"),
        insights_failed: n("insights_failed"),
    }
}

fn run_from_row(row: &sqlx::postgres::PgRow) -> Result<PilotRun, DomainError> {
    let status: String = row.try_get("status").map_err(persistence)?;
    let stats: serde_json::Value = row.try_get("stats").map_err(persistence)?;
    Ok(PilotRun {
        id: RunId::from_uuid(row.try_get("id").map_err(persistence)?),
        dataset_id: DatasetId::from_uuid(row.try_get("dataset_id").map_err(persistence)?),
        target_organization_id: OrganizationId::from_uuid(
            row.try_get("target_organization_id").map_err(persistence)?,
        ),
        label: row.try_get("label").map_err(persistence)?,
        engine_version: row.try_get("engine_version").map_err(persistence)?,
        ai_enabled: row.try_get("ai_enabled").map_err(persistence)?,
        ai_provider: row.try_get("ai_provider").map_err(persistence)?,
        ai_model: row.try_get("ai_model").map_err(persistence)?,
        status: RunStatus::from_str(&status)?,
        stats: stats_from_json(&stats),
        error: row.try_get("error").map_err(persistence)?,
        started_at: row.try_get("started_at").map_err(persistence)?,
        finished_at: row.try_get("finished_at").map_err(persistence)?,
    })
}

#[async_trait]
impl PilotRepository for PgStore {
    async fn create_dataset(
        &self,
        organization_id: OrganizationId,
        name: &str,
        description: Option<&str>,
        source_name: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<PilotDataset, DomainError> {
        let mut tx = self.pool().begin().await.map_err(persistence)?;
        let dataset_id = DatasetId::new();

        sqlx::query(
            "INSERT INTO pilot_datasets (id, organization_id, name, description, frozen_at, created_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(dataset_id.as_uuid())
        .bind(organization_id.as_uuid())
        .bind(name)
        .bind(description)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DomainError::Validation(format!("a dataset named {name:?} already exists"))
            }
            _ => persistence(e),
        })?;

        // Membership by reference, ordered by capture time so a replay
        // reproduces arrival sequence and not just content.
        let inserted = sqlx::query(
            r#"
            INSERT INTO pilot_dataset_signals (dataset_id, raw_signal_id, ordinal)
            SELECT $1, r.id, ROW_NUMBER() OVER (ORDER BY r.received_at, r.id)
              FROM raw_signals r
              JOIN sources s ON s.id = r.source_id
             WHERE r.organization_id = $2
               AND ($3::text IS NULL OR s.name = $3)
            "#,
        )
        .bind(dataset_id.as_uuid())
        .bind(organization_id.as_uuid())
        .bind(source_name)
        .execute(&mut *tx)
        .await
        .map_err(persistence)?
        .rows_affected();

        if inserted == 0 {
            // An empty dataset is a mistake every time, and it is much cheaper
            // to refuse it here than to debug an empty replay later.
            tx.rollback().await.map_err(persistence)?;
            return Err(DomainError::Validation(
                "no captured signals matched; dataset not created".into(),
            ));
        }

        tx.commit().await.map_err(persistence)?;

        self.find_dataset(organization_id, name)
            .await?
            .ok_or_else(|| DomainError::Persistence("dataset vanished after creation".into()))
    }

    async fn list_datasets(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<PilotDataset>, DomainError> {
        let rows = sqlx::query(&format!(
            "{DATASET_SELECT} WHERE d.organization_id = $1 ORDER BY d.created_at DESC"
        ))
        .bind(organization_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;
        rows.iter().map(dataset_from_row).collect()
    }

    async fn find_dataset(
        &self,
        organization_id: OrganizationId,
        name: &str,
    ) -> Result<Option<PilotDataset>, DomainError> {
        let row = sqlx::query(&format!(
            "{DATASET_SELECT} WHERE d.organization_id = $1 AND d.name = $2"
        ))
        .bind(organization_id.as_uuid())
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(persistence)?;
        row.as_ref().map(dataset_from_row).transpose()
    }

    async fn start_run(
        &self,
        dataset_id: DatasetId,
        label: &str,
        engine_version: &str,
        ai: Option<(&str, &str)>,
        at: DateTime<Utc>,
    ) -> Result<PilotRun, DomainError> {
        let mut tx = self.pool().begin().await.map_err(persistence)?;
        let run_id = RunId::new();
        let target = OrganizationId::new();

        // The throwaway tenant. Flagged is_replay so it can never be mistaken
        // for the real organization in a query, a dashboard or a bill.
        sqlx::query(
            "INSERT INTO organizations (id, name, slug, is_replay)
             VALUES ($1, $2, $3, TRUE)",
        )
        .bind(target.as_uuid())
        .bind(format!("Replay: {label}"))
        .bind(format!("replay-{run_id}"))
        .execute(&mut *tx)
        .await
        .map_err(persistence)?;

        sqlx::query(
            "INSERT INTO pilot_runs
                (id, dataset_id, target_organization_id, label, engine_version,
                 ai_enabled, ai_provider, ai_model, status, started_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'running',$9)",
        )
        .bind(run_id.as_uuid())
        .bind(dataset_id.as_uuid())
        .bind(target.as_uuid())
        .bind(label)
        .bind(engine_version)
        .bind(ai.is_some())
        .bind(ai.map(|(p, _)| p))
        .bind(ai.map(|(_, m)| m))
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DomainError::Validation(format!("a run labelled {label:?} already exists"))
            }
            _ => persistence(e),
        })?;

        tx.commit().await.map_err(persistence)?;

        self.find_run(label)
            .await?
            .ok_or_else(|| DomainError::Persistence("run vanished after creation".into()))
    }

    async fn replay_signals_into_run(
        &self,
        run: &PilotRun,
        at: DateTime<Utc>,
    ) -> Result<(i64, i64), DomainError> {
        let mut tx = self.pool().begin().await.map_err(persistence)?;

        // Sources are recreated inside the run's tenant, preserving name and
        // type so per-source normalization config still applies and the noise
        // breakdown stays meaningful.
        sqlx::query(
            r#"
            INSERT INTO sources (id, organization_id, source_type, name, enabled, config)
            SELECT gen_random_uuid(), $1, s.source_type, s.name, s.enabled, s.config
              FROM sources s
             WHERE s.id IN (
                 SELECT DISTINCT r.source_id
                   FROM pilot_dataset_signals ds
                   JOIN raw_signals r ON r.id = ds.raw_signal_id
                  WHERE ds.dataset_id = $2)
            "#,
        )
        .bind(run.target_organization_id.as_uuid())
        .bind(run.dataset_id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(persistence)?;

        // Payloads are copied verbatim: a replay that alters evidence proves
        // nothing. received_at is the replay time; the source's own timestamp
        // lives in the payload and becomes occurred_at at normalization, which
        // is why correlation windows survive a replay unchanged.
        let replayed = sqlx::query(
            r#"
            INSERT INTO raw_signals
                (id, organization_id, source_id, received_at, external_id,
                 content_type, payload, payload_hash, processing_status)
            SELECT gen_random_uuid(), $1, ns.id, $3, r.external_id,
                   r.content_type, r.payload, r.payload_hash, 'received'
              FROM pilot_dataset_signals ds
              JOIN raw_signals r  ON r.id = ds.raw_signal_id
              JOIN sources os     ON os.id = r.source_id
              JOIN sources ns     ON ns.organization_id = $1 AND ns.name = os.name
             WHERE ds.dataset_id = $2
             ORDER BY ds.ordinal
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(run.target_organization_id.as_uuid())
        .bind(run.dataset_id.as_uuid())
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(persistence)?
        .rows_affected() as i64;

        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pilot_dataset_signals WHERE dataset_id = $1")
                .bind(run.dataset_id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(persistence)?;

        tx.commit().await.map_err(persistence)?;
        Ok((replayed, total - replayed))
    }

    async fn finish_run(
        &self,
        run_id: RunId,
        stats: &RunStats,
        error: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE pilot_runs
                SET status = $2, stats = $3, error = $4, finished_at = $5
              WHERE id = $1",
        )
        .bind(run_id.as_uuid())
        .bind(if error.is_some() {
            "failed"
        } else {
            "completed"
        })
        .bind(stats_to_json(stats))
        .bind(error)
        .bind(at)
        .execute(self.pool())
        .await
        .map_err(persistence)?;
        Ok(())
    }

    async fn collect_stats(
        &self,
        organization_id: OrganizationId,
    ) -> Result<RunStats, DomainError> {
        // One statement so the snapshot is internally consistent: counts taken
        // by separate queries can disagree if anything is still running.
        let row = sqlx::query(
            r#"
            SELECT
              (SELECT COUNT(*) FROM raw_signals WHERE organization_id = $1) AS signals,
              (SELECT COUNT(*) FROM raw_signals
                WHERE organization_id = $1 AND processing_status = 'failed') AS signals_failed,
              (SELECT COUNT(*) FROM events WHERE organization_id = $1) AS events,
              (SELECT COUNT(*) FROM events e
                WHERE e.organization_id = $1
                  AND NOT EXISTS (SELECT 1 FROM incident_events ie WHERE ie.event_id = e.id))
                AS events_without_incident,
              (SELECT COUNT(*) FROM incidents WHERE organization_id = $1) AS incidents,
              (SELECT COUNT(*) FROM incidents
                WHERE organization_id = $1 AND status IN ('open','acknowledged')) AS open,
              (SELECT COUNT(*) FROM incidents
                WHERE organization_id = $1 AND status = 'recovered') AS recovered,
              (SELECT COUNT(*) FROM incidents
                WHERE organization_id = $1 AND severity = 'critical') AS critical,
              (SELECT COUNT(*) FROM incident_events ie
                 JOIN incidents i ON i.id = ie.incident_id
                WHERE i.organization_id = $1 AND ie.relation = 'duplicate') AS duplicates,
              (SELECT COUNT(*) FROM incident_events ie
                 JOIN incidents i ON i.id = ie.incident_id
                WHERE i.organization_id = $1 AND ie.relation = 'recovery') AS recoveries,
              (SELECT COUNT(*) FROM (
                   SELECT fingerprint FROM incidents WHERE organization_id = $1
                    GROUP BY fingerprint HAVING COUNT(*) > 1) t) AS recurrences,
              (SELECT COUNT(*) FROM insights
                WHERE organization_id = $1 AND status = 'ok') AS insights_ok,
              (SELECT COUNT(*) FROM insights
                WHERE organization_id = $1 AND status = 'failed') AS insights_failed
            "#,
        )
        .bind(organization_id.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(persistence)?;

        let g = |k: &str| -> Result<i64, DomainError> { row.try_get(k).map_err(persistence) };
        Ok(RunStats {
            signals_replayed: g("signals")?,
            signals_suppressed: 0, // set by the caller from the replay result
            events: g("events")?,
            events_failed: g("signals_failed")?,
            events_without_incident: g("events_without_incident")?,
            incidents: g("incidents")?,
            open: g("open")?,
            recovered: g("recovered")?,
            critical: g("critical")?,
            duplicate_relations: g("duplicates")?,
            recovery_relations: g("recoveries")?,
            recurrence_patterns: g("recurrences")?,
            insights_ok: g("insights_ok")?,
            insights_failed: g("insights_failed")?,
        })
    }

    async fn list_runs(&self, dataset_id: DatasetId) -> Result<Vec<PilotRun>, DomainError> {
        let rows = sqlx::query(&format!(
            "{RUN_SELECT} WHERE dataset_id = $1 ORDER BY started_at DESC"
        ))
        .bind(dataset_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;
        rows.iter().map(run_from_row).collect()
    }

    async fn find_run(&self, label: &str) -> Result<Option<PilotRun>, DomainError> {
        let row = sqlx::query(&format!(
            "{RUN_SELECT} WHERE label = $1 ORDER BY started_at DESC LIMIT 1"
        ))
        .bind(label)
        .fetch_optional(self.pool())
        .await
        .map_err(persistence)?;
        row.as_ref().map(run_from_row).transpose()
    }
}
