//! HTTP server: product API, Generic Webhook ingestion, health.
//!
//! Handlers stay thin (guardrail 34.9): parse, delegate, serialise. All domain
//! logic lives in core, all SQL in the persistence adapter.
//!
//! Tenancy: the organization is resolved from server configuration for the
//! product API, and from the ingestion token for webhooks. A request body can
//! never select a tenant (tech sheet 20).

mod dto;
mod webhook;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use ops_core::domains::events::Severity;
use ops_core::domains::incidents::IncidentStatus;
use ops_core::ids::{IncidentId, OrganizationId, SourceId};
use ops_core::ports::{
    EventFilter, IncidentFilter, InsightRepository, ProductQueries, SourceRepository,
};
use ops_core::{Clock, DomainError, SystemClock};
use ops_persistence::PgStore;
use serde::Deserialize;
use std::str::FromStr;
use std::sync::Arc;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

const DEFAULT_LIMIT: i64 = 100;
/// Alert payloads are small. A cap keeps a misconfigured source from exhausting
/// memory (tech sheet 21).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Upper bound on the set considered when ranking by attention. Matches the
/// repository's own hard LIMIT clamp.
const MAX_RANKING_CANDIDATES: i64 = 500;

#[derive(Clone)]
struct AppState {
    store: Arc<PgStore>,
    organization_id: OrganizationId,
    clock: Arc<dyn Clock>,
    base_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL is not set (see the config block in README.md)")?;
    let org_slug =
        std::env::var("DEFAULT_ORGANIZATION_SLUG").unwrap_or_else(|_| "pilot-org".to_string());
    let host = std::env::var("APP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = std::env::var("APP_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse()
        .context("APP_PORT must be a port number")?;
    let base_url =
        std::env::var("API_BASE_URL").unwrap_or_else(|_| format!("http://{host}:{port}"));

    let pool = ops_persistence::connect(&database_url)
        .await
        .context("connecting to PostgreSQL")?;
    ops_persistence::run_migrations(&pool)
        .await
        .context("applying migrations")?;
    let store = PgStore::new(pool);

    let organization = ops_core::ports::OrganizationRepository::ensure_by_slug(
        &store,
        &org_slug,
        "Pilot Organization",
    )
    .await
    .context("ensuring organization")?;

    let state = AppState {
        store: Arc::new(store),
        organization_id: organization.id,
        clock: Arc::new(SystemClock),
        base_url,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .route("/api/v1/operations/summary", get(summary))
        .route("/api/v1/incidents", get(list_incidents))
        .route("/api/v1/incidents/{id}", get(get_incident))
        .route("/api/v1/incidents/{id}/acknowledge", post(acknowledge))
        .route("/api/v1/incidents/{id}/resolve", post(resolve))
        .route("/api/v1/incidents/{id}/insights", get(incident_insights))
        .route("/api/v1/insights", get(list_insights))
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/sources", get(list_sources).post(create_source))
        .route("/api/v1/sources/{id}", patch(update_source))
        .route("/api/v1/ingest/webhook/{token}", post(webhook::ingest))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .with_context(|| format!("binding {host}:{port}"))?;
    tracing::info!(%host, port, organization = %organization.slug, "server listening");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}

// ---------------------------------------------------------------- error type

/// Maps domain errors onto status codes. Validation is a client problem;
/// anything else is ours and must not leak internals to the caller.
pub struct ApiError(DomainError);

impl From<DomainError> for ApiError {
    fn from(e: DomainError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            DomainError::Validation(m) => (StatusCode::BAD_REQUEST, m.clone()),
            DomainError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            // A reasoning failure never breaks a request: the deterministic
            // answer is still correct and still served.
            DomainError::Reasoning(m) => {
                tracing::warn!(reason = %m, "reasoning unavailable");
                (StatusCode::SERVICE_UNAVAILABLE, m.clone())
            }
            DomainError::Source(m) => (StatusCode::BAD_REQUEST, m.clone()),
            DomainError::Persistence(m) => {
                tracing::error!(error = %m, "persistence failure");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

// ---------------------------------------------------------------- handlers

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness depends on the database actually answering, not on the process
/// being up. Never exposes connection details (tech sheet 21).
async fn ready(State(state): State<AppState>) -> Response {
    match state.store.ping().await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "unavailable" })),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct WindowQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

async fn summary(
    State(state): State<AppState>,
    Query(q): Query<WindowQuery>,
) -> ApiResult<Json<dto::SummaryResponse>> {
    let summary = state
        .store
        .operations_summary(state.organization_id, q.from, q.to)
        .await?;
    Ok(Json(summary.into()))
}

#[derive(Deserialize)]
struct IncidentQuery {
    status: Option<String>,
    severity: Option<String>,
    service: Option<String>,
    resource: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<i64>,
    /// Sort by deterministic triage rank instead of recency.
    sort: Option<String>,
}

async fn list_incidents(
    State(state): State<AppState>,
    Query(q): Query<IncidentQuery>,
) -> ApiResult<Json<Vec<dto::IncidentDto>>> {
    let requested = q.limit.unwrap_or(DEFAULT_LIMIT);
    let by_attention = q.sort.as_deref() == Some("attention");

    // Ranking has to see the whole candidate set, not one page of it. Sorting
    // after a LIMIT returns "the most recent N, reordered", which is a different
    // and much less useful answer than "the N that matter most".
    //
    // ponytail: pull the candidate set and rank in memory, bounded by
    // MAX_RANKING_CANDIDATES. Exact while a tenant holds fewer incidents than
    // that bound. If it stops holding, push attention_score into SQL and keep
    // the core function as the single definition to test it against.
    let filter = IncidentFilter {
        status: q
            .status
            .as_deref()
            .map(IncidentStatus::from_str)
            .transpose()?,
        severity: q.severity.as_deref().map(Severity::from_str).transpose()?,
        service: q.service,
        resource: q.resource,
        from: q.from,
        to: q.to,
        limit: if by_attention {
            MAX_RANKING_CANDIDATES
        } else {
            requested
        },
    };
    let mut incidents = state
        .store
        .query_incidents(state.organization_id, filter)
        .await?;

    if by_attention {
        ops_core::insights::rank_by_attention(&mut incidents);
        incidents.truncate(requested.clamp(1, MAX_RANKING_CANDIDATES) as usize);
    }
    Ok(Json(incidents.iter().map(dto::IncidentDto::from).collect()))
}

async fn get_incident(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::IncidentDetailDto>> {
    let id = parse_incident_id(&id)?;
    state
        .store
        .incident_with_evidence(state.organization_id, id)
        .await?
        .map(|d| Json(d.into()))
        .ok_or_else(|| ApiError(DomainError::NotFound(format!("incident {id} not found"))))
}

async fn acknowledge(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::IncidentDto>> {
    let id = parse_incident_id(&id)?;
    let incident = state
        .store
        .acknowledge_incident(state.organization_id, id, state.clock.now())
        .await?;
    Ok(Json(dto::IncidentDto::from(&incident)))
}

async fn resolve(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::IncidentDto>> {
    let id = parse_incident_id(&id)?;
    let incident = state
        .store
        .resolve_incident(state.organization_id, id, state.clock.now())
        .await?;
    Ok(Json(dto::IncidentDto::from(&incident)))
}

#[derive(Deserialize)]
struct EventQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    source_id: Option<String>,
    severity: Option<String>,
    service: Option<String>,
    resource: Option<String>,
    limit: Option<i64>,
}

async fn list_events(
    State(state): State<AppState>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Json<Vec<dto::EventDto>>> {
    let source_id = match q.source_id.as_deref() {
        None => None,
        Some(s) => Some(SourceId::from_uuid(uuid::Uuid::parse_str(s).map_err(
            |_| DomainError::Validation("source_id must be a UUID".into()),
        )?)),
    };
    let filter = EventFilter {
        from: q.from,
        to: q.to,
        source_id,
        severity: q.severity.as_deref().map(Severity::from_str).transpose()?,
        service: q.service,
        resource: q.resource,
        limit: q.limit.unwrap_or(DEFAULT_LIMIT),
    };
    let events = state
        .store
        .query_events(state.organization_id, filter)
        .await?;
    Ok(Json(events.iter().map(dto::EventDto::from).collect()))
}

async fn list_sources(State(state): State<AppState>) -> ApiResult<Json<Vec<dto::SourceDto>>> {
    let sources = state.store.list_sources(state.organization_id).await?;
    Ok(Json(sources.iter().map(dto::SourceDto::from).collect()))
}

#[derive(Deserialize)]
struct CreateSource {
    name: String,
}

/// Creates a Generic Webhook source. The token is generated server-side and
/// returned exactly once — it is not readable from any later request.
async fn create_source(
    State(state): State<AppState>,
    Json(body): Json<CreateSource>,
) -> ApiResult<(StatusCode, Json<dto::CreatedSourceDto>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(DomainError::Validation("source name is required".into()).into());
    }
    let token = webhook::generate_token();
    let source = state
        .store
        .create_webhook(state.organization_id, name, &token)
        .await?;

    let response = dto::CreatedSourceDto {
        source: dto::SourceDto::from(&source),
        ingest_url: format!("{}/api/v1/ingest/webhook/{}", state.base_url, token),
        ingest_token: token,
    };
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Deserialize)]
struct UpdateSource {
    enabled: bool,
}

async fn update_source(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSource>,
) -> ApiResult<Json<dto::SourceDto>> {
    let id = SourceId::from_uuid(
        uuid::Uuid::parse_str(&id)
            .map_err(|_| DomainError::Validation("source id must be a UUID".into()))?,
    );
    let source = state
        .store
        .set_enabled(state.organization_id, id, body.enabled)
        .await?;
    Ok(Json(dto::SourceDto::from(&source)))
}

fn parse_incident_id(raw: &str) -> Result<IncidentId, DomainError> {
    uuid::Uuid::parse_str(raw)
        .map(IncidentId::from_uuid)
        .map_err(|_| DomainError::Validation("incident id must be a UUID".into()))
}

async fn incident_insights(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<dto::InsightDto>>> {
    let id = parse_incident_id(&id)?;
    let insights = state
        .store
        .list_for_incident(state.organization_id, id)
        .await?;
    Ok(Json(insights.iter().map(dto::InsightDto::from).collect()))
}

#[derive(Deserialize)]
struct InsightQuery {
    limit: Option<i64>,
}

async fn list_insights(
    State(state): State<AppState>,
    Query(q): Query<InsightQuery>,
) -> ApiResult<Json<Vec<dto::InsightDto>>> {
    let insights = state
        .store
        .list_recent(state.organization_id, q.limit.unwrap_or(DEFAULT_LIMIT))
        .await?;
    Ok(Json(insights.iter().map(dto::InsightDto::from).collect()))
}
