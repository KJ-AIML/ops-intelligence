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
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use ops_core::domains::events::Severity;
use ops_core::domains::incidents::IncidentStatus;
use ops_core::domains::sources::SourceType;
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
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

const DEFAULT_LIMIT: i64 = 100;
/// Grafana posts one body per notification group, and its rendered digest
/// travels in that body even though the engine never stores it. Measured
/// against the fixture, a 1 MiB limit admitted 540 to 815 alerts; 4 MiB admits
/// roughly 2,160 to 3,260 at the same alert shapes. The cap still keeps a
/// misconfigured source from exhausting memory (tech sheet 21): what a hostile
/// body can cost is bounded by the adapter's expansion budget, not by this
/// number, so raising this raises only the realistic ceiling. The operator-side
/// guarantee is Grafana's `Max alerts` contact-point setting; see the README.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
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
    let api_token = std::env::var("API_TOKEN")
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    if api_token.is_none() {
        tracing::warn!(
            "API_TOKEN is not set: the product API accepts every request (local development only)"
        );
    }

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

    let web_dir = std::env::var("WEB_DIST_DIR").unwrap_or_else(|_| "web/dist".to_string());
    let app = build_router(state, api_token, &web_dir);

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .with_context(|| format!("binding {host}:{port}"))?;
    tracing::info!(%host, port, organization = %organization.slug, "server listening");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}

fn build_router(state: AppState, api_token: Option<String>, web_dir: &str) -> Router {
    let router = Router::new()
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
        // An unmatched path under `/api` is a wrong or stale API call, never
        // a React Router route. Without this, a typo'd path like
        // `/api/v1/incidnets` would miss every route above and fall through
        // to the SPA catch-all below, answering an API client's mistake with
        // a 200 and an HTML body instead of a 404.
        .route("/api/{*rest}", axum::routing::any(unknown_api_route));

    // The built UI is served from the same origin as the API: no CORS, and no
    // second process on the pilot host. Unknown non-API paths fall back to
    // index.html so React Router owns them. Registered before the layer block
    // so static requests get the same body limit, 413 warning and trace span
    // as everything else.
    let index = std::path::Path::new(web_dir).join("index.html");
    let router = if index.is_file() {
        tracing::info!(%web_dir, "serving web UI");
        // `ServeDir::not_found_service` wraps its fallback in `SetStatus`,
        // which rewrites the response status to 404 regardless of what the
        // inner service returned — great for a pretty 404 *page*, wrong here:
        // it would serve index.html's bytes to every deep link (`/incidents`,
        // `/sources`, ...) with a 404 status stamped over the 200 ServeFile
        // actually produced. React Router still renders it client-side, so a
        // browser click-through looks healthy while `curl -f`, uptime checks
        // and status-keyed caches all see a false failure. `ServeDir::fallback`
        // passes the inner status straight through instead, which is the SPA
        // idiom: unmatched paths get the shell with a real 200.
        //
        // That alone would make `/assets/nope.js` (or any other path under
        // the built asset directory that doesn't exist on disk) *also* fall
        // through to the SPA shell with a 200 and an HTML content type — a
        // stale index.html referencing a dead asset hash would then surface
        // as an opaque "module has MIME type text/html" console error
        // instead of a clean 404. Nesting a plain `ServeDir` (no fallback of
        // its own) at `/assets` first keeps that prefix honest: a miss there
        // is answered by tower-http's own empty-bodied 404 and never reaches
        // the SPA catch-all below.
        router
            .nest_service(
                "/assets",
                ServeDir::new(std::path::Path::new(web_dir).join("assets")),
            )
            .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
    } else {
        tracing::warn!(%web_dir, "web build not found; serving API only (run `npm run build` in web/)");
        router
    };

    router
        .layer(middleware::from_fn_with_state(api_token, require_api_token))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        // axum's `Json` extractor enforces its own 2 MiB default independently
        // of the layer above, so raising MAX_BODY_BYTES past 2 MiB would
        // otherwise be silently capped back down to 2 MiB here. The layer
        // above is already the single source of truth for the ingest limit
        // (and the one `warn_on_oversized_body` reports), so disable this
        // second, hidden one rather than keep two numbers in sync. Both of
        // these layers only wrap the routes already registered above them on
        // this chain: a route added after this `.layer(...)` block falls back
        // to axum's 2 MiB default with no `warn_on_oversized_body` coverage.
        // The static fallback is registered above, before this block, for
        // exactly this reason.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(middleware::from_fn(warn_on_oversized_body))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                tracing::debug_span!(
                    "request",
                    method = %request.method(),
                    route = %matched_route(request),
                )
            }),
        )
        .with_state(state)
}

