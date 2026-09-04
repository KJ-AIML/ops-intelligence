use super::{persistence, PgStore};
use async_trait::async_trait;
use ops_core::ports::{Clock, EventRepository, SignalNormalizer};
use ops_core::{
    DomainError, Event, EventId, OrganizationId, ProcessingStatus, RawSignal, RawSignalId,
    SourceId, SourceType,
};
use sqlx::{postgres::PgRow, Row};

#[async_trait]
impl EventRepository for PgStore {
    async fn process_next(
        &self,
        organization_id: OrganizationId,
        normalizer: &dyn SignalNormalizer,
        clock: &dyn Clock,
    ) -> Result<Option<ProcessingStatus>, DomainError> {
        let mut tx = self.pool.begin().await.map_err(persistence)?;
        // Keep the row lock until both evidence and terminal status commit. A killed
        // worker leaves received work behind, never a stranded processing claim.
        let row = sqlx::query(
            r#"
            SELECT r.*, s.source_type FROM raw_signals r
            JOIN sources s ON s.id = r.source_id AND s.organization_id = r.organization_id
            WHERE r.organization_id = $1 AND r.processing_status = 'received'
            ORDER BY r.received_at, r.id FOR UPDATE OF r SKIP LOCKED LIMIT 1
        "#,
        )
        .bind(organization_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(persistence)?;
        let Some(row) = row else { return Ok(None) };
        let raw = RawSignal {
            id: RawSignalId::from_uuid(row.try_get("id").map_err(persistence)?),
            organization_id,
            source_id: SourceId::from_uuid(row.try_get("source_id").map_err(persistence)?),
            received_at: row.try_get("received_at").map_err(persistence)?,
            external_id: row.try_get("external_id").map_err(persistence)?,
            content_type: row.try_get("content_type").map_err(persistence)?,
            payload: row.try_get("payload").map_err(persistence)?,
            payload_hash: row.try_get("payload_hash").map_err(persistence)?,
            processing_status: ProcessingStatus::Processing,
            processing_error: None,
        };
        sqlx::query("UPDATE raw_signals SET processing_status = 'processing', processing_error = NULL, updated_at = now() WHERE organization_id = $1 AND id = $2")
            .bind(organization_id.as_uuid()).bind(raw.id.as_uuid()).execute(&mut *tx).await.map_err(persistence)?;
        let source_type: String = row.try_get("source_type").map_err(persistence)?;
        let outcome = source_type
            .parse::<SourceType>()
            .and_then(|source_type| normalizer.normalize(&raw, source_type, clock.now()))
            .and_then(|event| {
                if event.organization_id != raw.organization_id
                    || event.source_id != raw.source_id
                    || event.raw_signal_id != raw.id
                    || event.external_id != raw.external_id
                {
                    Err(DomainError::Validation(
                        "normalized Event evidence identity mismatch".into(),
                    ))
                } else {
                    Ok(event)
                }
            });
        let (status, error) = match outcome {
            Ok(event) => {
                let labels = serde_json::to_value(&event.labels)
                    .map_err(|e| DomainError::Validation(e.to_string()))?;
                // No conflict suppression: an unexpected pre-existing Event is an
                // invariant violation, not permission to mark different evidence processed.
                sqlx::query(r#"
                    INSERT INTO events (id, organization_id, source_id, raw_signal_id, occurred_at,
                        environment, service, resource, event_family, severity, state, title, message,
                        labels, external_id, created_at)
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
                "#)
                    .bind(event.id.as_uuid()).bind(event.organization_id.as_uuid()).bind(event.source_id.as_uuid())
                    .bind(event.raw_signal_id.as_uuid()).bind(event.occurred_at).bind(event.environment)
                    .bind(event.service).bind(event.resource).bind(event.event_family.as_str())
                    .bind(event.severity.as_str()).bind(event.state.as_str()).bind(event.title).bind(event.message)
                    .bind(labels).bind(event.external_id).bind(event.created_at)
                    .execute(&mut *tx).await.map_err(persistence)?;
                (ProcessingStatus::Processed, None)
            }
            Err(error) => (ProcessingStatus::Failed, Some(error.to_string())),
        };
        sqlx::query("UPDATE raw_signals SET processing_status = $3, processing_error = $4, updated_at = now() WHERE organization_id = $1 AND id = $2")
            .bind(organization_id.as_uuid()).bind(raw.id.as_uuid()).bind(status.as_str()).bind(error)
            .execute(&mut *tx).await.map_err(persistence)?;
        tx.commit().await.map_err(persistence)?;
        Ok(Some(status))
    }

    async fn list(&self, organization_id: OrganizationId) -> Result<Vec<Event>, DomainError> {
        let rows =
            sqlx::query("SELECT * FROM events WHERE organization_id = $1 ORDER BY occurred_at, id")
                .bind(organization_id.as_uuid())
                .fetch_all(&self.pool)
                .await
                .map_err(persistence)?;
        rows.iter().map(event_from_row).collect()
    }
}

fn event_from_row(row: &PgRow) -> Result<Event, DomainError> {
    let family: String = row.try_get("event_family").map_err(persistence)?;
    let severity: String = row.try_get("severity").map_err(persistence)?;
    let state: String = row.try_get("state").map_err(persistence)?;
    let labels: serde_json::Value = row.try_get("labels").map_err(persistence)?;
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
        event_family: family.parse()?,
        severity: severity.parse()?,
        state: state.parse()?,
        title: row.try_get("title").map_err(persistence)?,
        message: row.try_get("message").map_err(persistence)?,
        labels: serde_json::from_value(labels)
            .map_err(|e| DomainError::Persistence(e.to_string()))?,
        external_id: row.try_get("external_id").map_err(persistence)?,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}
