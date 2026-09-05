use super::{persistence, PgStore};
use async_trait::async_trait;
use ops_core::correlation::{firing_relation, relation_for, CorrelationAction};
use ops_core::ports::IncidentRepository;
use ops_core::{
    CorrelationWindows, DomainError, Event, EventId, EventState, Incident, IncidentEvent,
    IncidentEventRelation, IncidentFingerprint, IncidentId, OrganizationId, SourceId,
};
use sqlx::{postgres::PgRow, Row};

#[async_trait]
impl IncidentRepository for PgStore {
    async fn correlate_next(&self, organization_id: OrganizationId) -> Result<bool, DomainError> {
        let mut tx = self.pool.begin().await.map_err(persistence)?;
        let row = sqlx::query("SELECT * FROM events WHERE organization_id = $1 AND correlation_status = 'received' ORDER BY occurred_at, id FOR UPDATE SKIP LOCKED LIMIT 1")
            .bind(organization_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(persistence)?;
        let Some(row) = row else { return Ok(false) };
        let event = event_from_row(&row)?;
        sqlx::query("UPDATE events SET correlation_status = 'processing' WHERE id = $1 AND organization_id = $2")
            .bind(event.id.as_uuid()).bind(organization_id.as_uuid()).execute(&mut *tx).await.map_err(persistence)?;
        let fingerprint = IncidentFingerprint::from_event(&event);
        let key = fingerprint.key();
        let open = find_active_incident(&mut tx, organization_id, &key).await?;
        let recovered = if open.is_none() {
            find_recent_recovered(
                &mut tx,
                organization_id,
                &key,
                event.occurred_at,
                event.state == EventState::Resolved,
            )
            .await?
        } else {
            None
        };
        match relation_for(
            &event,
            open.as_ref(),
            recovered.as_ref(),
            CorrelationWindows::default(),
        ) {
            CorrelationAction::Ignore => terminal(&mut tx, &event, "ignored").await?,
            CorrelationAction::Open => {
                let incident = Incident::opened(&event, event.occurred_at);
                insert_incident(&mut tx, &incident).await?;
                insert_relation(
                    &mut tx,
                    incident.id,
                    event.id,
                    IncidentEventRelation::Trigger,
                )
                .await?;
                terminal(&mut tx, &event, "correlated").await?;
            }
            CorrelationAction::AttachFiring => {
                let mut incident = open.expect("action guarantees open");
                let relation =
                    firing_relation(&mut incident, &event, CorrelationWindows::default());
                update_incident(&mut tx, &incident).await?;
                insert_relation(&mut tx, incident.id, event.id, relation).await?;
                terminal(&mut tx, &event, "correlated").await?;
            }
            CorrelationAction::Recover => {
                let mut incident = open.expect("action guarantees open");
                incident.recover(&event);
                update_incident(&mut tx, &incident).await?;
                insert_relation(
                    &mut tx,
                    incident.id,
                    event.id,
                    IncidentEventRelation::Recovery,
                )
                .await?;
                terminal(&mut tx, &event, "correlated").await?;
            }
            CorrelationAction::AttachRecovery => {
                let mut incident = recovered.expect("action guarantees recovered");
                incident.recover(&event);
                update_incident(&mut tx, &incident).await?;
                insert_relation(
                    &mut tx,
                    incident.id,
                    event.id,
                    IncidentEventRelation::Recovery,
                )
                .await?;
                terminal(&mut tx, &event, "correlated").await?;
            }
            CorrelationAction::Reopen => {
                let mut incident = recovered.expect("action guarantees recovered");
                incident.reopen(&event);
                update_incident(&mut tx, &incident).await?;
                insert_relation(
                    &mut tx,
                    incident.id,
                    event.id,
                    IncidentEventRelation::Trigger,
                )
                .await?;
                terminal(&mut tx, &event, "correlated").await?;
            }
        }
        tx.commit().await.map_err(persistence)?;
        Ok(true)
    }
    async fn list_incidents(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<Incident>, DomainError> {
        sqlx::query("SELECT * FROM incidents WHERE organization_id = $1 ORDER BY started_at, id")
            .bind(organization_id.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(persistence)?
            .iter()
            .map(incident_from_row)
            .collect()
    }
    async fn list_incident_events(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<IncidentEvent>, DomainError> {
        sqlx::query("SELECT ie.incident_id, ie.event_id, ie.relation FROM incident_events ie JOIN incidents i ON i.id = ie.incident_id WHERE i.organization_id = $1 ORDER BY ie.incident_id, ie.event_id").bind(organization_id.as_uuid()).fetch_all(&self.pool).await.map_err(persistence)?.iter().map(|r| Ok(IncidentEvent { incident_id: IncidentId::from_uuid(r.try_get("incident_id").map_err(persistence)?), event_id: EventId::from_uuid(r.try_get("event_id").map_err(persistence)?), relation: r.try_get::<String,_>("relation").map_err(persistence)?.parse()? })).collect()
    }
    async fn count_correlation_status(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<(String, i64)>, DomainError> {
        sqlx::query("SELECT correlation_status, COUNT(*) AS n FROM events WHERE organization_id = $1 GROUP BY correlation_status ORDER BY correlation_status").bind(organization_id.as_uuid()).fetch_all(&self.pool).await.map_err(persistence)?.into_iter().map(|r| Ok((r.try_get("correlation_status").map_err(persistence)?, r.try_get("n").map_err(persistence)?))).collect()
    }
}

/// An acknowledged incident is still ACTIVE: someone looking at a problem does
/// not stop it happening, so evidence keeps attaching (migration 0004).
async fn find_active_incident(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: OrganizationId,
    key: &str,
) -> Result<Option<Incident>, DomainError> {
    sqlx::query("SELECT * FROM incidents WHERE organization_id = $1 AND fingerprint = $2 AND status IN ('open','acknowledged') FOR UPDATE").bind(organization_id.as_uuid()).bind(key).fetch_optional(&mut **tx).await.map_err(persistence)?.as_ref().map(incident_from_row).transpose()
}
async fn find_recent_recovered(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: OrganizationId,
    key: &str,
    occurred_at: chrono::DateTime<chrono::Utc>,
    recovery_evidence: bool,
) -> Result<Option<Incident>, DomainError> {
    sqlx::query("SELECT * FROM incidents WHERE organization_id = $1 AND fingerprint = $2 AND status = 'recovered' AND recovered_at <= $3 AND recovered_at >= $3 - CASE WHEN $4 THEN INTERVAL '600 seconds' ELSE INTERVAL '1800 seconds' END ORDER BY recovered_at DESC FOR UPDATE LIMIT 1").bind(organization_id.as_uuid()).bind(key).bind(occurred_at).bind(recovery_evidence).fetch_optional(&mut **tx).await.map_err(persistence)?.as_ref().map(incident_from_row).transpose()
}
async fn terminal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &Event,
    status: &str,
) -> Result<(), DomainError> {
    sqlx::query("UPDATE events SET correlation_status = $2 WHERE id = $1")
        .bind(event.id.as_uuid())
        .bind(status)
        .execute(&mut **tx)
        .await
        .map_err(persistence)?;
    Ok(())
}
async fn insert_incident(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    incident: &Incident,
) -> Result<(), DomainError> {
    sqlx::query("INSERT INTO incidents (id,organization_id,fingerprint,environment,service,resource,event_family,status,severity,started_at,last_event_at,recovered_at,acknowledged_at,resolved_at,reopened_count,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)").bind(incident.id.as_uuid()).bind(incident.organization_id.as_uuid()).bind(incident.fingerprint.key()).bind(&incident.fingerprint.environment).bind(&incident.fingerprint.service).bind(&incident.fingerprint.resource).bind(incident.fingerprint.event_family.as_str()).bind(incident.status.as_str()).bind(incident.severity.as_str()).bind(incident.started_at).bind(incident.last_event_at).bind(incident.recovered_at).bind(incident.acknowledged_at).bind(incident.resolved_at).bind(incident.reopened_count as i32).bind(incident.created_at).execute(&mut **tx).await.map_err(persistence)?;
    Ok(())
}
async fn update_incident(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    incident: &Incident,
) -> Result<(), DomainError> {
    sqlx::query("UPDATE incidents SET status=$2,severity=$3,last_event_at=$4,recovered_at=$5,reopened_count=$6,acknowledged_at=$7,resolved_at=$8 WHERE id=$1").bind(incident.id.as_uuid()).bind(incident.status.as_str()).bind(incident.severity.as_str()).bind(incident.last_event_at).bind(incident.recovered_at).bind(incident.reopened_count as i32).bind(incident.acknowledged_at).bind(incident.resolved_at).execute(&mut **tx).await.map_err(persistence)?;
    Ok(())
}
async fn insert_relation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    incident_id: IncidentId,
    event_id: EventId,
    relation: IncidentEventRelation,
) -> Result<(), DomainError> {
    sqlx::query("INSERT INTO incident_events (incident_id,event_id,relation) VALUES ($1,$2,$3)")
        .bind(incident_id.as_uuid())
        .bind(event_id.as_uuid())
        .bind(relation.as_str())
        .execute(&mut **tx)
        .await
        .map_err(persistence)?;
    Ok(())
}

fn event_from_row(row: &PgRow) -> Result<Event, DomainError> {
    Ok(Event {
        id: EventId::from_uuid(row.try_get("id").map_err(persistence)?),
        organization_id: OrganizationId::from_uuid(
            row.try_get("organization_id").map_err(persistence)?,
        ),
        source_id: SourceId::from_uuid(row.try_get("source_id").map_err(persistence)?),
        raw_signal_id: ops_core::RawSignalId::from_uuid(
            row.try_get("raw_signal_id").map_err(persistence)?,
        ),
        occurred_at: row.try_get("occurred_at").map_err(persistence)?,
        environment: row.try_get("environment").map_err(persistence)?,
        service: row.try_get("service").map_err(persistence)?,
        resource: row.try_get("resource").map_err(persistence)?,
        event_family: row
            .try_get::<String, _>("event_family")
            .map_err(persistence)?
            .parse()?,
        severity: row
            .try_get::<String, _>("severity")
            .map_err(persistence)?
            .parse()?,
        state: row
            .try_get::<String, _>("state")
            .map_err(persistence)?
            .parse()?,
        title: row.try_get("title").map_err(persistence)?,
        message: row.try_get("message").map_err(persistence)?,
        labels: serde_json::from_value(row.try_get("labels").map_err(persistence)?)
            .map_err(|e| DomainError::Persistence(e.to_string()))?,
        external_id: row.try_get("external_id").map_err(persistence)?,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}
fn incident_from_row(row: &PgRow) -> Result<Incident, DomainError> {
    let organization_id =
        OrganizationId::from_uuid(row.try_get("organization_id").map_err(persistence)?);
    Ok(Incident {
        id: IncidentId::from_uuid(row.try_get("id").map_err(persistence)?),
        organization_id,
        fingerprint: IncidentFingerprint {
            organization_id,
            environment: row.try_get("environment").map_err(persistence)?,
            service: row.try_get("service").map_err(persistence)?,
            resource: row.try_get("resource").map_err(persistence)?,
            event_family: row
                .try_get::<String, _>("event_family")
                .map_err(persistence)?
                .parse()?,
        },
        status: row
            .try_get::<String, _>("status")
            .map_err(persistence)?
            .parse()?,
        severity: row
            .try_get::<String, _>("severity")
            .map_err(persistence)?
            .parse()?,
        started_at: row.try_get("started_at").map_err(persistence)?,
        last_event_at: row.try_get("last_event_at").map_err(persistence)?,
        recovered_at: row.try_get("recovered_at").map_err(persistence)?,
        acknowledged_at: row.try_get("acknowledged_at").map_err(persistence)?,
        resolved_at: row.try_get("resolved_at").map_err(persistence)?,
        reopened_count: row
            .try_get::<i32, _>("reopened_count")
            .map_err(persistence)? as u32,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}