/// Placeholder for a request axum's router never matched to a route (its
/// built-in 404, or the static fallback when the web build is present).
/// `MatchedPath` is only ever absent in that case, which after the static
/// fallback was added means every UI asset request; a placeholder rather
/// than the literal path keeps those spans low-cardinality too.
const UNMATCHED_ROUTE: &str = "unmatched";

/// The route *pattern* a request matched, e.g.
/// `/api/v1/ingest/webhook/{token}` — never the literal request path. For the
/// ingest route the literal path IS the ingestion token: logging it verbatim
/// would print a live write credential into every log sink at whatever level
/// captures the event (README.md and webhook.rs both promise the token is
/// never logged, at any level). `MatchedPath` is inserted into request
/// extensions by axum's router during route matching, which happens before
/// any `Router::layer` middleware runs — including both callers of this
/// function below — so it is already present by the time either reads it.
fn matched_route(request: &Request) -> &str {
    request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(axum::extract::MatchedPath::as_str)
        .unwrap_or(UNMATCHED_ROUTE)
}

/// `RequestBodyLimitLayer` turns an oversized body into a bare 413 before any
/// handler runs, so the usual "reject and log" path in webhook.rs never fires
/// for the largest refusals — exactly the ones an operator most needs to see
/// (tech sheet 21, Task 3c fix round 1). `TraceLayer`'s default classifier
/// only treats 5xx as a failure and logs 2xx/4xx responses at DEBUG, so at the
/// default `info` filter a 413 here was otherwise silent. This wraps the
/// whole router and promotes exactly that one case to a warning, naming the
/// route and the configured limit; every other response passes through
/// unchanged and unlogged. Logs the matched *pattern*, not the literal path —
/// see `matched_route` — because the one route this warning exists to serve
/// carries its ingestion token in the literal path.
async fn warn_on_oversized_body(request: Request, next: Next) -> Response {
    let route = matched_route(&request).to_string();
    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        tracing::warn!(
            route = %route,
            limit_bytes = MAX_BODY_BYTES,
            "request body exceeds the ingest limit"
        );
    }
    response
}

/// Bearer check for the product API (tech sheet 20, option A: a local auth
/// boundary). The ingest route authenticates with its own per-source token and
/// the health routes must stay open for the compose healthcheck, so both are
/// matched by route pattern and skipped. Static assets never match a route and
/// pass too: the SPA shell is public, the data behind it is not. An unset token
/// disables the check, which is only acceptable on a developer's machine.
async fn require_api_token(
    State(expected): State<Option<String>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = expected else {
        return next.run(request).await;
    };
    let route = matched_route(&request);
    if route == UNMATCHED_ROUTE
        || route.starts_with("/api/v1/ingest/")
        || route.starts_with("/health")
    {
        return next.run(request).await;
    }
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or("");
    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "api token required" })),
        )
            .into_response()
    }
}

/// Length is not hidden; the token is random and long, so that leaks nothing
/// useful. Content comparison does not short-circuit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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

