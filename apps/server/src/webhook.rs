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
    if !source.enabled || source.source_type != SourceType::GenericWebhook {
        return Err(DomainError::Validation("unknown or disabled ingestion token".into()).into());
    }

    // Validate before persisting so a malformed body is rejected at the door
    // rather than becoming a signal that can only ever fail normalization.
    ops_source_webhook::extract_facts(&payload)?;

    let received_at = state.clock.now();
    let signal = RawSignal::received(
        source.organization_id,
        source.id,
        ops_source_webhook::external_id_of(&payload),
        ops_source_webhook::CONTENT_TYPE,
        payload,
        received_at,
    );

    let outcome = state.store.insert_if_new(&signal).await?;
    state
        .store
        .touch_last_seen(source.organization_id, source.id, received_at)
        .await?;

    // A redelivery is normal traffic, not an error: answer 200 so the sender
    // stops retrying, and say plainly that it was already known.
    Ok(match outcome {
        InsertOutcome::Inserted(id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "accepted": true, "signal_id": id.to_string() })),
        ),
        InsertOutcome::Duplicate => (
            StatusCode::OK,
            Json(serde_json::json!({ "accepted": true, "duplicate": true })),
        ),
    })
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
