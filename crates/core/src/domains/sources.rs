use crate::error::DomainError;
use crate::ids::{OrganizationId, SourceId};
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;

/// Configured input. `CsvImport` is the offline/synthetic path; `GenericWebhook`
/// is the first live-style one. Vendor types are added only once the real source
/// inventory selects them (decision 0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    CsvImport,
    GenericWebhook,
    Grafana,
    AzureMonitor,
    Email,
}

impl SourceType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::CsvImport => "csv_import",
            Self::GenericWebhook => "generic_webhook",
            Self::Grafana => "grafana",
            Self::AzureMonitor => "azure_monitor",
            Self::Email => "email",
        }
    }
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceType {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "csv_import" => Ok(Self::CsvImport),
            "generic_webhook" => Ok(Self::GenericWebhook),
            "grafana" => Ok(Self::Grafana),
            "azure_monitor" => Ok(Self::AzureMonitor),
            "email" => Ok(Self::Email),
            other => Err(DomainError::Validation(format!(
                "unknown source type: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: SourceId,
    pub organization_id: OrganizationId,
    pub source_type: SourceType,
    pub name: String,
    pub enabled: bool,
    /// Webhook credential. Unguessable, unique, and never written to a log or
    /// returned by a list endpoint (tech sheet 21).
    pub ingest_token: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