/// Catches any `/api/...` path that missed every route above. Kept separate
/// from the SPA fallback so a wrong or stale API call gets a plain 404
/// instead of the HTML shell.
async fn unknown_api_route() -> StatusCode {
    StatusCode::NOT_FOUND
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
    /// `generic_webhook` (default) or `grafana`.
    #[serde(default)]
    source_type: Option<String>,
}

/// Creates a webhook-fed source. The token is generated server-side and
/// returned exactly once — it is not readable from any later request.
async fn create_source(
    State(state): State<AppState>,
    Json(body): Json<CreateSource>,
) -> ApiResult<(StatusCode, Json<dto::CreatedSourceDto>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(DomainError::Validation("source name is required".into()).into());
    }
    let source_type = match body
        .source_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => SourceType::GenericWebhook,
        Some(s) => SourceType::from_str(s)?,
    };
    if !matches!(
        source_type,
        SourceType::GenericWebhook | SourceType::Grafana
    ) {
        return Err(DomainError::Validation(format!(
            "{source_type} sources do not receive webhooks; use generic_webhook or grafana"
        ))
        .into());
    }
    let token = webhook::generate_token();
    let source = state
        .store
        .create_webhook(state.organization_id, source_type, name, &token)
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

// ------------------------------------------------------- body limit tests

// Router-level regression pin for the gap between "MAX_BODY_BYTES says 4 MiB"
// and "axum's `Json` extractor silently still caps at 2 MiB" (see the comment
// on `DefaultBodyLimit::disable()` above). No database needed: a throwaway
// route stands in for the real ingest route, wrapped in the same two layers
// `main()` wires around the whole router. Without `DefaultBodyLimit::disable()`
// the first test fails, because the request never gets past axum's hidden
// default to reach the handler at all.
#[cfg(test)]
mod body_limit_tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::routing::post as route_post;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn probe_router() -> Router {
        async fn accept(Json(_body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            Json(serde_json::json!({ "received": true }))
        }

        Router::new()
            .route("/probe", route_post(accept))
            .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
            .layer(DefaultBodyLimit::disable())
    }

