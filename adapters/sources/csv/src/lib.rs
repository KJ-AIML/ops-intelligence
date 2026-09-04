//! CSV / spreadsheet source adapter.
//!
//! Responsibility boundary (decision 0002): this adapter extracts raw source
//! facts only. It preserves each row verbatim, then extracts SourceFacts and
//! FamilyHints. CsvNormalizer delegates severity/state/family classification to
//! the core normalization layer; no canonical keyword table lives in this adapter.

pub mod normalization;
pub use normalization::CsvNormalizer;

use ops_core::error::DomainError;
use serde_json::{Map, Value};
use std::io::Read;
use std::path::Path;

/// Content type recorded on every RawSignal produced from a CSV row.
pub const CONTENT_TYPE: &str = "text/csv";

/// Columns the importer needs in order to route and preserve a row.
/// `timestamp` and `title` are the canonical required minimum (tech sheet 14);
/// `source` is needed to attribute the row to an originating system.
const REQUIRED_HEADERS: [&str; 3] = ["timestamp", "source", "title"];

/// One CSV row, ready to become a RawSignal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvSignal {
    /// Value of the `source` column — the system the alert originally came from.
    pub origin: String,
    /// Source-side identifier, when the row carries one. Drives idempotency.
    pub external_id: Option<String>,
    /// The row, verbatim, every column preserved.
    pub payload: Value,
    /// 1-based data row number (excluding the header), for error reporting.
    pub row_number: u64,
}

pub fn read_signals_from_path(path: &Path) -> Result<Vec<CsvSignal>, DomainError> {
    let file = std::fs::File::open(path)
        .map_err(|e| DomainError::Source(format!("cannot open {}: {e}", path.display())))?;
    read_signals(file)
}

pub fn read_signals<R: Read>(reader: R) -> Result<Vec<CsvSignal>, DomainError> {
    let mut rdr = csv::Reader::from_reader(reader);

    let headers = rdr
        .headers()
        .map_err(|e| DomainError::Source(format!("cannot read CSV header: {e}")))?
        .clone();

    for required in REQUIRED_HEADERS {
        if !headers.iter().any(|h| h.trim() == required) {
            return Err(DomainError::Source(format!(
                "CSV is missing required column `{required}` (found: {})",
                headers.iter().collect::<Vec<_>>().join(", ")
            )));
        }
    }

    let mut out = Vec::new();
    for (idx, record) in rdr.records().enumerate() {
        let row_number = idx as u64 + 1;
        let record =
            record.map_err(|e| DomainError::Source(format!("CSV row {row_number}: {e}")))?;

        // Preserve every column verbatim. Empty strings stay empty strings rather
        // than becoming null: "the source sent an empty field" and "the source
        // omitted the field" are different facts, and this is the evidence record.
        let mut payload = Map::new();
        for (header, value) in headers.iter().zip(record.iter()) {
            payload.insert(header.trim().to_string(), Value::String(value.to_string()));
        }

        let origin = non_empty(&payload, "source").ok_or_else(|| {
            DomainError::Source(format!("CSV row {row_number}: `source` column is empty"))
        })?;

        out.push(CsvSignal {
            origin,
            external_id: non_empty(&payload, "external_id"),
            payload: Value::Object(payload),
            row_number,
        });
    }

    Ok(out)
}

fn non_empty(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "row_id,timestamp,source,external_id,title,severity,state\n\
        R001,2026-09-02T02:40:12+07:00,grafana,GRF-39880,HighAPILatency,warning,alerting\n\
        R002,2026-09-02T05:30:19+07:00,uptime_kuma,,vpn-gw Up,,Up\n";

    #[test]
    fn reads_rows_and_preserves_every_column() {
        let rows = read_signals(SAMPLE.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);

        let first = &rows[0];
        assert_eq!(first.origin, "grafana");
        assert_eq!(first.external_id.as_deref(), Some("GRF-39880"));
        assert_eq!(first.row_number, 1);
        assert_eq!(first.payload["title"], "HighAPILatency");
        assert_eq!(first.payload["row_id"], "R001");
        // every header is present in the payload, nothing dropped
        assert_eq!(first.payload.as_object().unwrap().len(), 7);
    }

    #[test]
    fn empty_external_id_becomes_none_so_dedup_falls_back_to_payload_hash() {
        let rows = read_signals(SAMPLE.as_bytes()).unwrap();
        assert_eq!(rows[1].external_id, None);
        // ...but the empty column itself is still preserved as evidence
        assert_eq!(rows[1].payload["external_id"], "");
    }

    #[test]
    fn missing_required_column_fails_fast() {
        let err =
            read_signals("timestamp,title\n2026-09-02T00:00:00+07:00,x\n".as_bytes()).unwrap_err();
        assert!(err.to_string().contains("source"), "got: {err}");
    }

    #[test]
    fn empty_source_cell_is_rejected() {
        let err = read_signals("timestamp,source,title\n2026-09-02T00:00:00+07:00,,x\n".as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("row 1"), "got: {err}");
    }

    #[test]
    fn header_only_file_is_valid_and_empty() {
        assert!(read_signals("timestamp,source,title\n".as_bytes())
            .unwrap()
            .is_empty());
    }
}
