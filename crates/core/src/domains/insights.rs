use crate::error::DomainError;
use crate::ids::{IncidentId, InsightId, OrganizationId};
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightType {
    IncidentExplanation,
    Actionability,
    SuggestedInvestigation,
    RecurringPattern,
    NoisePattern,
    DailyBrief,
}

impl InsightType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IncidentExplanation => "incident_explanation",
            Self::Actionability => "actionability",
            Self::SuggestedInvestigation => "suggested_investigation",
            Self::RecurringPattern => "recurring_pattern",
            Self::NoisePattern => "noise_pattern",
            Self::DailyBrief => "daily_brief",
        }
    }
}

impl fmt::Display for InsightType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InsightType {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "incident_explanation" => Ok(Self::IncidentExplanation),
            "actionability" => Ok(Self::Actionability),
            "suggested_investigation" => Ok(Self::SuggestedInvestigation),
            "recurring_pattern" => Ok(Self::RecurringPattern),
            "noise_pattern" => Ok(Self::NoisePattern),
            "daily_brief" => Ok(Self::DailyBrief),
            other => Err(DomainError::Validation(format!(
                "unknown insight type: {other}"
            ))),
        }
    }
}

/// Who produced this. The UI may present both kinds, but the distinction must
/// never be lost: a computed count and a model's interpretation are not the
/// same class of claim (architecture 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightSource {
    Deterministic,
    Ai,
}

impl InsightSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Ai => "ai",
        }
    }
}

impl fmt::Display for InsightSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InsightSource {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "deterministic" => Ok(Self::Deterministic),
            "ai" => Ok(Self::Ai),
            other => Err(DomainError::Validation(format!(
                "unknown insight source: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightStatus {
    Ok,
    Failed,
}

impl InsightStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for InsightStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InsightStatus {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ok" => Ok(Self::Ok),
            "failed" => Ok(Self::Failed),
            other => Err(DomainError::Validation(format!(
                "unknown insight status: {other}"
            ))),
        }
    }
}

/// Audit trail for one reasoning call (tech sheet 11).
///
/// Enough to answer "which model said this, when, and what did it cost" months
/// later. Deliberately carries no prompt content and no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelMetadata {
    pub provider: String,
    pub model: String,
    pub request_id: Option<String>,
    pub latency_ms: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub stop_reason: Option<String>,
    pub prompt_version: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    pub id: InsightId,
    pub organization_id: OrganizationId,
    pub incident_id: Option<IncidentId>,
    pub insight_type: InsightType,
    pub source: InsightSource,
    pub status: InsightStatus,
    pub title: String,
    pub summary: Option<String>,
    pub structured_payload: Option<serde_json::Value>,
    pub schema_version: i32,
    pub model_metadata: ModelMetadata,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_strings_round_trip() {
        for t in [
            InsightType::IncidentExplanation,
            InsightType::Actionability,
            InsightType::SuggestedInvestigation,
            InsightType::RecurringPattern,
            InsightType::NoisePattern,
            InsightType::DailyBrief,
        ] {
            assert_eq!(InsightType::from_str(t.as_str()).unwrap(), t);
        }
        for s in [InsightSource::Deterministic, InsightSource::Ai] {
            assert_eq!(InsightSource::from_str(s.as_str()).unwrap(), s);
        }
        for s in [InsightStatus::Ok, InsightStatus::Failed] {
            assert_eq!(InsightStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn unknown_values_are_rejected_rather_than_defaulted() {
        assert!(InsightType::from_str("vibes").is_err());
        assert!(InsightSource::from_str("magic").is_err());
        assert!(InsightStatus::from_str("maybe").is_err());
    }
}