    /// A JSON body of at least `min_len` bytes. Exact length is a few bytes
    /// over `min_len` (the `{"pad":"...."}` wrapper), which is irrelevant:
    /// callers only need to land on the right side of a threshold, not hit
    /// it exactly.
    fn json_body_at_least(min_len: usize) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "pad": "x".repeat(min_len) })).unwrap()
    }

    fn post_probe(body: Vec<u8>) -> Request {
        axum::http::Request::builder()
            .method("POST")
            .uri("/probe")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_between_axums_hidden_default_and_the_configured_limit_is_accepted() {
        // 75% of MAX_BODY_BYTES: anchored to the constant, not a literal, so
        // this tracks it if it ever changes. At today's 4 MiB that is 3 MiB —
        // comfortably above axum's independent 2 MiB `Json` default and
        // comfortably below MAX_BODY_BYTES.
        let target = MAX_BODY_BYTES - MAX_BODY_BYTES / 4;
        let body = json_body_at_least(target);
        assert!(
            body.len() > 2 * 1024 * 1024,
            "test body ({} bytes) must exceed axum's independent 2 MiB Json \
             default, or this test cannot exercise the regression it pins",
            body.len()
        );
        assert!(body.len() < MAX_BODY_BYTES);

        let response = probe_router().oneshot(post_probe(body)).await.unwrap();
        assert_ne!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "a body under MAX_BODY_BYTES must not be refused, even though it \
             exceeds axum's hidden 2 MiB default"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["received"], true);
    }

    #[tokio::test]
    async fn a_body_over_the_configured_limit_is_refused() {
        let target = MAX_BODY_BYTES + MAX_BODY_BYTES / 4;
        let body = json_body_at_least(target);
        assert!(body.len() > MAX_BODY_BYTES);

        let response = probe_router().oneshot(post_probe(body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}

#[cfg(test)]
mod api_token_tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get as route_get, post as route_post};
    use tower::ServiceExt;

    fn probe_router(token: Option<&str>) -> Router {
        async fn ok() -> Json<serde_json::Value> {
            Json(serde_json::json!({ "ok": true }))
        }
        Router::new()
            .route("/api/v1/probe", route_get(ok))
            .route("/api/v1/ingest/webhook/{token}", route_post(ok))
            .route("/health", route_get(ok))
            .layer(middleware::from_fn_with_state(
                token.map(str::to_owned),
                require_api_token,
            ))
    }

    fn get(path: &str, bearer: Option<&str>) -> Request {
        let mut builder = axum::http::Request::builder().method("GET").uri(path);
        if let Some(b) = bearer {
            builder = builder.header("authorization", format!("Bearer {b}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn the_product_api_needs_the_token_when_one_is_configured() {
        let app = probe_router(Some("s3cret"));
        assert_eq!(
            app.clone()
                .oneshot(get("/api/v1/probe", None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(get("/api/v1/probe", Some("wrong")))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.oneshot(get("/api/v1/probe", Some("s3cret")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn ingestion_and_health_stay_outside_the_bearer_check() {
        let app = probe_router(Some("s3cret"));
        let ingest = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/ingest/webhook/abc")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(ingest).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(get("/health", None)).await.unwrap().status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn no_configured_token_means_no_check() {
        let app = probe_router(None);
        assert_eq!(
            app.oneshot(get("/api/v1/probe", None))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
}

// ------------------------------------------------------ static-serving tests

// Pins the property Task 5's router split exists for: the static fallback
// registered in front of `.layer(...)` behaves like every other route on the
// chain — a real 200 for deep links, a real 404 for a missing built asset,
// and still inside the body limit. Nothing above exercises this:
// `body_limit_tests::probe_router` registers no fallback at all, so a
// regression that moved `.fallback_service(...)`/`.nest_service(...)` below
// the layer block, or swapped `ServeDir::fallback` back for
// `not_found_service`, would leave every other test green with nothing to
// catch it (fix round 2 on Task 5 — a browser click-through found this after
// the curl-body-only checks in the brief's Step 3 missed it).
//
// Manually verified (fix round 2, not left in the tree): reverting
// `ServeDir::fallback(...)` back to `ServeDir::not_found_service(...)` in
// `static_probe_router` flips `a_deep_link_gets_the_spa_shell_with_200_not_404`
// from pass to fail (200 becomes 404). Moving the `.nest_service("/assets", ...)`
// call after `.fallback_service(...)` so `/assets` misses hit the SPA
// catch-all instead flips `a_missing_built_asset_stays_a_real_404_not_the_spa_shell`
// (404 becomes 200, text/html body). Moving the `.layer(...)` calls in
// `static_probe_router` above the `.nest_service`/`.fallback_service` calls —
// mirroring the bug this task's ordering rule exists to prevent — flips
// `an_oversized_request_through_the_static_fallback_is_still_413` (413
// becomes whatever axum's hidden 2 MiB `Json` default would otherwise do,
// since the body limit no longer wraps the fallback at all).
#[cfg(test)]
mod static_serving_tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::routing::post as route_post;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// A throwaway `web/dist`-shaped directory under the OS temp dir: an
    /// `index.html` the SPA fallback should serve for any unknown non-API
    /// path, and an empty `assets/` directory so a request for a file that
    /// was never built stays a real 404 instead of silently resolving to the
    /// SPA shell. Removed on drop so a failed assertion still leaves no
    /// litter on disk. `tempfile` is not a dependency of this workspace, so
    /// this hand-rolls the same lifetime pattern under `std::env::temp_dir()`.
    struct TempWebDist {
        dir: std::path::PathBuf,
    }

    impl TempWebDist {
        fn new(unique: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ops-server-static-test-{unique}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("assets")).expect("create temp assets dir");
            std::fs::write(
                dir.join("index.html"),
                "<!doctype html><html><body>spa-shell</body></html>",
            )
            .expect("write temp index.html");
            Self { dir }
        }
    }

    impl Drop for TempWebDist {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Mirrors the shape of the chain in `main()`: routes, then the static
    /// fallback (nested `/assets` plus the SPA catch-all), then the same two
    /// body-limit layers `main()` wires around the whole router — in that
    /// order, so this pins the ordering of the fix, not just its presence.
    fn static_probe_router(web_dir: &std::path::Path) -> Router {
        async fn accept(Json(_body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            Json(serde_json::json!({ "received": true }))
        }

        let index = web_dir.join("index.html");
        let router = Router::new().route("/probe", route_post(accept));
        let router = router
            .nest_service("/assets", ServeDir::new(web_dir.join("assets")))
            .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)));
        router
            .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
            .layer(DefaultBodyLimit::disable())
    }

    fn get(path: &str) -> Request {
        axum::http::Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_deep_link_gets_the_spa_shell_with_200_not_404() {
        let dist = TempWebDist::new("deep-link");
        let response = static_probe_router(&dist.dir)
            .oneshot(get("/incidents"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "an unknown non-API path is a React Router route, not a missing \
             resource: it must serve the SPA shell with 200, not the 404 \
             `ServeDir::not_found_service` would stamp over it"
        );
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("text/html"),
            "expected an HTML content type for the SPA shell, got {content_type:?}"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.starts_with(b"<!doctype html>"));
    }

    #[tokio::test]
    async fn a_missing_built_asset_stays_a_real_404_not_the_spa_shell() {
        let dist = TempWebDist::new("missing-asset");
        let response = static_probe_router(&dist.dir)
            .oneshot(get("/assets/nope.js"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a request under /assets that misses on disk must not resolve to \
             the SPA shell — that would surface as a confusing 'MIME type \
             text/html' console error instead of a clean 404"
        );
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            !content_type.starts_with("text/html"),
            "a missing asset must not be answered with an HTML body, got \
             content-type {content_type:?}"
        );
    }

    #[tokio::test]
    async fn an_oversized_request_through_the_static_fallback_is_still_413() {
        // Proves the ordering, not just the presence, of the fix: the static
        // fallback is registered before `.layer(RequestBodyLimitLayer::new(...))`
        // on the chain built by `static_probe_router`, so a request the
        // fallback would otherwise serve never reaches it at all — it is
        // turned away at the body limit first.
        //
        // `RequestBodyLimit::call` (tower-http 0.6.11,
        // src/limit/service.rs) checks `Content-Length` *eagerly*, before
        // the inner service — the whole rest of this router, fallback
        // included — ever runs: `Some(len) if len > self.limit =>
        // ResponseFuture::payload_too_large()`. That is deliberate here: a
        // static-file service like `ServeDir` never reads a POST body at all
        // (it 405s on the method before touching it), so without an eager,
        // header-based check an oversized request that the router routes to
        // static serving would sail through unread rather than being turned
        // away — which is exactly the gap this task's ordering rule closes.
        // The `Content-Length` header is set explicitly below to exercise
        // that path; body_limit_tests's own cases omit it and instead rely
        // on the lazy, read-time enforcement that fires once the `/probe`
        // handler's `Json` extractor actually consumes the body.
        let dist = TempWebDist::new("oversized");
        let target = MAX_BODY_BYTES + MAX_BODY_BYTES / 4;
        let body = serde_json::to_vec(&serde_json::json!({ "pad": "x".repeat(target) })).unwrap();
        assert!(body.len() > MAX_BODY_BYTES);
        let content_length = body.len().to_string();
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/not-a-real-route")
            .header("content-type", "application/json")
            .header(axum::http::header::CONTENT_LENGTH, content_length)
            .body(Body::from(body))
            .unwrap();
        let response = static_probe_router(&dist.dir)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
