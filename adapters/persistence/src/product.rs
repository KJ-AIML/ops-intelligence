//! Read side and manual transitions for the product API.
//!
//! Aggregation happens in SQL rather than by loading rows into Rust: the
//! Operations View must stay cheap as volume grows, and `COUNT`/`GROUP BY` is
//! exactly the deterministic work principle P1 asks for.
//!
//! Every statement carries `organization_id` explicitly. There is no code path
//! that reads an incident without naming its tenant.

use crate::{persistence, PgStore};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ops_core::domains::events::{Event, EventFamily, EventState, Severity};
use ops_core::domains::incidents::{IncidentEventRelation, IncidentStatus};
use ops_core::error::DomainError;
use ops_core::ids::{EventId, IncidentId, OrganizationId, RawSignalId, SourceId};
use ops_core::insights::{
    share_percent, IncidentSummary, OperationsSummary, RecurringPattern, SourceNoise,
};
use ops_core::ports::{EvidenceEntry, IncidentFilter, IncidentWithEvidence};
use sqlx::Row;
use std::collections::BTreeMap;
use std::str::FromStr;

/// An incident has no title column: the title is evidence, and belongs to the
/// event that opened it. This keeps the incident row free of anything a source
/// could later contradict.
pub(crate) const INCIDENT_SELECT_PUB: &str = INCIDENT_SELECT;

const INCIDENT_SELECT: &str = r#"
    SELECT i.id, i.status, i.severity, i.environment, i.service, i.resource,
           i.event_family, i.fingerprint, i.started_at, i.last_event_at,
           i.recovered_at, i.reopened_count,
           COALESCE(m.event_count, 0)  AS event_count,
           COALESCE(m.source_count, 0) AS source_count,
           COALESCE(o.occurrences, 1)  AS occurrences,
           COALESCE(t.title, '(no evidence)') AS title
      FROM incidents i
      LEFT JOIN LATERAL (
          SELECT COUNT(*) AS event_count, COUNT(DISTINCT e.source_id) AS source_count
            FROM incident_events ie JOIN events e ON e.id = ie.event_id
           WHERE ie.incident_id = i.id
      ) m ON TRUE
      LEFT JOIN LATERAL (
          SELECT COUNT(*) AS occurrences FROM incidents i2
           WHERE i2.organization_id = i.organization_id AND i2.fingerprint = i.fingerprint
      ) o ON TRUE
      LEFT JOIN LATERAL (
          SELECT e.title FROM incident_events ie JOIN events e ON e.id = ie.event_id
           WHERE ie.incident_id = i.id AND ie.relation = 'trigger'
           ORDER BY e.occurred_at LIMIT 1
      ) t ON TRUE
     WHERE i.organization_id = $1
"#;

pub(crate) fn incident_from_row_pub(
    row: &sqlx::postgres::PgRow,
) -> Result<IncidentSummary, DomainError> {
    incident_from_row(row)
}

fn incident_from_row(row: &sqlx::postgres::PgRow) -> Result<IncidentSummary, DomainError> {
    let status: String = row.try_get("status").map_err(persistence)?;
    let severity: String = row.try_get("severity").map_err(persistence)?;
    let family: String = row.try_get("event_family").map_err(persistence)?;
    Ok(IncidentSummary {
        id: IncidentId::from_uuid(row.try_get("id").map_err(persistence)?),
        title: row.try_get("title").map_err(persistence)?,
        status: IncidentStatus::from_str(&status)?,
        severity: Severity::from_str(&severity)?,
        environment: row.try_get("environment").map_err(persistence)?,
        service: row.try_get("service").map_err(persistence)?,
        resource: row.try_get("resource").map_err(persistence)?,
        event_family: EventFamily::from_str(&family)?,
        event_count: row.try_get("event_count").map_err(persistence)?,
        source_count: row.try_get("source_count").map_err(persistence)?,
        started_at: row.try_get("started_at").map_err(persistence)?,
        last_event_at: row.try_get("last_event_at").map_err(persistence)?,
        recovered_at: row.try_get("recovered_at").map_err(persistence)?,
        reopened_count: i64::from(
            row.try_get::<i32, _>("reopened_count")
                .map_err(persistence)?,
        ),
        occurrences: row.try_get("occurrences").map_err(persistence)?,
    })
}

