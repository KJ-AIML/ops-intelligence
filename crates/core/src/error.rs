use thiserror::Error;

/// Categorised domain errors (tech sheet 23).
///
/// Only the variants the current slice can actually produce are defined.
/// Normalization / correlation / reasoning / delivery variants arrive with the
/// code that can return them.
#[derive(Debug, Error)]
pub enum DomainError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("source error: {0}")]
    Source(String),

    #[error("persistence error: {0}")]
    Persistence(String),
}
