//! Generic Webhook ingestion endpoint.
//!
//! Durably persists the RawSignal and returns immediately (tech sheet 13). The
//! worker normalizes and correlates asynchronously, so a slow pipeline can never
//! make a source's alert delivery time out and retry.

use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ops_core::domains::sources::SourceType;
use ops_core::ports::{InsertOutcome, RawSignalRepository, SourceRepository};
use ops_core::{DomainError, RawSignal};
use rand::Rng;
use serde_json::Value;

/// 32 URL-safe characters from a CSPRNG: unguessable, and short enough to paste
/// into a monitoring tool's contact-point config.
const TOKEN_ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const TOKEN_LEN: usize = 32;

pub fn generate_token() -> String {
    let mut rng = rand::rng();
    (0..TOKEN_LEN)
        .map(|_| TOKEN_ALPHABET[rng.random_range(0..TOKEN_ALPHABET.len())] as char)
        .collect()
}

/// `POST /api/v1/ingest/webhook/{token}`
///
/// The token identifies both the source AND the tenant. Nothing in the request
/// body selects an organization, so a leaked payload cannot cross a tenant
/// boundary (tech sheet 20).
///
/// A Generic Webhook body is one signal. A Grafana body is one notification
/// group carrying several alerts; each alert becomes its own RawSignal so the
/// evidence unit stays "one alert", and the group context travels with it.
pub async fn ingest(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // Deliberately identical responses for "no such token" and "disabled
    // source": a caller must not be able to probe which tokens exist. The token
    // itself is never logged.
    let Some(source) = state.store.find_by_ingest_token(&token).await? else {
        return Err(DomainError::Validation("unknown or disabled ingestion token".into()).into());
    };
    if !source.enabled
        || !matches!(
            source.source_type,
            SourceType::GenericWebhook | SourceType::Grafana
        )
    {
        return Err(DomainError::Validation("unknown or disabled ingestion token".into()).into());
    }

    // Validate before persisting so a malformed body is rejected at the door
    // rather than becoming a signal that can only ever fail normalization.
    // Exhaustive on purpose: a new SourceType must pick a parser here or be
    // refused here, and the compiler makes that choice mandatory.
    let (content_type, drafts): (&str, Vec<(Option<String>, Value)>) = match source.source_type {
        SourceType::Grafana => {
            // Grafana reports how many alerts it dropped from a group. Nothing
            // here can recover them, but their absence must not be silent
            // (product principle P3).
            let truncated = payload
                .get("truncatedAlerts")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if truncated > 0 {
                tracing::warn!(
                    source = %source.name,
                    truncated,
                    "grafana dropped alerts from this notification group"
                );
            }
            // A refused batch is a whole notification group lost on our side.
            // Grafana retries and then gives up, so the refusal is logged here,
            // where the operator can see it, before it becomes a 400.
            let alerts = ops_source_grafana::split_batch(&payload).map_err(|e| {
                tracing::warn!(source = %source.name, error = %e, "grafana batch rejected");
                e
            })?;
            (
                ops_source_grafana::CONTENT_TYPE,
                alerts
                    .into_iter()
                    .map(|alert| (Some(alert.external_id), alert.payload))
                    .collect(),
            )
        }
        SourceType::GenericWebhook => {
            // Same visibility as the Grafana arm: a source posting bodies the
            // engine cannot read must show up in our log, not only in theirs.
            ops_source_webhook::extract_facts(&payload).map_err(|e| {
                tracing::warn!(source = %source.name, error = %e, "webhook payload rejected");
                e
            })?;
            (
                ops_source_webhook::CONTENT_TYPE,
                vec![(ops_source_webhook::external_id_of(&payload), payload)],
            )
        }
        SourceType::CsvImport | SourceType::AzureMonitor | SourceType::Email => {
            // Unreachable while the guard above holds; kept explicit so that
            // adding a type to the guard without a parser fails to compile.
            return Err(DomainError::Validation(format!(
                "{} sources do not receive webhooks",
                source.source_type
            ))
            .into());
        }
    };

    let received_at = state.clock.now();
    let (mut inserted, mut duplicates) = (0usize, 0usize);
    let mut first_id = None;
    for (external_id, body) in drafts {
        let signal = RawSignal::received(
            source.organization_id,
            source.id,
            external_id,
            content_type,
            body,
            received_at,
        );
        match state.store.insert_if_new(&signal).await? {
            InsertOutcome::Inserted(id) => {
                inserted += 1;
                if first_id.is_none() {
                    first_id = Some(id);
                }
            }
            InsertOutcome::Duplicate => duplicates += 1,
        }
    }
    state
        .store
        .touch_last_seen(source.organization_id, source.id, received_at)
        .await?;

    // A redelivery is normal traffic, not an error: answer 200 so the sender
    // stops retrying, and say plainly that it was already known.
    let status = if inserted > 0 {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(serde_json::json!({
            "accepted": true,
            "inserted": inserted,
            "duplicates": duplicates,
            "duplicate": inserted == 0,
            "signal_id": first_id.map(|id| id.to_string()),
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tokens_are_long_unguessable_and_unique() {
        let tokens: HashSet<String> = (0..500).map(|_| generate_token()).collect();
        assert_eq!(tokens.len(), 500, "token collision in 500 draws");
        for t in &tokens {
            assert_eq!(t.len(), TOKEN_LEN);
            assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
        }
    }

    #[test]
    fn token_alphabet_excludes_visually_ambiguous_characters() {
        // Tokens get copied by hand out of a UI into a monitoring tool.
        let alphabet = std::str::from_utf8(TOKEN_ALPHABET).unwrap();
        for c in ['0', 'O', 'l', 'I', '1'] {
            assert!(!alphabet.contains(c), "{c} is easy to mistype");
        }
    }
}