pub(crate) fn event_from_row(row: &sqlx::postgres::PgRow) -> Result<Event, DomainError> {
    let severity: String = row.try_get("severity").map_err(persistence)?;
    let state: String = row.try_get("state").map_err(persistence)?;
    let family: String = row.try_get("event_family").map_err(persistence)?;
    let labels: serde_json::Value = row.try_get("labels").map_err(persistence)?;
    let labels: BTreeMap<String, String> = serde_json::from_value(labels)
        .map_err(|e| DomainError::Persistence(format!("labels are not a string map: {e}")))?;
    Ok(Event {
        id: EventId::from_uuid(row.try_get("id").map_err(persistence)?),
        organization_id: OrganizationId::from_uuid(
            row.try_get("organization_id").map_err(persistence)?,
        ),
        source_id: SourceId::from_uuid(row.try_get("source_id").map_err(persistence)?),
        raw_signal_id: RawSignalId::from_uuid(row.try_get("raw_signal_id").map_err(persistence)?),
        occurred_at: row.try_get("occurred_at").map_err(persistence)?,
        environment: row.try_get("environment").map_err(persistence)?,
        service: row.try_get("service").map_err(persistence)?,
        resource: row.try_get("resource").map_err(persistence)?,
        event_family: EventFamily::from_str(&family)?,
        severity: Severity::from_str(&severity)?,
        state: EventState::from_str(&state)?,
        title: row.try_get("title").map_err(persistence)?,
        message: row.try_get("message").map_err(persistence)?,
        labels,
        external_id: row.try_get("external_id").map_err(persistence)?,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}

impl PgStore {
    async fn incident_by_id(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
    ) -> Result<IncidentSummary, DomainError> {
        let sql = format!("{INCIDENT_SELECT} AND i.id = $2");
        let row = sqlx::query(&sql)
            .bind(organization_id.as_uuid())
            .bind(incident_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(persistence)?
            .ok_or_else(|| DomainError::NotFound(format!("incident {incident_id} not found")))?;
        incident_from_row(&row)
    }
}

#[async_trait]
impl ops_core::ports::ProductQueries for PgStore {
    async fn query_incidents(
        &self,
        organization_id: OrganizationId,
        filter: IncidentFilter,
    ) -> Result<Vec<IncidentSummary>, DomainError> {
        // Optional filters are expressed as "$n IS NULL OR column = $n" so the
        // statement stays a single prepared query instead of string assembly.
        let sql = format!(
            "{INCIDENT_SELECT}
               AND ($2::text        IS NULL OR i.status   = $2)
               AND ($3::text        IS NULL OR i.severity = $3)
               AND ($4::text        IS NULL OR i.service  = $4)
               AND ($5::text        IS NULL OR i.resource = $5)
               AND ($6::timestamptz IS NULL OR i.last_event_at >= $6)
               AND ($7::timestamptz IS NULL OR i.last_event_at <= $7)
             ORDER BY i.last_event_at DESC
             LIMIT $8"
        );
        let rows = sqlx::query(&sql)
            .bind(organization_id.as_uuid())
            .bind(filter.status.map(|s| s.as_str()))
            .bind(filter.severity.map(|s| s.as_str()))
            .bind(filter.service.as_deref())
            .bind(filter.resource.as_deref())
            .bind(filter.from)
            .bind(filter.to)
            .bind(filter.limit.clamp(1, 500))
            .fetch_all(self.pool())
            .await
            .map_err(persistence)?;

        rows.iter().map(incident_from_row).collect()
    }

    async fn incident_with_evidence(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
    ) -> Result<Option<IncidentWithEvidence>, DomainError> {
        let sql = format!("{INCIDENT_SELECT} AND i.id = $2");
        let Some(row) = sqlx::query(&sql)
            .bind(organization_id.as_uuid())
            .bind(incident_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(persistence)?
        else {
            return Ok(None);
        };
        let incident = incident_from_row(&row)?;

        // Ordered by occurred_at: the timeline is the source's story, not ours.
        let rows = sqlx::query(
            r#"
            SELECT e.id, e.organization_id, e.source_id, e.raw_signal_id, e.occurred_at,
                   e.environment, e.service, e.resource, e.event_family, e.severity,
                   e.state, e.title, e.message, e.labels, e.external_id, e.created_at,
                   ie.relation, s.name AS source_name, r.payload AS raw_payload
              FROM incident_events ie
              JOIN events e      ON e.id = ie.event_id
              JOIN sources s     ON s.id = e.source_id
              JOIN raw_signals r ON r.id = e.raw_signal_id
             WHERE ie.incident_id = $2 AND e.organization_id = $1
             ORDER BY e.occurred_at, e.id
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(incident_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;

        let mut timeline = Vec::with_capacity(rows.len());
        for row in &rows {
            let relation: String = row.try_get("relation").map_err(persistence)?;
            timeline.push(EvidenceEntry {
                event: event_from_row(row)?,
                relation: IncidentEventRelation::from_str(&relation)?,
                raw_signal_id: RawSignalId::from_uuid(
                    row.try_get("raw_signal_id").map_err(persistence)?,
                ),
                source_name: row.try_get("source_name").map_err(persistence)?,
                raw_payload: row.try_get("raw_payload").map_err(persistence)?,
            });
        }

        Ok(Some(IncidentWithEvidence { incident, timeline }))
    }

    async fn acknowledge_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
        at: DateTime<Utc>,
    ) -> Result<IncidentSummary, DomainError> {
        // The guard lives in the WHERE clause so two concurrent acknowledges
        // cannot both succeed; the domain rule is enforced by the database, not
        // by a read-then-write race.
        let updated = sqlx::query(
            r#"
            UPDATE incidents
               SET status = 'acknowledged', acknowledged_at = $3
             WHERE organization_id = $1 AND id = $2 AND status = 'open'
            RETURNING id
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(incident_id.as_uuid())
        .bind(at)
        .fetch_optional(self.pool())
        .await
        .map_err(persistence)?;

        if updated.is_none() {
            let current = self.incident_by_id(organization_id, incident_id).await?;
            return Err(DomainError::Validation(format!(
                "cannot acknowledge an incident that is {}",
                current.status
            )));
        }
        self.incident_by_id(organization_id, incident_id).await
    }

    async fn resolve_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
        at: DateTime<Utc>,
    ) -> Result<IncidentSummary, DomainError> {
        let updated = sqlx::query(
            r#"
            UPDATE incidents
               SET status = 'resolved', resolved_at = $3
             WHERE organization_id = $1 AND id = $2 AND status <> 'resolved'
            RETURNING id
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(incident_id.as_uuid())
        .bind(at)
        .fetch_optional(self.pool())
        .await
        .map_err(persistence)?;

        if updated.is_none() {
            let current = self.incident_by_id(organization_id, incident_id).await?;
            return Err(DomainError::Validation(format!(
                "incident is already {}",
                current.status
            )));
        }
        self.incident_by_id(organization_id, incident_id).await
    }

    async fn operations_summary(
        &self,
        organization_id: OrganizationId,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<OperationsSummary, DomainError> {
        // Windowed on occurred_at / last_event_at, never on ingest time: "today"
        // means when things happened, not when we imported them.
        let totals = sqlx::query(
            r#"
            SELECT
              (SELECT COUNT(*) FROM raw_signals rs
                WHERE rs.organization_id = $1) AS raw_signals,
              (SELECT COUNT(*) FROM events e
                WHERE e.organization_id = $1
                  AND ($2::timestamptz IS NULL OR e.occurred_at >= $2)
                  AND ($3::timestamptz IS NULL OR e.occurred_at <= $3)) AS events,
              (SELECT COUNT(*) FROM events e
                WHERE e.organization_id = $1
                  AND ($2::timestamptz IS NULL OR e.occurred_at >= $2)
                  AND ($3::timestamptz IS NULL OR e.occurred_at <= $3)
                  AND NOT EXISTS (SELECT 1 FROM incident_events ie WHERE ie.event_id = e.id))
                AS events_without_incident
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_one(self.pool())
        .await
        .map_err(persistence)?;

        let status_rows = sqlx::query(
            r#"
            SELECT status, severity, COUNT(*) AS n
              FROM incidents
             WHERE organization_id = $1
               AND ($2::timestamptz IS NULL OR last_event_at >= $2)
               AND ($3::timestamptz IS NULL OR last_event_at <= $3)
             GROUP BY status, severity
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;

        let (mut incidents, mut open, mut acknowledged, mut recovered, mut resolved, mut critical) =
            (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
        for row in &status_rows {
            let status: String = row.try_get("status").map_err(persistence)?;
            let severity: String = row.try_get("severity").map_err(persistence)?;
            let n: i64 = row.try_get("n").map_err(persistence)?;
            incidents += n;
            match IncidentStatus::from_str(&status)? {
                IncidentStatus::Open => open += n,
                IncidentStatus::Acknowledged => acknowledged += n,
                IncidentStatus::Recovered => recovered += n,
                IncidentStatus::Resolved => resolved += n,
            }
            if Severity::from_str(&severity)? == Severity::Critical {
                critical += n;
            }
        }

        let source_rows = sqlx::query(
            r#"
            SELECT s.id, s.name, s.source_type,
                   COUNT(DISTINCT rs.id) AS signals,
                   COUNT(DISTINCT e.id)  AS events,
                   COUNT(DISTINCT ie.incident_id) AS incidents
              FROM sources s
              LEFT JOIN raw_signals rs ON rs.source_id = s.id
              LEFT JOIN events e       ON e.source_id  = s.id
                   AND ($2::timestamptz IS NULL OR e.occurred_at >= $2)
                   AND ($3::timestamptz IS NULL OR e.occurred_at <= $3)
              LEFT JOIN incident_events ie ON ie.event_id = e.id
             WHERE s.organization_id = $1
             GROUP BY s.id, s.name, s.source_type
             ORDER BY signals DESC, s.name
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;

        let total_signals: i64 = source_rows
            .iter()
            .map(|r| r.try_get::<i64, _>("signals").unwrap_or(0))
            .sum();

        let mut noisiest_sources = Vec::with_capacity(source_rows.len());
        for row in &source_rows {
            let signals: i64 = row.try_get("signals").map_err(persistence)?;
            noisiest_sources.push(SourceNoise {
                source_id: SourceId::from_uuid(row.try_get("id").map_err(persistence)?),
                name: row.try_get("name").map_err(persistence)?,
                source_type: row.try_get("source_type").map_err(persistence)?,
                signals,
                events: row.try_get("events").map_err(persistence)?,
                incidents: row.try_get("incidents").map_err(persistence)?,
                share_percent: share_percent(signals, total_signals),
            });
        }

        // A recurrence is more than one incident on one exact fingerprint. This
        // is only meaningful because correlation refuses to merge across
        // families (decision 0003) — otherwise everything would look recurring.
        let recurring_rows = sqlx::query(
            r#"
            SELECT fingerprint,
                   MIN(environment)  AS environment,
                   MIN(service)      AS service,
                   MIN(resource)     AS resource,
                   MIN(event_family) AS event_family,
                   COUNT(*)          AS occurrences,
                   MAX(last_event_at) AS last_seen_at,
                   BOOL_OR(status IN ('open', 'acknowledged')) AS currently_active
              FROM incidents
             WHERE organization_id = $1
               AND ($2::timestamptz IS NULL OR last_event_at >= $2)
               AND ($3::timestamptz IS NULL OR last_event_at <= $3)
             GROUP BY fingerprint
            HAVING COUNT(*) > 1
             ORDER BY occurrences DESC, last_seen_at DESC
             LIMIT 20
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;

        let mut recurring = Vec::with_capacity(recurring_rows.len());
        for row in &recurring_rows {
            let family: String = row.try_get("event_family").map_err(persistence)?;
            recurring.push(RecurringPattern {
                fingerprint: row.try_get("fingerprint").map_err(persistence)?,
                environment: row.try_get("environment").map_err(persistence)?,
                service: row.try_get("service").map_err(persistence)?,
                resource: row.try_get("resource").map_err(persistence)?,
                event_family: EventFamily::from_str(&family)?,
                occurrences: row.try_get("occurrences").map_err(persistence)?,
                last_seen_at: row.try_get("last_seen_at").map_err(persistence)?,
                currently_active: row.try_get("currently_active").map_err(persistence)?,
            });
        }

        Ok(OperationsSummary {
            from,
            to,
            raw_signals: totals.try_get("raw_signals").map_err(persistence)?,
            events: totals.try_get("events").map_err(persistence)?,
            events_without_incident: totals
                .try_get("events_without_incident")
                .map_err(persistence)?,
            incidents,
            open,
            acknowledged,
            recovered,
            resolved,
            critical,
            needs_attention: open,
            noisiest_sources,
            recurring,
        })
    }

    async fn query_events(
        &self,
        organization_id: OrganizationId,
        filter: ops_core::ports::EventFilter,
    ) -> Result<Vec<Event>, DomainError> {
        let rows = sqlx::query(
            r#"
            SELECT e.id, e.organization_id, e.source_id, e.raw_signal_id, e.occurred_at,
                   e.environment, e.service, e.resource, e.event_family, e.severity,
                   e.state, e.title, e.message, e.labels, e.external_id, e.created_at
              FROM events e
             WHERE e.organization_id = $1
               AND ($2::timestamptz IS NULL OR e.occurred_at >= $2)
               AND ($3::timestamptz IS NULL OR e.occurred_at <= $3)
               AND ($4::uuid        IS NULL OR e.source_id   = $4)
               AND ($5::text        IS NULL OR e.severity    = $5)
               AND ($6::text        IS NULL OR e.service     = $6)
               AND ($7::text        IS NULL OR e.resource    = $7)
             ORDER BY e.occurred_at DESC, e.id
             LIMIT $8
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(filter.from)
        .bind(filter.to)
        .bind(filter.source_id.map(|s| s.as_uuid()))
        .bind(filter.severity.map(|s| s.as_str()))
        .bind(filter.service.as_deref())
        .bind(filter.resource.as_deref())
        .bind(filter.limit.clamp(1, 500))
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;

        rows.iter().map(event_from_row).collect()
    }
}
