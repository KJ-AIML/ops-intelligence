//! `reason` — generate bounded AI explanations for incidents that lack one.
//!
//! Every call costs money and sends derived facts to a provider, so this is
//! deliberately opt-in (`AI_ENABLED`), bounded (`limit`), idempotent (one
//! successful insight per incident), and it records failures rather than
//! retrying them silently.

use anyhow::Result;
use ops_core::domains::insights::{
    Insight, InsightSource, InsightStatus, InsightType, ModelMetadata,
};
use ops_core::intelligence::reasoner::{PROMPT_VERSION, SCHEMA_VERSION};
use ops_core::intelligence::{
    DisabledProvider, IncidentContext, IncidentReasoner, ReasoningProvider,
};
use ops_core::ports::{Clock, InsightRepository, ProductQueries};
use ops_core::{InsightId, OrganizationId};
use ops_persistence::PgStore;
use std::time::Instant;

/// Selects the configured provider, or the disabled one.
///
/// The disabled path is the shipping default: no key, no calls, no data leaving
/// the deployment until the pilot's data boundary is agreed
/// (source inventory 21).
pub fn provider_from_env() -> Box<dyn ReasoningProvider> {
    match ops_ai_anthropic::AnthropicConfig::from_env() {
        None => Box::new(DisabledProvider),
        Some(config) => match ops_ai_anthropic::AnthropicProvider::new(config) {
            Ok(provider) => Box::new(provider),
            Err(e) => {
                tracing::error!(error = %e, "AI is enabled but the provider could not be built; falling back to disabled");
                Box::new(DisabledProvider)
            }
        },
    }
}

/// Build the anchored context. This function is the data boundary: whatever it
/// does not copy across, the model never sees.
pub async fn build_context(
    store: &PgStore,
    organization_id: OrganizationId,
    incident: &ops_core::insights::IncidentSummary,
) -> Result<IncidentContext> {
    let evidence = store
        .incident_with_evidence(organization_id, incident.id)
        .await?;

    let (mut titles, mut sources) = (Vec::new(), Vec::new());
    if let Some(detail) = &evidence {
        for entry in &detail.timeline {
            // Titles only. Messages and raw payloads stay out by construction.
            if !titles.contains(&entry.event.title) {
                titles.push(entry.event.title.clone());
            }
            if !sources.contains(&entry.source_name) {
                sources.push(entry.source_name.clone());
            }
        }
    }
    titles.truncate(10);

    let duration_seconds = incident
        .recovered_at
        .unwrap_or(incident.last_event_at)
        .signed_duration_since(incident.started_at)
        .num_seconds()
        .max(0);

    Ok(IncidentContext {
        incident_id: incident.id.to_string(),
        title: incident.title.clone(),
        status: incident.status,
        severity: incident.severity,
        event_family: incident.event_family,
        environment: incident.environment.clone(),
        service: incident.service.clone(),
        resource: incident.resource.clone(),
        duration_seconds,
        event_count: incident.event_count,
        source_count: incident.source_count,
        recent_recurrences: incident.occurrences,
        reopened_count: incident.reopened_count,
        evidence_titles: titles,
        source_names: sources,
    })
}

pub struct ReasoningReport {
    pub explained: usize,
    pub failed: usize,
    pub considered: usize,
    pub provider: String,
    pub model: String,
    pub enabled: bool,
}

pub async fn run(
    store: &PgStore,
    organization_id: OrganizationId,
    provider: &dyn ReasoningProvider,
    clock: &dyn Clock,
    limit: i64,
) -> Result<ReasoningReport> {
    let descriptor = provider.descriptor();
    let mut report = ReasoningReport {
        explained: 0,
        failed: 0,
        considered: 0,
        provider: descriptor.provider.clone(),
        model: descriptor.model.clone(),
        enabled: provider.is_enabled(),
    };

    let candidates = store
        .incidents_needing_insight(organization_id, InsightType::IncidentExplanation, limit)
        .await?;
    report.considered = candidates.len();

    if !provider.is_enabled() {
        // Not an error. The deterministic product is complete without this.
        return Ok(report);
    }

    for incident in &candidates {
        let context = build_context(store, organization_id, incident).await?;
        let started = Instant::now();

        match IncidentReasoner::explain(provider, &context).await {
            Ok((explanation, response, attempts)) => {
                let metadata = ModelMetadata {
                    provider: descriptor.provider.clone(),
                    model: response.model.clone(),
                    request_id: response.request_id.clone(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    input_tokens: response.input_tokens,
                    output_tokens: response.output_tokens,
                    stop_reason: response.stop_reason.clone(),
                    prompt_version: PROMPT_VERSION.to_string(),
                    attempts,
                };
                let insight = Insight {
                    id: InsightId::new(),
                    organization_id,
                    incident_id: Some(incident.id),
                    insight_type: InsightType::IncidentExplanation,
                    source: InsightSource::Ai,
                    status: InsightStatus::Ok,
                    title: incident.title.clone(),
                    summary: Some(explanation.explanation.clone()),
                    structured_payload: Some(serde_json::to_value(&explanation)?),
                    schema_version: SCHEMA_VERSION,
                    model_metadata: metadata,
                    error: None,
                    created_at: clock.now(),
                };
                store.insert(&insight).await?;
                report.explained += 1;
                tracing::info!(incident = %incident.id, attempts, "insight generated");
            }
            Err(e) => {
                // Recorded, not swallowed: a held result stays visible and the
                // incident is not silently left without an explanation.
                let insight = Insight {
                    id: InsightId::new(),
                    organization_id,
                    incident_id: Some(incident.id),
                    insight_type: InsightType::IncidentExplanation,
                    source: InsightSource::Ai,
                    status: InsightStatus::Failed,
                    title: incident.title.clone(),
                    summary: None,
                    structured_payload: None,
                    schema_version: SCHEMA_VERSION,
                    model_metadata: ModelMetadata {
                        provider: descriptor.provider.clone(),
                        model: descriptor.model.clone(),
                        latency_ms: started.elapsed().as_millis() as u64,
                        prompt_version: PROMPT_VERSION.to_string(),
                        ..Default::default()
                    },
                    error: Some(e.to_string()),
                    created_at: clock.now(),
                };
                store.insert(&insight).await?;
                report.failed += 1;
                tracing::warn!(incident = %incident.id, error = %e, "reasoning failed");
            }
        }
    }

    Ok(report)
}
