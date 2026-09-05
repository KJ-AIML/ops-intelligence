//! Insight persistence.

use crate::product::incident_from_row_pub;
use crate::{persistence, PgStore};
use async_trait::async_trait;
use ops_core::domains::insights::{
    Insight, InsightSource, InsightStatus, InsightType, ModelMetadata,
};
use ops_core::error::DomainError;
use ops_core::ids::{IncidentId, InsightId, OrganizationId};
use ops_core::insights::IncidentSummary;
use ops_core::ports::InsightRepository;
use sqlx::Row;
use std::str::FromStr;

const INSIGHT_COLUMNS: &str = "id, organization_id, incident_id, insight_type, source, status, \
     title, summary, structured_payload, schema_version, model_metadata, error, created_at";

fn insight_from_row(row: &sqlx::postgres::PgRow) -> Result<Insight, DomainError> {
    let insight_type: String = row.try_get("insight_type").map_err(persistence)?;
    let source: String = row.try_get("source").map_err(persistence)?;
    let status: String = row.try_get("status").map_err(persistence)?;
    let metadata: serde_json::Value = row.try_get("model_metadata").map_err(persistence)?;

    Ok(Insight {
        id: InsightId::from_uuid(row.try_get("id").map_err(persistence)?),
        organization_id: OrganizationId::from_uuid(
            row.try_get("organization_id").map_err(persistence)?,
        ),
        incident_id: row
            .try_get::<Option<uuid::Uuid>, _>("incident_id")
            .map_err(persistence)?
            .map(IncidentId::from_uuid),
        insight_type: InsightType::from_str(&insight_type)?,
        source: InsightSource::from_str(&source)?,
        status: InsightStatus::from_str(&status)?,
        title: row.try_get("title").map_err(persistence)?,
        summary: row.try_get("summary").map_err(persistence)?,
        structured_payload: row.try_get("structured_payload").map_err(persistence)?,
        schema_version: row.try_get("schema_version").map_err(persistence)?,
        model_metadata: metadata_from_json(&metadata),
        error: row.try_get("error").map_err(persistence)?,
        created_at: row.try_get("created_at").map_err(persistence)?,
    })
}

fn metadata_to_json(m: &ModelMetadata) -> serde_json::Value {
    serde_json::json!({
        "provider": m.provider,
        "model": m.model,
        "request_id": m.request_id,
        "latency_ms": m.latency_ms,
        "input_tokens": m.input_tokens,
        "output_tokens": m.output_tokens,
        "stop_reason": m.stop_reason,
        "prompt_version": m.prompt_version,
        "attempts": m.attempts,
    })
}

fn metadata_from_json(v: &serde_json::Value) -> ModelMetadata {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    ModelMetadata {
        provider: s("provider"),
        model: s("model"),
        request_id: v
            .get("request_id")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        latency_ms: v.get("latency_ms").and_then(|x| x.as_u64()).unwrap_or(0),
        input_tokens: v
            .get("input_tokens")
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
        output_tokens: v
            .get("output_tokens")
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
        stop_reason: v
            .get("stop_reason")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        prompt_version: s("prompt_version"),
        attempts: v.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
    }
}

#[async_trait]
impl InsightRepository for PgStore {
    async fn insert(&self, insight: &Insight) -> Result<(), DomainError> {
        // ON CONFLICT DO NOTHING against the one-successful-insight-per-incident
        // index: a concurrent second reasoning run must not double-write, and
        // losing the race is not an error.
        sqlx::query(
            r#"
            INSERT INTO insights
                (id, organization_id, incident_id, insight_type, source, status, title, summary,
                 structured_payload, schema_version, model_metadata, error, created_at)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(insight.id.as_uuid())
        .bind(insight.organization_id.as_uuid())
        .bind(insight.incident_id.map(|i| i.as_uuid()))
        .bind(insight.insight_type.as_str())
        .bind(insight.source.as_str())
        .bind(insight.status.as_str())
        .bind(&insight.title)
        .bind(insight.summary.as_deref())
        .bind(insight.structured_payload.as_ref())
        .bind(insight.schema_version)
        .bind(metadata_to_json(&insight.model_metadata))
        .bind(insight.error.as_deref())
        .bind(insight.created_at)
        .execute(self.pool())
        .await
        .map_err(persistence)?;
        Ok(())
    }

    async fn list_for_incident(
        &self,
        organization_id: OrganizationId,
        incident_id: IncidentId,
    ) -> Result<Vec<Insight>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {INSIGHT_COLUMNS} FROM insights
              WHERE organization_id = $1 AND incident_id = $2
              ORDER BY created_at DESC"
        ))
        .bind(organization_id.as_uuid())
        .bind(incident_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;
        rows.iter().map(insight_from_row).collect()
    }

    async fn list_recent(
        &self,
        organization_id: OrganizationId,
        limit: i64,
    ) -> Result<Vec<Insight>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {INSIGHT_COLUMNS} FROM insights
              WHERE organization_id = $1
              ORDER BY created_at DESC LIMIT $2"
        ))
        .bind(organization_id.as_uuid())
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await
        .map_err(persistence)?;
        rows.iter().map(insight_from_row).collect()
    }

    async fn incidents_needing_insight(
        &self,
        organization_id: OrganizationId,
        insight_type: InsightType,
        limit: i64,
    ) -> Result<Vec<IncidentSummary>, DomainError> {
        // Active first, then most recent. Reasoning costs money per call, so a
        // bounded run must spend it on what an engineer is most likely to open.
        let sql = format!(
            "{}
               AND NOT EXISTS (
                   SELECT 1 FROM insights ins
                    WHERE ins.incident_id = i.id
                      AND ins.insight_type = $2
                      AND ins.status = 'ok')
             ORDER BY (i.status IN ('open','acknowledged')) DESC,
                      (i.severity = 'critical') DESC,
                      i.last_event_at DESC
             LIMIT $3",
            crate::product::INCIDENT_SELECT_PUB
        );
        let rows = sqlx::query(&sql)
            .bind(organization_id.as_uuid())
            .bind(insight_type.as_str())
            .bind(limit.clamp(1, 200))
            .fetch_all(self.pool())
            .await
            .map_err(persistence)?;
        rows.iter().map(incident_from_row_pub).collect()
    }
}
