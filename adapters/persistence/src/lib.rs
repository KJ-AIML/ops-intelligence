//! PostgreSQL persistence adapter (SQLx).
//!
//! Uses runtime-checked `sqlx::query` rather than the compile-time `query!`
//! macros.
//! ponytail: runtime-checked queries, so `cargo build` needs no live database
//! and no committed `.sqlx` offline cache. Switch to the macros (and commit
//! `.sqlx`) if CI starts catching schema drift late.
//!
//! Every statement is parameterised, and every tenant-owned query carries
//! `organization_id` explicitly (tech sheet 21).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ops_core::domains::organizations::Organization;
use ops_core::domains::raw_signals::{IdempotencyKey, ProcessingStatus, RawSignal};
use ops_core::domains::sources::{Source, SourceType};
use ops_core::error::DomainError;
use ops_core::ids::{OrganizationId, RawSignalId, SourceId};
use ops_core::ports::{
    InsertOutcome, OrganizationRepository, RawSignalRepository, SourceRepository,
};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::str::FromStr;

fn persistence(e: sqlx::Error) -> DomainError {
    DomainError::Persistence(e.to_string())
}

pub async fn connect(database_url: &str) -> Result<PgPool, DomainError> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .map_err(persistence)
}

/// Applies `migrations/` from an empty database upward. Idempotent.
pub async fn run_migrations(pool: &PgPool) -> Result<(), DomainError> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .map_err(|e| DomainError::Persistence(format!("migration failed: {e}")))
}

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl OrganizationRepository for PgStore {
    async fn ensure_by_slug(&self, slug: &str, name: &str) -> Result<Organization, DomainError> {
        let row = sqlx::query(
            r#"
            INSERT INTO organizations (id, name, slug)
            VALUES ($1, $2, $3)
            ON CONFLICT (slug) DO UPDATE SET updated_at = now()
            RETURNING id, name, slug, created_at
            "#,
        )
        .bind(OrganizationId::new().as_uuid())
        .bind(name)
        .bind(slug)
        .fetch_one(&self.pool)
        .await
        .map_err(persistence)?;

        Ok(Organization {
            id: OrganizationId::from_uuid(row.try_get("id").map_err(persistence)?),
            name: row.try_get("name").map_err(persistence)?,
            slug: row.try_get("slug").map_err(persistence)?,
            created_at: row.try_get("created_at").map_err(persistence)?,
        })
    }
}

#[async_trait]
impl SourceRepository for PgStore {
    async fn ensure(
        &self,
        organization_id: OrganizationId,
        source_type: SourceType,
        name: &str,
    ) -> Result<Source, DomainError> {
        let row = sqlx::query(
            r#"
            INSERT INTO sources (id, organization_id, source_type, name)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (organization_id, name) DO UPDATE SET updated_at = now()
            RETURNING id, organization_id, source_type, name, enabled, last_seen_at, created_at
            "#,
        )
        .bind(SourceId::new().as_uuid())
        .bind(organization_id.as_uuid())
        .bind(source_type.as_str())
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(persistence)?;

        let type_str: String = row.try_get("source_type").map_err(persistence)?;

        Ok(Source {
            id: SourceId::from_uuid(row.try_get("id").map_err(persistence)?),
            organization_id: OrganizationId::from_uuid(
                row.try_get("organization_id").map_err(persistence)?,
            ),
            source_type: SourceType::from_str(&type_str)?,
            name: row.try_get("name").map_err(persistence)?,
            enabled: row.try_get("enabled").map_err(persistence)?,
            last_seen_at: row.try_get("last_seen_at").map_err(persistence)?,
            created_at: row.try_get("created_at").map_err(persistence)?,
        })
    }

    async fn touch_last_seen(
        &self,
        organization_id: OrganizationId,
        source_id: SourceId,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        sqlx::query(
            r#"
            UPDATE sources
               SET last_seen_at = $3, updated_at = now()
             WHERE organization_id = $1
               AND id = $2
            "#,
        )
        .bind(organization_id.as_uuid())
        .bind(source_id.as_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(persistence)?;
        Ok(())
    }
}

#[async_trait]
impl RawSignalRepository for PgStore {
    async fn insert_if_new(&self, signal: &RawSignal) -> Result<InsertOutcome, DomainError> {
        // Which partial unique index guards this row depends on whether the source
        // gave us a stable id, so the conflict target has to match (tech sheet 9).
        let sql = match signal.idempotency_key() {
            IdempotencyKey::ExternalId(_) => {
                r#"
                INSERT INTO raw_signals
                    (id, organization_id, source_id, received_at, external_id,
                     content_type, payload, payload_hash, processing_status)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (source_id, external_id) WHERE external_id IS NOT NULL
                DO NOTHING
                RETURNING id
                "#
            }
            IdempotencyKey::PayloadHash(_) => {
                r#"
                INSERT INTO raw_signals
                    (id, organization_id, source_id, received_at, external_id,
                     content_type, payload, payload_hash, processing_status)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (source_id, payload_hash) WHERE external_id IS NULL
                DO NOTHING
                RETURNING id
                "#
            }
        };

        let inserted: Option<uuid::Uuid> = sqlx::query_scalar(sql)
            .bind(signal.id.as_uuid())
            .bind(signal.organization_id.as_uuid())
            .bind(signal.source_id.as_uuid())
            .bind(signal.received_at)
            .bind(signal.external_id.as_deref())
            .bind(&signal.content_type)
            .bind(&signal.payload)
            .bind(&signal.payload_hash)
            .bind(signal.processing_status.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(persistence)?;

        Ok(match inserted {
            Some(id) => InsertOutcome::Inserted(RawSignalId::from_uuid(id)),
            None => InsertOutcome::Duplicate,
        })
    }

    async fn count_by_status(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<(String, i64)>, DomainError> {
        let rows = sqlx::query(
            r#"
            SELECT processing_status, COUNT(*) AS n
              FROM raw_signals
             WHERE organization_id = $1
             GROUP BY processing_status
             ORDER BY processing_status
            "#,
        )
        .bind(organization_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(persistence)?;

        rows.into_iter()
            .map(|r| {
                let status: String = r.try_get("processing_status").map_err(persistence)?;
                // Reject anything the domain does not recognise rather than
                // silently reporting an unknown status.
                ProcessingStatus::from_str(&status)?;
                let n: i64 = r.try_get("n").map_err(persistence)?;
                Ok((status, n))
            })
            .collect()
    }
}
