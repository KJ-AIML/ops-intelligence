# Pilot Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This repo lives inside a Heli-Harness workspace: claim a write lease on the task before editing, stage files by name, never `git add .`.

**Goal:** Make the engine handoff-ready for the infra teammate's Grafana capture pilot: native Grafana webhook ingestion, one-command deployment, a CI gate, failed-signal visibility in the Operations View, and a runbook a non-author can follow.

**Architecture:** Grafana enters through a new source adapter crate that splits one notification batch into one RawSignal per alert at the ingestion boundary, preserving the whole payload; nothing downstream of RawSignal changes. The server serves the built React app from the same origin, so a pilot host runs two containers (server, PostgreSQL) and the worker as a one-shot tool. CI runs the README's existing checks plus the PostgreSQL acceptance tests.

**Tech Stack:** Rust 1.82+ workspace (axum 0.8, sqlx 0.8, tower-http 0.6, serde_json, chrono), React 19 + Vite 6 + TypeScript 5, PostgreSQL 17, Docker Compose, GitHub Actions.

**Spec:** `../../../../resources/operations-intelligence-product-tech-sheet-v0.1.md` sections 6 (v0.1 scope), 14 (webhook contract), 15 (Grafana integration), 20 (authentication), 24 (configuration), 26 step 3 (CI minimum), 31 Day 6 and Day 7, 33 (definition of done). Also `README.md` sections "Pilot Lab" and "Checks", and `docs/decisions/0002-event-family-derivation.md`.

## Global Constraints

- `crates/core` must not depend on axum, sqlx implementation details, any vendor SDK, or React (tech sheet 26 step 2). Adapters depend on core; core depends on nothing outside itself.
- Source adapters extract facts and hints only. Severity, state and event_family mapping happen in `crates/core/src/normalization` (decision 0002).
- The tenant is derived from the ingestion token or from server configuration, never from a request body (tech sheet 20).
- Raw payloads are stored verbatim and are never mutated after insert.
- `rust-version = "1.82"`, `edition = "2021"` (workspace `Cargo.toml`).
- Every task ends green on: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, `cargo test --workspace`.
- PostgreSQL acceptance tests are `#[ignore]` and run with `TEST_DATABASE_URL` pointing at a database whose name ends in `_test`.
- No `.env` files in the repo. The Heli hook blocks `.env`-shaped writes. Do not create `.env.example`.
- Stage files by name. Never `git add .`.
- Commit message style follows the existing log: `Handoff: <what changed>`.
- Line numbers in **Files** blocks were taken on 2026-09-08 before execution began and have drifted since. Match on the quoted content, never on the line number.
- Expected counts in manual checks assume an empty database unless the step says otherwise. The compose volume `ops-pgdata` is shared with local development and already holds the synthetic fixture plus Task 3 residue; isolate by source name where the step allows it.

## Task brief (Heli feature protocol)

- **Risk tier:** S1 for docs, packaging, CI. S2 for Task 3 (touches the ingest path and a port signature) and Task 5 (changes what the server serves at `/`).
- **Done criteria:** on a fresh machine with Docker, `docker compose up -d --build` starts the stack; a Grafana source can be created in the UI; a native Grafana test notification returns 202 and appears on the Sources page; the capture freezes into a dataset and replays twice with identical stats; failed signals show on the Overview page; CI is green; the runbook has been followed once end to end by someone other than the author.
- **Out of scope:** daily brief, Teams delivery, configurable correlation windows, suppression, Azure Monitor and Email adapters, moving mapping tables into `sources.config`, golden cases (P3), shadow mode (P4).
- **Verification per slice:** each task ends with the four workspace checks. Tasks 3 and 4 also run the ignored PostgreSQL tests. Tasks 3, 5 and 6 include a manual curl check whose expected output is written down.

## File map

| File | Responsibility | Task |
|---|---|---|
| `README.md` | truth fixes, Grafana section, Docker section, status table | 1, 3, 6, 9 |
| `adapters/sources/grafana/Cargo.toml` | new crate manifest | 2 |
| `adapters/sources/grafana/src/lib.rs` | batch splitting, fact extraction, `GrafanaNormalizer` | 2 |
| `adapters/sources/grafana/tests/fixtures/grafana-webhook-v1.json` | provisional Grafana payload from the docs | 2 |
| `adapters/sources/grafana/tests/normalization.rs` | fixture → RawSignal → Event | 2 |
| `Cargo.toml` (workspace) | new member, dependency alias, tower-http features | 2, 5 |
| `crates/core/src/ports.rs` | `create_webhook` takes a `SourceType` | 3 |
| `adapters/persistence/src/lib.rs` | bind the source type on insert | 3 |
| `apps/server/src/main.rs` | `source_type` on create, body limit, static UI | 3, 5 |
| `apps/server/src/webhook.rs` | per-source-type ingest branching | 3 |
| `apps/server/Cargo.toml`, `apps/worker/Cargo.toml` | depend on the new crate | 3 |
| `apps/worker/src/normalizers.rs` | Grafana dispatch arm | 3 |
| `web/src/api.ts`, `web/src/pages/Sources.tsx` | source type selector | 3 |
| `crates/core/src/insights.rs`, `adapters/persistence/src/product.rs`, `apps/server/src/dto.rs`, `web/src/api.ts`, `web/src/pages/Overview.tsx` | `failed_signals` | 4 |
| `adapters/persistence/tests/normalization.rs` | assert the failed count is visible | 4 |
| `Dockerfile`, `.dockerignore`, `docker-compose.yml` | packaging | 6 |
| `.github/workflows/ci.yml`, `scripts/check.sh` | CI gate | 7 |
| `docs/pilot-runbook.md`, `apps/worker/src/pilot.rs` | runbook; print the replay tenant slug | 8 |

---

### Task 1: Stop the README contradicting the code

**Files:**
- Modify: `README.md:12-30` (status table), `README.md:249-250` (capture comment), `README.md:302-309` (phases)

**Interfaces:**
- Consumes: nothing.
- Produces: phase numbering P1 Lab, P2 Real capture, P3 Golden cases, P4 Shadow mode. Later tasks and the runbook use this numbering.

- [ ] **Step 1: Replace the capture comment**

In the Pilot Lab code block, replace these two lines:

```sh
# 1. CAPTURE — point Grafana / Azure at a webhook source and let signals arrive.
#    No vendor adapter needed; the webhook endpoint already exists.
```

with:

```sh
# 1. CAPTURE — create a source of type `grafana` (or `generic_webhook`) in the UI and
#    point the Grafana contact point at its ingestion URL. See docs/pilot-runbook.md.
```

- [ ] **Step 2: Renumber the phases**

Replace the "Not built yet, deliberately" section body with:

```markdown
**Real capture (P2).** The first frozen dataset from live Grafana traffic on the
design partner's infrastructure. Everything below needs it. Plan:
`docs/plans/2026-09-08-pilot-handoff.md`.

**Golden cases (P3).** A dataset with human-approved expected groupings. The
system proposes, the engineer corrects, and *that* becomes the oracle. A model
must never write its own answer key.

**Shadow mode (P4).** Real signals flowing alongside the existing workflow, with
the capture tenant processed live so the UI shows incidents as they happen.
Needs P2 first.
```

- [ ] **Step 3: Update the status table rows for the phases**

Replace the two rows `P2 | Golden cases...` and `P4 | Shadow mode...` with:

```markdown
| P2 | Real capture: Grafana adapter, Docker deployment, CI, runbook | in progress |
| P3 | Golden cases + human-reviewed regression | not started |
| P4 | Shadow mode against live infrastructure | not started |
```

- [ ] **Step 4: Verify**

Run: `grep -n "No vendor adapter needed" README.md`
Expected: no output.

Run: `grep -n "^| P[0-9]" README.md`
Expected: four rows, P1 through P4, in order.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "Handoff: README stops claiming Grafana works without an adapter; phases renumbered"
```

---

### Task 2: Grafana source adapter crate

**Files:**
- Create: `adapters/sources/grafana/Cargo.toml`
- Create: `adapters/sources/grafana/src/lib.rs`
- Create: `adapters/sources/grafana/tests/fixtures/grafana-webhook-v1.json`
- Create: `adapters/sources/grafana/tests/normalization.rs`
- Modify: `Cargo.toml:3-13` (workspace members) and `Cargo.toml:21-26` (path dependencies)

**Interfaces:**
- Consumes: `ops_core::normalization::{normalize, FamilyHints, SourceFacts, SourceNormalizationConfig}`, `ops_core::ports::SignalNormalizer`, `ops_core::{DomainError, Event, RawSignal, SourceType}`.
- Produces:
  - `pub const CONTENT_TYPE: &str = "application/json"`
  - `pub const ORIGIN: &str = "grafana"`
  - `pub struct AlertSignal { pub external_id: String, pub payload: serde_json::Value }`
  - `pub fn split_batch(body: &Value) -> Result<Vec<AlertSignal>, DomainError>`
  - `pub fn extract_facts(payload: &Value) -> Result<SourceFacts, DomainError>`
  - `pub struct GrafanaNormalizer;` implementing `SignalNormalizer` for `SourceType::Grafana`.

Design decisions locked here:
- One Grafana POST is one notification group with an `alerts` array. The engine's evidence unit is one alert, so the batch is split at the door. Each stored payload is `{ "alert": <the alert object>, "group": <every top-level field except alerts> }`, so the original context is preserved on every signal and nothing is dropped.
- `external_id` is `fingerprint:status:startsAt`. Grafana repeats a firing notification on its repeat interval with the same fingerprint and startsAt, which must deduplicate. Its resolution has a different status, and a later re-fire has a different startsAt, which must not.
- A resolved alert occurred when it ended (`endsAt`); everything else occurred when it started. Grafana's "no end" sentinel is `0001-01-01T00:00:00Z`.
- Label keys for environment, service and resource are constants until real Grafana data says otherwise. They are marked with a `ponytail:` comment.
- The fixture is written from Grafana's documented webhook contact-point format. Replace it with a real payload from the design partner's Grafana as soon as one exists, and keep the real one as the regression fixture (tech sheet 35, P8-03).

- [ ] **Step 1: Create the crate manifest**

`adapters/sources/grafana/Cargo.toml`:

```toml
[package]
name = "ops-source-grafana"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
ops-core.workspace = true

chrono.workspace = true
serde_json.workspace = true
```

- [ ] **Step 2: Register the crate in the workspace**

In `Cargo.toml`, add `"adapters/sources/grafana",` to `members` after the webhook line, and add to `[workspace.dependencies]` after `ops-source-webhook`:

```toml
ops-source-grafana = { path = "adapters/sources/grafana" }
```

- [ ] **Step 3: Create the provisional fixture**

`adapters/sources/grafana/tests/fixtures/grafana-webhook-v1.json`:

```json
{
  "receiver": "ops-intelligence",
  "status": "firing",
  "orgId": 1,
  "alerts": [
    {
      "status": "firing",
      "labels": {
        "alertname": "HighAPILatency",
        "grafana_folder": "payments",
        "severity": "warning",
        "service": "payment-api",
        "instance": "api-prod-01",
        "environment": "production"
      },
      "annotations": {
        "summary": "p95 latency above 800ms on payment-api",
        "description": "p95 latency has been above 800ms for 5 minutes"
      },
      "startsAt": "2026-09-08T09:42:10Z",
      "endsAt": "0001-01-01T00:00:00Z",
      "generatorURL": "https://grafana.example.internal/alerting/grafana/abc123/view",
      "fingerprint": "3f2a9c1d8e7b6a50",
      "silenceURL": "https://grafana.example.internal/alerting/silence/new?alertmanager=grafana",
      "dashboardURL": "",
      "panelURL": "",
      "values": { "B": 912.4 },
      "valueString": "[ var='B' labels={} value=912.4 ]"
    },
    {
      "status": "firing",
      "labels": {
        "alertname": "DiskAlmostFull",
        "grafana_folder": "infra",
        "severity": "critical",
        "instance": "db-prod-02",
        "environment": "production"
      },
      "annotations": {
        "summary": "Disk usage above 90% on db-prod-02"
      },
      "startsAt": "2026-09-08T09:40:00Z",
      "endsAt": "0001-01-01T00:00:00Z",
      "generatorURL": "https://grafana.example.internal/alerting/grafana/def456/view",
      "fingerprint": "9b8c7d6e5f4a3210",
      "values": { "B": 91.7 },
      "valueString": "[ var='B' labels={} value=91.7 ]"
    }
  ],
  "groupLabels": { "grafana_folder": "payments" },
  "commonLabels": { "environment": "production" },
  "commonAnnotations": {},
  "externalURL": "https://grafana.example.internal/",
  "version": "1",
  "groupKey": "{}:{grafana_folder=\"payments\"}",
  "truncatedAlerts": 0,
  "title": "[FIRING:2]  (payments production)",
  "state": "alerting",
  "message": "**Firing**\n\nValue: B=912.4\nLabels:\n - alertname = HighAPILatency\n"
}
```

- [ ] **Step 4: Write the unit tests against stub signatures**

Create `adapters/sources/grafana/src/lib.rs` with the public items as `todo!()` stubs and the test module. The stubs:

```rust
//! Grafana Alerting source adapter (tech sheet 15).
//!
//! Grafana's webhook contact point posts one JSON body per notification group,
//! carrying an `alerts` array. The engine's evidence unit is one RawSignal per
//! alert, so the batch is split at the ingestion boundary and every alert keeps
//! the group-level context it arrived with. Like every source adapter, this
//! crate extracts facts and hints only; severity, state and family mapping stay
//! in core normalization (decision 0002).

use chrono::{DateTime, Utc};
use ops_core::normalization::{normalize, FamilyHints, SourceFacts, SourceNormalizationConfig};
use ops_core::ports::SignalNormalizer;
use ops_core::{DomainError, Event, RawSignal, SourceType};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const CONTENT_TYPE: &str = "application/json";
pub const ORIGIN: &str = "grafana";
/// Grafana's "this alert has not ended" sentinel.
const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// One alert lifted out of a Grafana notification batch, ready to become a RawSignal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertSignal {
    /// `fingerprint:status:startsAt`. Identical across Grafana's repeat
    /// notifications of one firing alert; distinct for its resolution and for
    /// a later re-fire.
    pub external_id: String,
    /// `{ "alert": <alert object>, "group": <every top-level field except alerts> }`.
    pub payload: Value,
}

pub fn split_batch(body: &Value) -> Result<Vec<AlertSignal>, DomainError> {
    todo!()
}

pub fn extract_facts(payload: &Value) -> Result<SourceFacts, DomainError> {
    todo!()
}

pub struct GrafanaNormalizer;

impl SignalNormalizer for GrafanaNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        todo!()
    }
}
```

The test module, appended to the same file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn batch() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/grafana-webhook-v1.json")).unwrap()
    }

    #[test]
    fn splits_a_batch_into_one_signal_per_alert_with_stable_ids() {
        let signals = split_batch(&batch()).unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(
            signals[0].external_id,
            "3f2a9c1d8e7b6a50:firing:2026-09-08T09:42:10Z"
        );
        assert_eq!(
            signals[1].external_id,
            "9b8c7d6e5f4a3210:firing:2026-09-08T09:40:00Z"
        );
    }

    #[test]
    fn every_alert_keeps_the_group_context_it_arrived_with() {
        for signal in split_batch(&batch()).unwrap() {
            assert_eq!(signal.payload["group"]["receiver"], "ops-intelligence");
            assert_eq!(
                signal.payload["group"]["groupKey"],
                "{}:{grafana_folder=\"payments\"}"
            );
            assert!(
                signal.payload["group"].get("alerts").is_none(),
                "the alerts array must not be nested inside every alert"
            );
            assert!(signal.payload["alert"].is_object());
        }
    }

    #[test]
    fn a_repeat_notification_of_the_same_firing_alert_has_the_same_id() {
        let first = split_batch(&batch()).unwrap();
        let again = split_batch(&batch()).unwrap();
        assert_eq!(first[0].external_id, again[0].external_id);
    }

    #[test]
    fn resolution_and_refire_get_distinct_ids() {
        let mut resolved = batch();
        resolved["alerts"][0]["status"] = json!("resolved");
        resolved["alerts"][0]["endsAt"] = json!("2026-09-08T10:05:00Z");
        let mut refired = batch();
        refired["alerts"][0]["startsAt"] = json!("2026-09-08T11:00:00Z");
        let ids: Vec<String> = [batch(), resolved, refired]
            .iter()
            .map(|b| split_batch(b).unwrap()[0].external_id.clone())
            .collect();
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn facts_come_from_the_alert_not_the_group() {
        let signal = split_batch(&batch()).unwrap().remove(0);
        let facts = extract_facts(&signal.payload).unwrap();
        assert_eq!(facts.origin, ORIGIN);
        assert_eq!(facts.title, "p95 latency above 800ms on payment-api");
        assert_eq!(
            facts.message.as_deref(),
            Some("p95 latency has been above 800ms for 5 minutes")
        );
        assert_eq!(facts.service.as_deref(), Some("payment-api"));
        assert_eq!(facts.resource.as_deref(), Some("api-prod-01"));
        assert_eq!(facts.environment.as_deref(), Some("production"));
        assert_eq!(facts.severity_raw.as_deref(), Some("warning"));
        assert_eq!(facts.state_raw.as_deref(), Some("firing"));
        assert_eq!(facts.occurred_at.to_rfc3339(), "2026-09-08T09:42:10+00:00");
        assert_eq!(facts.external_id.as_deref(), Some(signal.external_id.as_str()));
        assert_eq!(facts.hints.metric_name.as_deref(), Some("HighAPILatency"));
    }

    #[test]
    fn a_resolved_alert_occurs_when_it_ended() {
        let mut body = batch();
        body["alerts"][0]["status"] = json!("resolved");
        body["alerts"][0]["endsAt"] = json!("2026-09-08T10:05:00Z");
        let signal = split_batch(&body).unwrap().remove(0);
        let facts = extract_facts(&signal.payload).unwrap();
        assert_eq!(facts.state_raw.as_deref(), Some("resolved"));
        assert_eq!(facts.occurred_at.to_rfc3339(), "2026-09-08T10:05:00+00:00");
    }

    #[test]
    fn title_falls_back_from_summary_to_alertname() {
        let mut body = batch();
        body["alerts"][1]["annotations"] = json!({});
        let signal = split_batch(&body).unwrap().remove(1);
        assert_eq!(extract_facts(&signal.payload).unwrap().title, "DiskAlmostFull");
    }

    #[test]
    fn malformed_batches_are_rejected_at_the_door() {
        assert!(split_batch(&json!([])).is_err());
        assert!(split_batch(&json!({ "alerts": [] })).is_err());
        assert!(split_batch(&json!({ "status": "firing" })).is_err());

        let mut no_fingerprint = batch();
        no_fingerprint["alerts"][0]
            .as_object_mut()
            .unwrap()
            .remove("fingerprint");
        assert!(split_batch(&no_fingerprint).is_err());

        let mut bad_time = batch();
        bad_time["alerts"][0]["startsAt"] = json!("yesterday");
        let signal = split_batch(&bad_time).unwrap().remove(0);
        let err = extract_facts(&signal.payload).unwrap_err().to_string();
        assert!(err.contains("RFC3339"), "got: {err}");
    }
}
```

- [ ] **Step 5: Run the tests and watch them fail**

Run: `cargo test -p ops-source-grafana`
Expected: every test panics with `not yet implemented`.

- [ ] **Step 6: Replace the stubs with the implementation**

Replace the three `todo!()` bodies with:

```rust
pub fn split_batch(body: &Value) -> Result<Vec<AlertSignal>, DomainError> {
    let object = body
        .as_object()
        .ok_or_else(|| DomainError::Validation("grafana payload must be a JSON object".into()))?;
    let alerts = match object.get("alerts") {
        Some(Value::Array(alerts)) if !alerts.is_empty() => alerts,
        _ => {
            return Err(DomainError::Validation(
                "grafana payload must carry a non-empty alerts array".into(),
            ))
        }
    };
    // Everything Grafana said about the group travels with each alert, so the
    // stored evidence is complete without the other alerts in the batch.
    let group: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| key.as_str() != "alerts")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    alerts
        .iter()
        .map(|alert| {
            let fingerprint = required_str(alert, "fingerprint")?;
            let status = required_str(alert, "status")?;
            let starts_at = required_str(alert, "startsAt")?;
            Ok(AlertSignal {
                external_id: format!("{fingerprint}:{status}:{starts_at}"),
                payload: serde_json::json!({
                    "alert": alert,
                    "group": Value::Object(group.clone()),
                }),
            })
        })
        .collect()
}

fn required_str<'a>(alert: &'a Value, key: &str) -> Result<&'a str, DomainError> {
    alert
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DomainError::Validation(format!("grafana alert is missing {key}")))
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| DomainError::Validation(format!("grafana time {s:?} is not RFC3339")))
}

fn first_label<'a>(labels: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| labels.get(*key).map(String::as_str))
}

pub fn extract_facts(payload: &Value) -> Result<SourceFacts, DomainError> {
    let alert = payload
        .get("alert")
        .filter(|a| a.is_object())
        .ok_or_else(|| {
            DomainError::Validation("grafana signal payload must carry an alert object".into())
        })?;
    let group = payload.get("group").cloned().unwrap_or(Value::Null);

    let fingerprint = required_str(alert, "fingerprint")?;
    let status = required_str(alert, "status")?;
    let starts_at_raw = required_str(alert, "startsAt")?;
    let starts_at = parse_time(starts_at_raw)?;
    let ends_at = alert
        .get("endsAt")
        .and_then(Value::as_str)
        .filter(|s| *s != ZERO_TIME)
        .map(parse_time)
        .transpose()?;
    // A resolution happened when the alert ended; everything else happened
    // when it started. Never ingest time.
    let occurred_at = match (status, ends_at) {
        ("resolved", Some(ended)) => ended,
        _ => starts_at,
    };

    let labels: BTreeMap<String, String> = alert
        .get("labels")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default();
    let annotation = |key: &str| -> Option<String> {
        alert
            .get("annotations")
            .and_then(|a| a.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };

    let title = annotation("summary")
        .or_else(|| labels.get("alertname").cloned())
        .or_else(|| group.get("title").and_then(Value::as_str).map(str::to_owned))
        .ok_or_else(|| {
            DomainError::Validation(
                "grafana alert has neither a summary annotation nor an alertname".into(),
            )
        })?;
    let message = annotation("description");

    // ponytail: which label carries environment/service/resource is a constant
    // until real Grafana data says otherwise; move it to sources.config with
    // the other per-source mappings.
    let environment = first_label(&labels, &["environment", "env"]).map(str::to_owned);
    let service = first_label(&labels, &["service", "job", "app"]).map(str::to_owned);
    let resource =
        first_label(&labels, &["instance", "host", "resource", "pod"]).map(str::to_owned);

    let hints = FamilyHints {
        title: title.clone(),
        message: message.clone(),
        labels: labels.clone(),
        metric_name: labels.get("alertname").cloned(),
        monitor_type: None,
        vendor_category: None,
    };

    Ok(SourceFacts {
        origin: ORIGIN.to_string(),
        occurred_at,
        title,
        message,
        environment,
        service,
        resource,
        severity_raw: labels.get("severity").cloned(),
        state_raw: Some(status.to_string()),
        external_id: Some(format!("{fingerprint}:{status}:{starts_at_raw}")),
        labels,
        hints,
    })
}

impl SignalNormalizer for GrafanaNormalizer {
    fn normalize(
        &self,
        raw: &RawSignal,
        source_type: SourceType,
        created_at: DateTime<Utc>,
    ) -> Result<Event, DomainError> {
        if source_type != SourceType::Grafana || raw.content_type != CONTENT_TYPE {
            return Err(DomainError::Source(
                "unsupported normalization source/content type".into(),
            ));
        }
        let facts = extract_facts(&raw.payload)?;
        if facts.external_id != raw.external_id {
            return Err(DomainError::Validation(
                "payload external_id differs from RawSignal".into(),
            ));
        }
        let config = SourceNormalizationConfig::for_origin(&facts.origin);
        normalize(
            raw.organization_id,
            raw.source_id,
            raw.id,
            &facts,
            &config,
            created_at,
        )
    }
}
```

- [ ] **Step 7: Run the unit tests**

Run: `cargo test -p ops-source-grafana`
Expected: 8 passed.

- [ ] **Step 8: Write the end-to-end normalization test**

`adapters/sources/grafana/tests/normalization.rs`:

```rust
use chrono::{DateTime, Utc};
use ops_core::domains::events::{EventFamily, EventState, Severity};
use ops_core::ports::SignalNormalizer;
use ops_core::{OrganizationId, RawSignal, SourceId, SourceType};
use ops_source_grafana::{split_batch, GrafanaNormalizer, CONTENT_TYPE};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/grafana-webhook-v1.json")).unwrap()
}

#[test]
fn a_native_grafana_notification_becomes_canonical_events() {
    let org = OrganizationId::new();
    let source = SourceId::new();
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();

    let raws: Vec<RawSignal> = split_batch(&fixture())
        .unwrap()
        .into_iter()
        .map(|s| RawSignal::received(org, source, Some(s.external_id), CONTENT_TYPE, s.payload, now))
        .collect();

    let latency = GrafanaNormalizer
        .normalize(&raws[0], SourceType::Grafana, now)
        .unwrap();
    assert_eq!(latency.event_family, EventFamily::Latency);
    assert_eq!(latency.severity, Severity::Warning);
    assert_eq!(latency.state, EventState::Firing);
    assert_eq!(latency.service.as_deref(), Some("payment-api"));
    assert_eq!(latency.resource.as_deref(), Some("api-prod-01"));
    assert_eq!(latency.environment.as_deref(), Some("production"));
    assert_eq!(latency.occurred_at.to_rfc3339(), "2026-09-08T09:42:10+00:00");
    assert_ne!(latency.occurred_at, raws[0].received_at, "never ingest time");
    assert_eq!(latency.raw_signal_id, raws[0].id);
    assert_eq!(latency.organization_id, org);
    assert_eq!(latency.created_at, now);

    let disk = GrafanaNormalizer
        .normalize(&raws[1], SourceType::Grafana, now)
        .unwrap();
    assert_eq!(disk.severity, Severity::Critical);
    assert_eq!(disk.state, EventState::Firing);
    assert_eq!(disk.resource.as_deref(), Some("db-prod-02"));
    assert!(disk.service.is_none(), "no service label means no service, not a guess");
}

#[test]
fn the_wrong_source_type_is_refused() {
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    let signal = split_batch(&fixture()).unwrap().remove(0);
    let raw = RawSignal::received(
        OrganizationId::new(),
        SourceId::new(),
        Some(signal.external_id),
        CONTENT_TYPE,
        signal.payload,
        now,
    );
    assert!(GrafanaNormalizer
        .normalize(&raw, SourceType::GenericWebhook, now)
        .is_err());
}
```

- [ ] **Step 9: Run everything for the crate**

Run: `cargo test -p ops-source-grafana`
Expected: 8 unit tests and 2 integration tests pass.

- [ ] **Step 10: Workspace checks**

Run:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

Expected: all green. If clippy complains about `map(|key| labels.get(*key)...)`, apply its suggestion; do not silence it.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock adapters/sources/grafana
git commit -m "Handoff: Grafana source adapter splits a notification batch into one RawSignal per alert"
```

---

### Task 3: Wire Grafana into ingestion, source creation, the worker and the UI

**Files:**
- Modify: `crates/core/src/ports.rs:70-80` (`create_webhook`)
- Modify: `adapters/persistence/src/lib.rs:168-195` (`create_webhook`)
- Modify: `apps/server/src/main.rs:20-40` (imports, `MAX_BODY_BYTES`), `apps/server/src/main.rs:338-366` (`CreateSource`, `create_source`)
- Modify: `apps/server/src/webhook.rs:29-83` (`ingest`)
- Modify: `apps/server/Cargo.toml`, `apps/worker/Cargo.toml` (add `ops-source-grafana.workspace = true`)
- Modify: `apps/worker/src/normalizers.rs:20-36`
- Modify: `web/src/api.ts` (`createSource`), `web/src/pages/Sources.tsx:1-60`
- Modify: `README.md` "Generic Webhook" section (add a Grafana subsection)

**Interfaces:**
- Consumes: `ops_source_grafana::{split_batch, CONTENT_TYPE, GrafanaNormalizer}` from Task 2.
- Produces: `SourceRepository::create_webhook(&self, organization_id, source_type: SourceType, name, token)`; `POST /api/v1/sources` accepts `{ "name": string, "source_type": "generic_webhook" | "grafana" }` with `generic_webhook` as the default; `POST /api/v1/ingest/webhook/{token}` returns `{ accepted, inserted, duplicates, duplicate, signal_id }`.

- [ ] **Step 1: Widen the port**

In `crates/core/src/ports.rs`, change the `create_webhook` signature to:

```rust
    /// Create a webhook-fed source (`GenericWebhook` or `Grafana`) and return
    /// it together with its freshly minted token. The token is returned
    /// exactly once, here; it is never readable again from a list or detail
    /// endpoint.
    async fn create_webhook(
        &self,
        organization_id: OrganizationId,
        source_type: SourceType,
        name: &str,
        token: &str,
    ) -> Result<Source, DomainError>;
```

- [ ] **Step 2: Bind the type in persistence**

In `adapters/persistence/src/lib.rs`, change `create_webhook` to:

```rust
    async fn create_webhook(
        &self,
        organization_id: OrganizationId,
        source_type: SourceType,
        name: &str,
        token: &str,
    ) -> Result<Source, DomainError> {
        // No upsert here: silently handing back an existing source's row would
        // make the caller believe the token it just generated is live.
        let row = sqlx::query(&format!(
            "INSERT INTO sources (id, organization_id, source_type, name, ingest_token)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING {SOURCE_COLUMNS}"
        ))
        .bind(SourceId::new().as_uuid())
        .bind(organization_id.as_uuid())
        .bind(source_type.as_str())
        .bind(name)
        .bind(token)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DomainError::Validation(format!("a source named {name:?} already exists"))
            }
            _ => persistence(e),
        })?;
        source_from_row(&row)
    }
```

`SourceType` is already imported in that file (it is used by `ensure`). Run `cargo build -p ops-persistence` to confirm; add `use ops_core::SourceType;` only if the compiler asks.

- [ ] **Step 3: Server dependencies and constants**

In `apps/server/Cargo.toml` add under `ops-source-webhook.workspace = true`:

```toml
ops-source-grafana.workspace = true
```

In `apps/server/src/main.rs` add the import:

```rust
use ops_core::domains::sources::SourceType;
```

and change the body limit:

```rust
/// Alert payloads are small, but Grafana posts one body per notification group
/// and a large group with values and annotations can pass 256 KiB. A cap still
/// keeps a misconfigured source from exhausting memory (tech sheet 21).
const MAX_BODY_BYTES: usize = 1024 * 1024;
```

- [ ] **Step 4: Source creation accepts a type**

Replace the `CreateSource` struct and `create_source` handler in `apps/server/src/main.rs` with:

```rust
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
```

- [ ] **Step 5: Ingestion branches on the source type**

Replace the `ingest` function in `apps/server/src/webhook.rs` with:

```rust
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

    // Grafana reports how many alerts it dropped from a group. Nothing here can
    // recover them, but their absence must not be silent (product principle P3).
    let truncated = payload
        .get("truncatedAlerts")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Validate before persisting so a malformed body is rejected at the door
    // rather than becoming a signal that can only ever fail normalization.
    let (content_type, drafts): (&str, Vec<(Option<String>, Value)>) = match source.source_type {
        SourceType::Grafana => (
            ops_source_grafana::CONTENT_TYPE,
            ops_source_grafana::split_batch(&payload)?
                .into_iter()
                .map(|alert| (Some(alert.external_id), alert.payload))
                .collect(),
        ),
        _ => {
            ops_source_webhook::extract_facts(&payload)?;
            (
                ops_source_webhook::CONTENT_TYPE,
                vec![(ops_source_webhook::external_id_of(&payload), payload)],
            )
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
    if truncated > 0 {
        tracing::warn!(
            source = %source.name,
            truncated,
            "grafana dropped alerts from this notification group"
        );
    }

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
```

- [ ] **Step 6: Worker dispatch**

In `apps/worker/Cargo.toml` add `ops-source-grafana.workspace = true` next to the other source adapters. In `apps/worker/src/normalizers.rs` add an arm before the `other =>` arm:

```rust
            SourceType::Grafana => {
                ops_source_grafana::GrafanaNormalizer.normalize(raw, source_type, created_at)
            }
```

and change the module doc's "Adding Grafana or Azure Monitor later" to "Adding Azure Monitor later".

- [ ] **Step 7: Build and run the existing suites**

Run: `cargo build --workspace && cargo test --workspace`
Expected: green. The only caller of `create_webhook` is the server, so nothing else changes.

Run the ignored PostgreSQL tests (they use `ensure`, not `create_webhook`, and call the CSV normalizer directly, so they are unaffected; this run proves it):

```sh
export TEST_DATABASE_URL=postgres://ops:ops_local_dev@localhost:55432/ops_normalization_test
cargo test -p ops-persistence -- --ignored
```

Expected: 3 passed.

- [ ] **Step 8: UI can create a Grafana source**

In `web/src/api.ts` add after the `Severity` type:

```ts
export type WebhookSourceType = "generic_webhook" | "grafana";
```

and replace `createSource` with:

```ts
  createSource: (name: string, sourceType: WebhookSourceType) =>
    request<CreatedSource>("/sources", {
      method: "POST",
      body: JSON.stringify({ name, source_type: sourceType }),
    }),
```

In `web/src/pages/Sources.tsx`:

Change the type import to:

```ts
import type { CreatedSource, WebhookSourceType } from "../api";
```

After `const [name, setName] = useState("");` add:

```ts
  const [sourceType, setSourceType] = useState<WebhookSourceType>("grafana");
```

Replace the `create` mutation with:

```ts
  const create = useMutation({
    mutationFn: (input: { name: string; sourceType: WebhookSourceType }) =>
      api.createSource(input.name, input.sourceType),
    onSuccess: (source) => {
      // The token is readable exactly once, right here. Hold it in component
      // state so the engineer can copy it before it becomes unrecoverable.
      setCreated(source);
      setName("");
      refresh();
    },
  });
  const submit = () => create.mutate({ name: name.trim(), sourceType });
```

Replace the `<div className="toolbar">...</div>` block with:

```tsx
      <div className="toolbar">
        <select
          value={sourceType}
          onChange={(e) => setSourceType(e.target.value as WebhookSourceType)}
        >
          <option value="grafana">Grafana contact point</option>
          <option value="generic_webhook">Generic webhook</option>
        </select>
        <input
          placeholder="new source name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && name.trim()) submit();
          }}
        />
        <button className="primary" onClick={submit} disabled={!name.trim() || create.isPending}>
          Add source
        </button>
      </div>
```

Immediately before the `<pre>{created.ingest_url}</pre>` line add:

```tsx
            {created.source_type === "grafana" && (
              <p className="small muted">
                In Grafana: Alerting → Contact points → New → integration "Webhook" → this URL,
                HTTP method POST. Route alerts to it with a nested notification policy that
                continues matching, so existing notifications keep flowing. Step by step in
                docs/pilot-runbook.md.
              </p>
            )}
```

Run: `cd web && npm run typecheck`
Expected: no errors.

- [ ] **Step 9: Manual end-to-end check**

With the compose database up and the server running (`cargo run -p ops-server`), in a second shell:

```sh
curl -s -X POST localhost:8080/api/v1/sources -H 'content-type: application/json' \
  -d '{"name":"grafana-live","source_type":"grafana"}'
```

Expected: `201` with `"source_type":"grafana"` and an `ingest_url`. Export the token from the URL as `TOKEN`, then:

```sh
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8080/api/v1/ingest/webhook/$TOKEN \
  -H 'content-type: application/json' \
  --data-binary @adapters/sources/grafana/tests/fixtures/grafana-webhook-v1.json
```

Expected: `202`. Run the same command again. Expected: `200`, and the body has `"duplicates":2`.

```sh
cargo run -p ops-worker -- process
cargo run -p ops-worker -- correlate
```

Expected: `process` reports 2 processed, 0 failed. `correlate` reports 2 incidents, both open. These counts assume an empty database; against the shared dev database they include the synthetic backlog, so isolate the two Grafana incidents by source. Open `http://localhost:5173/incidents` (Vite dev server) and confirm both incidents are listed with the raw Grafana alert visible in the incident detail evidence. If the executor cannot drive a browser, say so in the report and hand the click-through to a person; the curl checks alone prove the data, not the rendering.

- [ ] **Step 10: Document it**

In `README.md`, after the Generic Webhook section, add:

````markdown
### Grafana

A source of type `grafana` accepts Grafana Alerting's native webhook contact-point
payload. One POST carries a notification group; every alert in it becomes its own
RawSignal with the group context attached, and `fingerprint:status:startsAt` is the
idempotency key, so Grafana's repeat notifications collapse while resolutions and
re-fires do not.

```sh
curl -X POST localhost:8080/api/v1/sources -H 'content-type: application/json' \
     -d '{"name":"grafana-live","source_type":"grafana"}'
```

Point a Grafana Webhook contact point at the returned `ingest_url` (method POST).
The response reports `inserted` and `duplicates` per batch. The fixture in
`adapters/sources/grafana/tests/fixtures/` is written from Grafana's documented
format; replace it with a real payload from the pilot environment when one exists.
````

- [ ] **Step 11: Checks and commit**

Run the four workspace checks plus `cd web && npm run typecheck`.

```bash
git add Cargo.lock crates/core/src/ports.rs adapters/persistence/src/lib.rs \
  apps/server/Cargo.toml apps/server/src/main.rs apps/server/src/webhook.rs \
  apps/worker/Cargo.toml apps/worker/src/normalizers.rs \
  web/src/api.ts web/src/pages/Sources.tsx README.md
git commit -m "Handoff: Grafana sources can be created and ingested natively; worker dispatches to the adapter"
```

---

### Task 3b: Grafana batches: drop the rendered digest, log rejections, make dispatch exhaustive

Added after the audit of commits 8af688d and b712ac2. Do this before Task 4.

**Why this task exists.** b712ac2 bounds `group_size × alert_count` at 32 MiB on the stated assumption that a real group is "a few KB at most". It is not. Grafana's top-level `message` is the rendered digest of every alert in the batch, so group size grows with the alert count and the product grows with its square. At roughly 600 bytes of digest per alert, a group of about 236 alerts crosses the budget while its body is well under 1 MiB. A fleet-wide outage notification would therefore be rejected, and rejected silently on our side because validation errors are not logged. That is the loss this product exists to prevent (principle P3). The fix is to stop duplicating the digest, which is fully derivable from `alerts`, and to log any batch the adapter refuses.

**Files:**
- Modify: `adapters/sources/grafana/src/lib.rs` (`split_batch` group filter, the doc comment on `AlertSignal.payload`, the comments on both `MAX_*` constants, tests)
- Modify: `apps/server/src/webhook.rs` (`ingest`: the `match source.source_type` block and the `truncated` handling)
- Modify: `README.md` "Grafana" section

**Interfaces:**
- Unchanged signatures. `AlertSignal.payload.group` no longer carries `message`. `title` stays: it is short and `extract_facts` uses it as the last title fallback.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `adapters/sources/grafana/src/lib.rs`, next to `a_realistic_batch_stays_within_the_expansion_budget`, add:

```rust
    #[test]
    fn the_rendered_digest_is_not_duplicated_into_every_alert() {
        let mut body = batch();
        body["message"] = json!("x".repeat(600 * MAX_ALERTS_PER_BATCH));
        for signal in split_batch(&body).unwrap() {
            assert!(
                signal.payload["group"].get("message").is_none(),
                "message is a rendering of alerts[], not evidence"
            );
            assert_eq!(
                signal.payload["group"]["title"],
                "[FIRING:2]  (payments production)"
            );
        }
    }

    #[test]
    fn a_full_cap_batch_with_a_grafana_sized_digest_is_accepted() {
        // Grafana's default template renders roughly 600 bytes per alert into
        // `message`, so a 500-alert group carries a digest of about 300 KB.
        // Multiplied across 500 alerts that would be 150 MB; it must not trip
        // the budget, because the digest is not stored.
        let mut body = batch_of(MAX_ALERTS_PER_BATCH);
        body["message"] = json!("x".repeat(600 * MAX_ALERTS_PER_BATCH));
        assert!(split_batch(&body).is_ok());
    }
```

Run: `cargo test -p ops-source-grafana`
Expected: both new tests fail; the first because `message` is present, the second with `expansion budget` in the error.

- [ ] **Step 2: Exclude the digest**

In `split_batch`, replace the group construction and its comment with:

```rust
    // Everything Grafana said about the group travels with each alert, so the
    // stored evidence is complete without the other alerts in the batch. The
    // one exception is `message`: Grafana renders every alert of the batch
    // into it, so it grows with the alert count and is fully derivable from
    // `alerts`. Duplicating it into each alert would make storage quadratic in
    // the group size for no evidence gain.
    let group: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "alerts" | "message"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
```

Change the `AlertSignal.payload` doc comment to:

```rust
    /// `{ "alert": <alert object>, "group": <every top-level field except alerts and message> }`.
```

Replace the comment block above `MAX_ALERTS_PER_BATCH` with:

```rust
// ponytail: deliberate bound, not derived from any Grafana limit. Grafana
// groups by alertname and folder by default, so a fleet-wide rule firing on
// hundreds of hosts arrives as ONE group; 500 is above any fleet a one-to-three
// person infra team runs while still bounding how many payloads one POST can
// become. Revisit with real numbers once a design partner's Grafana is wired
// up (tech sheet 35).
```

Replace the comment block above `MAX_BATCH_EXPANSION_BYTES` with:

```rust
// `split_batch` clones the group into every alert's payload, so the count cap
// above bounds how many clones happen but not their combined size. With the
// rendered `message` digest excluded, a real group is labels, annotations, a
// title and a few URLs: a few KB regardless of alert count, so a full-cap
// batch multiplies out to a few MB. 32 MiB is a safety net for bodies that are
// not shaped like Grafana's, not a limit a real notification should ever meet.
// If the server log ever shows "grafana batch rejected" for a real group, the
// model above is wrong and this needs real numbers, not a bigger constant.
const MAX_BATCH_EXPANSION_BYTES: usize = 32 * 1024 * 1024;
```

Run: `cargo test -p ops-source-grafana`
Expected: all pass, including the two b712ac2 tests, which pad a field other than `message`.

- [ ] **Step 3: Log rejections, move the truncation check, make dispatch exhaustive**

In `apps/server/src/webhook.rs`, delete the `let truncated = ...` block that sits before the match, delete the `if truncated > 0 { ... }` block after `touch_last_seen`, and replace the whole `let (content_type, drafts) ... = match source.source_type { ... };` with:

```rust
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
            ops_source_webhook::extract_facts(&payload)?;
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
```

Run: `cargo build -p ops-server && cargo clippy -p ops-server --all-targets -- -D warnings`
Expected: clean. If clippy flags the unreachable arm, keep it and add `#[allow(unreachable_code)]` only on that arm; the exhaustiveness is the point.

- [ ] **Step 4: Document**

In the README "Grafana" section, after the sentence ending "re-fires do not.", add:

```markdown
Grafana's rendered `message` digest is not stored; it is a rendering of `alerts[]`,
which is stored in full. A group with more than 500 alerts, or one whose stored
size would exceed 32 MiB, is refused and logged as `grafana batch rejected`.
```

- [ ] **Step 5: Manual check against the live handler**

With the server running and `TOKEN` from a Grafana source:

```sh
python - <<'EOF'
import json
body = json.load(open("adapters/sources/grafana/tests/fixtures/grafana-webhook-v1.json"))
alert = body["alerts"][0]
body["alerts"] = [dict(alert, fingerprint=f"fp{i:04d}") for i in range(400)]
body["message"] = "x" * (600 * 400)
json.dump(body, open("/tmp/big.json", "w"))
EOF
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8080/api/v1/ingest/webhook/$TOKEN \
  -H 'content-type: application/json' --data-binary @/tmp/big.json
```

Expected: `202`, and the response body reports `"inserted":400`. Before this task the same request returned `400` with `expansion budget` in the error.

- [ ] **Step 6: Checks and commit**

Run the four workspace checks.

```bash
git add adapters/sources/grafana/src/lib.rs apps/server/src/webhook.rs README.md
git commit -m "Handoff: Grafana digest is not duplicated per alert; refused batches are logged; webhook dispatch is exhaustive"
```

---

### Task 3c: No alert-count cap; log generic-webhook refusals; pin truncatedAlerts

Added after the audit of 937fb42. Do this before Task 4.

**Ruling on the 500-alert cap: remove it.** After Task 3b there are three bounds on one Grafana POST: the 1 MiB body limit, the 500-alert count cap, and the 32 MiB group-times-alerts expansion budget. Work out what each one stops. A realistic alert is 800 bytes to 1.5 KB, so a 1 MiB body holds roughly 700 to 1,300 of them; the body limit already bounds a real batch. An adversarial body of minimal 80-byte alerts holds about 14,000, which the expansion budget accepts (14,000 times a few hundred bytes of group is under 3 MB) and which costs 14,000 inserts on one request: slow, bounded, and only reachable with a valid 32-character token. The count cap therefore stops nothing the other two bounds do not, and the only real traffic it ever refuses is a fleet-wide outage of 500 to 1,300 hosts, whole, with a 400. That is the one notification the engine must not lose. Deletion over addition.

**Files:**
- Modify: `adapters/sources/grafana/src/lib.rs` (`MAX_ALERTS_PER_BATCH` and its check, the `MAX_BATCH_EXPANSION_BYTES` comment, tests)
- Modify: `apps/server/src/webhook.rs` (`SourceType::GenericWebhook` arm)
- Modify: `README.md` "Grafana" section
- Modify: this plan, Task 8 runbook section 9 (already updated; the runbook is not written yet)

**Interfaces:** unchanged.

- [ ] **Step 1: Write the failing test**

In the `tests` module of `adapters/sources/grafana/src/lib.rs`, replace the whole test `a_batch_at_the_cap_is_accepted_but_one_over_is_rejected` with:

```rust
    #[test]
    fn a_fleet_wide_group_is_accepted_whole() {
        // 1,200 alerts is about what a 1 MiB body holds at realistic alert
        // sizes. A fleet-wide outage must arrive whole, not be refused for
        // being large: it is the one notification that must not be lost.
        let signals = split_batch(&batch_of(1_200)).unwrap();
        assert_eq!(signals.len(), 1_200);
        assert_eq!(
            signals[1_199].external_id,
            "fp-1199:firing:2026-09-08T09:42:10Z"
        );
    }
```

Also add, inside `every_alert_keeps_the_group_context_it_arrived_with`, after the `assert!(signal.payload["alert"].is_object());` line:

```rust
            // Load-bearing: the argument that dropping `message` loses nothing
            // relies on Grafana truncating alerts[] BEFORE rendering the digest,
            // and on the truncation count surviving into stored evidence.
            assert_eq!(signal.payload["group"]["truncatedAlerts"], 0);
```

Run: `cargo test -p ops-source-grafana`
Expected: `a_fleet_wide_group_is_accepted_whole` fails with `exceeding the cap`; the truncation assertion passes.

- [ ] **Step 2: Remove the cap**

Delete the `const MAX_ALERTS_PER_BATCH: usize = 500;` line together with its whole `// ponytail: deliberate bound ...` comment block, and delete this check inside `split_batch`:

```rust
    if alerts.len() > MAX_ALERTS_PER_BATCH {
        return Err(DomainError::Validation(format!(
            "grafana payload carries {} alerts, exceeding the cap of {MAX_ALERTS_PER_BATCH}",
            alerts.len()
        )));
    }
```

Replace the comment block above `MAX_BATCH_EXPANSION_BYTES` with:

```rust
// There is deliberately no alert-count cap. The request body limit (1 MiB,
// apps/server) bounds how many alerts one POST can carry, and this budget
// bounds how much they can become once the group is attached to each. A count
// cap on top of those only ever refused real fleet-wide outages whole, which
// is the one notification that must not be lost.
//
// `split_batch` clones the group into every alert's payload. With the rendered
// `message` digest excluded, a real group is labels, annotations, a title and
// a few URLs: a few KB regardless of alert count, so even a thousand-alert
// batch multiplies out to a few MB. 32 MiB is a safety net for bodies that are
// not shaped like Grafana's, not a limit a real notification should ever meet.
// If the server log ever shows "grafana batch rejected" for a real group, the
// model above is wrong and this needs real numbers, not a bigger constant.
const MAX_BATCH_EXPANSION_BYTES: usize = 32 * 1024 * 1024;
```

In the `tests` module, add near the top, after `use serde_json::json;`:

```rust
    /// A batch the size of a large real notification group. Not a limit;
    /// a probe size shared by the expansion and digest tests.
    const FULL_BATCH: usize = 500;
```

Then replace every remaining `MAX_ALERTS_PER_BATCH` in the tests module with `FULL_BATCH` (the `batch_of` and `batch_with_padded_group` doc comments, `a_batch_whose_group_times_alert_count_exceeds_the_expansion_budget_is_rejected`, `a_realistic_batch_stays_within_the_expansion_budget`, `the_rendered_digest_is_not_duplicated_into_every_alert`, `a_full_cap_batch_with_a_grafana_sized_digest_is_accepted`). Change the `batch_of` doc comment to "used to probe batch-size behaviour", and rename `a_full_cap_batch_with_a_grafana_sized_digest_is_accepted` to `a_large_batch_with_a_grafana_sized_digest_is_accepted`.

Run: `cargo test -p ops-source-grafana`
Expected: all pass. The expansion rejection test still rejects, because 500 alerts times a 100 KB padded group is 50 MB.

- [ ] **Step 3: Log generic-webhook refusals**

In `apps/server/src/webhook.rs`, replace the `SourceType::GenericWebhook` arm with:

```rust
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
```

Run: `cargo build -p ops-server && cargo clippy -p ops-server --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: README precision**

In the README "Grafana" section, replace the sentence beginning "A group with more than 500 alerts" with:

```markdown
There is no alert-count cap: the 1 MiB request body limit bounds a batch, and a
fleet-wide outage must arrive whole. A batch whose group context multiplied
across its alerts would pass 32 MiB is refused with a 400 and logged as
`grafana batch rejected`; Grafana retries a few times and then discards the
notification, so that log line means a group was lost.
```

- [ ] **Step 5: Manual check**

With the server running and `TOKEN` from a Grafana source, reuse the Task 3b Step 5 script with `range(1200)` instead of `range(400)`.

Expected: `202` with `"inserted":1200`. Before this task the same request returned `400` with `exceeding the cap` in the error.

- [ ] **Step 6: Checks and commit**

Run the four workspace checks.

```bash
git add adapters/sources/grafana/src/lib.rs apps/server/src/webhook.rs README.md docs/plans/2026-09-08-pilot-handoff.md
git commit -m "Handoff: no alert-count cap on Grafana batches; generic webhook refusals are logged; truncatedAlerts pinned"
```

---

### Task 3d: Body limit 4 MiB, Grafana max alerts as the operator guarantee, large-batch regime pinned

Added after the audit of e46209b, 3d8d2bc and c843bf2. Do this before Task 4.

**Ruling on the body limit: raise it to 4 MiB, and make Grafana's `Max alerts` a required contact-point setting in the runbook.** The executor measured the real ceiling at 1 MiB as 540 to 815 alerts, because Grafana's rendered digest travels in the body even though it is never stored. The arithmetic for raising it: the adversarial memory ceiling is set by the 32 MiB expansion budget, not by the body limit, so raising the body limit does not raise what a hostile body can cost. What it raises is the realistic ceiling, to roughly 2,160 to 3,260 alerts per notification at the same measured alert shapes. A real request that large costs a few tens of MB in flight and a few thousand sequential inserts, seconds on the pilot host. The cost of being wrong the other way is a fleet-wide outage refused whole. That asymmetry decides it.

The body limit alone still cannot promise "arrives whole" for every fleet, and this plan has now been wrong three times by asserting bounds from a model. So the guarantee moves to the operator side, where it can be set from real numbers: Grafana's webhook contact point has a `Max alerts` setting that truncates the group before sending. With `Max alerts` at 1,000 and the fattest measured alert shape (about 2.5 KB including its share of the digest), a body is at most about 2.5 MB, under 4 MiB with margin. Beyond 1,000 alerts the loss is a truncation that is logged and whose count is pinned into stored evidence, never a whole refusal. That is the pair of settings the runbook now mandates, with the arithmetic beside it so the design partner can re-derive it from their own alert sizes.

**Files:**
- Modify: `apps/server/src/main.rs` (`MAX_BODY_BYTES` and its comment)
- Modify: `adapters/sources/grafana/src/lib.rs` (the comment above `MAX_BATCH_EXPANSION_BYTES`, two test comments, one new probe constant, one new test)
- Modify: `README.md` "Grafana" section (numbers, `Max alerts` guidance)
- Modify: this plan, Task 8 runbook sections 4 and 9 (already updated; the runbook is not written yet)

**Interfaces:** unchanged.

- [ ] **Step 1: Pin the regime the body limit now admits**

In the `tests` module of `adapters/sources/grafana/src/lib.rs`, after the `FULL_BATCH` constant add:

```rust
    /// Well past anything the deleted count cap allowed, inside what a 4 MiB
    /// body holds at measured alert sizes. Not a limit; a probe size.
    const LARGE_BATCH: usize = 3_000;
```

and add this test next to `a_realistic_batch_stays_within_the_expansion_budget`:

```rust
    #[test]
    fn a_large_batch_with_a_realistic_group_stays_far_inside_the_expansion_budget() {
        // 3,000 alerts times a 2 KB group is 6 MB, under a fifth of the budget.
        // This is the regime the 4 MiB body limit admits; the budget must not
        // bind here, or a raised body limit would just move the refusal.
        let signals = split_batch(&batch_with_padded_group(LARGE_BATCH, 2_000)).unwrap();
        assert_eq!(signals.len(), LARGE_BATCH);
    }
```

Fix the two comments the executor flagged: in `a_batch_whose_group_times_alert_count_exceeds_the_expansion_budget_is_rejected`, change "A full-cap batch (FULL_BATCH alerts)" to "A FULL_BATCH-sized batch"; in `a_realistic_batch_stays_within_the_expansion_budget`, change "full-cap batch of alerts is the shape" to "FULL_BATCH batch of alerts is the shape".

Run: `cargo test -p ops-source-grafana`
Expected: all pass. The new test passes on the current code; it is a regression pin for the regime Step 2 opens, not a failing-first test.

- [ ] **Step 2: Raise the body limit**

In `apps/server/src/main.rs`, replace the comment and constant for `MAX_BODY_BYTES` with:

```rust
/// Grafana posts one body per notification group, and its rendered digest
/// travels in that body even though the engine never stores it. Measured
/// against the fixture, a 1 MiB limit admitted 540 to 815 alerts; 4 MiB admits
/// roughly 2,160 to 3,260 at the same alert shapes. The cap still keeps a
/// misconfigured source from exhausting memory (tech sheet 21): what a hostile
/// body can cost is bounded by the adapter's expansion budget, not by this
/// number, so raising this raises only the realistic ceiling. The operator-side
/// guarantee is Grafana's `Max alerts` contact-point setting; see the README.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
```

Run: `cargo build -p ops-server`
Expected: clean. The 413 middleware reads the constant, so its log line reports the new limit without a change.

- [ ] **Step 3: Correct the numbers in the adapter comment**

In the comment above `MAX_BATCH_EXPANSION_BYTES` in `adapters/sources/grafana/src/lib.rs`, replace the sentences that cite "1 MiB", "~815", and "~540" with:

```rust
// The request body limit (4 MiB, apps/server) is what actually bounds a batch
// now, and Grafana's rendered `message` digest counts against that limit on
// the way in even though it is never stored: measured against the fixture's
// ~685 B slim alert, a 1 MiB limit admitted ~815 alerts with the ~600 B/alert
// digest included and ~540 at a fatter ~1,339 B shape, so 4 MiB admits roughly
// 2,160 to 3,260. Above that the group is refused whole, as a 413 from the
// body-limit layer (logged, apps/server/src/main.rs), not a 400 from this
// crate. The operator-side guarantee is Grafana's `Max alerts` setting; see
// README.md's Grafana section for the arithmetic.
```

Keep the rest of that comment block as it is.

- [ ] **Step 4: README**

In the README "Grafana" section, replace the paragraph that begins "Grafana's rendered `message` digest is not stored" and the paragraph that begins "For a fleet larger than the body-limit ceiling" with:

```markdown
Grafana's rendered `message` digest is not stored — it is a rendering of
`alerts[]`, which is stored in full — but Grafana sends it in the same POST, so it
counts against the 4 MiB request body limit on the way in. There is no
alert-count cap in the adapter. Measured against the fixture, that limit admits
roughly 2,160 alerts of a fat ~1.3 KB shape or 3,260 of a slim ~685 B shape, digest
included. Above it the request is refused whole with a 413, logged as
`request body exceeds the ingest limit`; Grafana retries a few times and then
discards the notification, so that log line means a group was lost.

Set **Max alerts** on the Grafana contact point so that can never happen. With
`Max alerts` at 1,000 and the fattest measured alert shape (about 2.5 KB with its
share of the digest), a body is at most about 2.5 MB. Beyond 1,000 alerts Grafana
truncates the group itself before sending; the truncation is warn-logged
(`grafana dropped alerts from this notification group`) and the `truncatedAlerts`
count is preserved into every stored alert's group context, pinned by a test, so
the loss is visible and bounded instead of silent and total. Re-derive the number
from your own alert sizes: `Max alerts` times bytes per alert must stay under
4 MiB with margin.
```

- [ ] **Step 5: Manual check**

With the server running and `TOKEN` from a Grafana source, reuse the Task 3b Step 5 script with `range(2500)` instead of `range(400)`; the body is about 3.2 MB.

Expected: `202` with `"inserted":2500`. Before this task the same request returned `413` and the server log showed `request body exceeds the ingest limit` with `limit_bytes=1048576`.

- [ ] **Step 6: Checks and commit**

Run the four workspace checks.

```bash
git add apps/server/src/main.rs adapters/sources/grafana/src/lib.rs README.md docs/plans/2026-09-08-pilot-handoff.md
git commit -m "Handoff: 4 MiB ingest body limit; Grafana max alerts documented as the operator guarantee; large-batch regime pinned"
```

---

### Task 4: Failed signals are visible in the Operations View

**Files:**
- Modify: `crates/core/src/insights.rs:18-34` (`OperationsSummary`)
- Modify: `adapters/persistence/src/product.rs:299-313` (totals query) and `:446-461` (struct construction)
- Modify: `apps/server/src/dto.rs:13-28` (`SummaryResponse`) and the `From<OperationsSummary>` body
- Modify: `web/src/api.ts` (`Summary`), `web/src/pages/Overview.tsx:43-52` (stat tiles)
- Modify: `adapters/persistence/tests/normalization.rs:264-267` (assert visibility)

**Interfaces:**
- Produces: `OperationsSummary.failed_signals: i64`, `SummaryResponse.failed_signals`, `Summary.failed_signals` in TypeScript.

- [ ] **Step 1: Write the failing acceptance assertion**

In `adapters/persistence/tests/normalization.rs`, directly after the assertion that ends with `vec![("failed".into(), 6), ("processed".into(), 1)]` and its closing `);`, add:

```rust
    // The failed count must be visible where the operator looks, not only in SQL.
    let summary =
        ops_core::ports::ProductQueries::operations_summary(&store, failures.id, None, None)
            .await
            .unwrap();
    assert_eq!(summary.failed_signals, 6);
```

Run: `cargo test -p ops-persistence --test normalization -- --ignored`
Expected: compile error, `no field failed_signals`.

- [ ] **Step 2: Add the field to core**

In `crates/core/src/insights.rs`, inside `OperationsSummary`, after `pub raw_signals: i64,` add:

```rust
    /// Raw signals that could not be normalized. Shown here because a signal
    /// that fails quietly is exactly the loss this product exists to prevent.
    pub failed_signals: i64,
```

- [ ] **Step 3: Count it in SQL**

In `adapters/persistence/src/product.rs`, in the totals query, after the `raw_signals` subquery add:

```sql
              (SELECT COUNT(*) FROM raw_signals rs
                WHERE rs.organization_id = $1
                  AND rs.processing_status = 'failed') AS failed_signals,
```

and in the `Ok(OperationsSummary { ... })` construction, after the `raw_signals:` line add:

```rust
            failed_signals: totals.try_get("failed_signals").map_err(persistence)?,
```

- [ ] **Step 4: Expose it on the wire**

In `apps/server/src/dto.rs`, add `pub failed_signals: i64,` after `pub raw_signals: i64,` in `SummaryResponse`, and `failed_signals: s.failed_signals,` after `raw_signals: s.raw_signals,` in the `From` impl.

In `web/src/api.ts`, add `failed_signals: number;` after `raw_signals: number;` in `Summary`.

- [ ] **Step 5: Show it**

In `web/src/pages/Overview.tsx`, after the `events` stat tile add:

```tsx
        <div className={`stat${s.failed_signals > 0 ? " critical" : ""}`}>
          <div className="value">{s.failed_signals}</div>
          <div className="label">failed signals</div>
        </div>
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p ops-persistence --test normalization -- --ignored`
Expected: passes, including the new assertion.

Run: `cd web && npm run typecheck`
Expected: no errors.

- [ ] **Step 7: Checks and commit**

Run the four workspace checks.

```bash
git add crates/core/src/insights.rs adapters/persistence/src/product.rs \
  adapters/persistence/tests/normalization.rs apps/server/src/dto.rs \
  web/src/api.ts web/src/pages/Overview.tsx
git commit -m "Handoff: failed signals are counted in the operations summary and shown on the overview"
```

---

### Task 5: The server serves the built UI

**Files:**
- Modify: `Cargo.toml`, the `tower-http = { version = "0.6", ... }` line under `[workspace.dependencies]`
- Modify: `apps/server/src/main.rs`, the `Router::new()` chain that ends in `.with_state(state);`

**Interfaces:**
- Produces: env var `WEB_DIST_DIR` (default `web/dist`). When `<WEB_DIST_DIR>/index.html` exists, `/` and every non-API path serve the SPA; otherwise the server logs a warning and serves the API only.

Why: one origin for UI and API means no CORS, no second process on the pilot host, and the Vite dev proxy keeps working unchanged.

- [ ] **Step 1: Swap the tower-http features**

In the workspace `Cargo.toml` change the tower-http line to:

```toml
tower-http = { version = "0.6", features = ["trace", "limit", "fs"] }
```

- [ ] **Step 2: Serve the directory**

In `apps/server/src/main.rs` add the import:

```rust
use tower_http::services::{ServeDir, ServeFile};
```

Remove the line `.layer(tower_http::cors::CorsLayer::permissive())`.

Layer order matters here, and the executor flagged it in Task 3d: `Router::layer` wraps only the routes registered above it on the chain, so a fallback added after the layer block would sit outside the body limit, the 413 warning and the trace span. Register the fallback before the layers. Split the existing `let app = Router::new()....with_state(state);` chain into three parts. First, the routes alone:

```rust
    let router = Router::new()
        .route("/health", get(health))
        // ... every existing .route(...) line, unchanged ...
        .route("/api/v1/ingest/webhook/{token}", post(webhook::ingest));
```

Second, the conditional fallback:

```rust
    // The built UI is served from the same origin as the API: no CORS, and no
    // second process on the pilot host. Unknown non-API paths fall back to
    // index.html so React Router owns them. Registered before the layer block
    // so static requests get the same body limit, 413 warning and trace span
    // as everything else.
    let web_dir = std::env::var("WEB_DIST_DIR").unwrap_or_else(|_| "web/dist".to_string());
    let index = std::path::Path::new(&web_dir).join("index.html");
    let router = if index.is_file() {
        tracing::info!(%web_dir, "serving web UI");
        router.fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(index)))
    } else {
        tracing::warn!(%web_dir, "web build not found; serving API only (run `npm run build` in web/)");
        router
    };
```

Third, the existing layer block and state, unchanged apart from starting from `router`:

```rust
    let app = router
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        // ... the existing DefaultBodyLimit::disable(), warn_on_oversized_body
        //     and TraceLayer lines, unchanged ...
        .with_state(state);
```

Also update the doc comment on `UNMATCHED_ROUTE`: it is now reachable, for static-file requests served by the fallback, which match no route pattern. Replace "so this should not be reachable in practice, but a placeholder is safer than a panic if it ever is" with "which after the static fallback was added means every UI asset request; a placeholder rather than the literal path keeps those spans low-cardinality too".

- [ ] **Step 3: Build the UI and check**

```sh
cd web && npm ci && npm run build && cd ..
cargo run -p ops-server
```

In a second shell:

```sh
curl -s localhost:8080/ | head -c 60
curl -s localhost:8080/incidents | head -c 60
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/api/v1/operations/summary
curl -s localhost:8080/health
```

Expected: the first two print the start of `<!doctype html>`, the third prints `200`, the fourth prints `{"status":"ok"}`. Open `http://localhost:8080/` in a browser and click through Overview, Incidents, Sources. If the executor cannot drive a browser, the four curl checks are the gate; say so in the report and hand the click-through to a person. Then stop the server, rename `web/dist` temporarily, start it again, and confirm the log line `web build not found` and that `/api/v1/operations/summary` still answers. Rename it back.

- [ ] **Step 4: Checks and commit**

Run the four workspace checks.

```bash
git add Cargo.toml Cargo.lock apps/server/src/main.rs
git commit -m "Handoff: server serves the built UI from the same origin; CORS layer removed"
```

---

### Task 6: One-command deployment

**Files:**
- Create: `Dockerfile`, `.dockerignore`
- Modify: `docker-compose.yml`
- Modify: `README.md` "Running it" (add a Docker subsection) and "Configuration" (mention `WEB_DIST_DIR`, `API_BASE_URL`)

**Interfaces:**
- Produces: image `ops-intelligence:local` containing `ops-server`, `ops-worker` and `web/dist`; compose services `postgres`, `server` (port 8080), and `worker` under the `tools` profile.

- [ ] **Step 1: Ignore file**

`.dockerignore`:

```
target
web/node_modules
web/dist
.env
.git
```

- [ ] **Step 2: Dockerfile**

```dockerfile
# syntax=docker/dockerfile:1

FROM node:22-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY adapters ./adapters
COPY apps ./apps
COPY migrations ./migrations
RUN cargo build --release -p ops-server -p ops-worker

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/ops-server /src/target/release/ops-worker /usr/local/bin/
COPY --from=web /web/dist ./web/dist
ENV APP_HOST=0.0.0.0 \
    APP_PORT=8080 \
    WEB_DIST_DIR=/app/web/dist
EXPOSE 8080
CMD ["ops-server"]
```

`migrations/` must be present at build time because `sqlx::migrate!` embeds it into the binary. `pilot-data/` is not needed at runtime; the compose file mounts it for the worker.

- [ ] **Step 3: Compose**

Replace `docker-compose.yml` with:

```yaml
services:
  postgres:
    image: postgres:17-alpine
    container_name: ops-intelligence-db
    environment:
      POSTGRES_USER: ops
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-ops_local_dev}
      POSTGRES_DB: ops_intelligence
    ports:
      # Bound to loopback: the database is never reachable from the network,
      # only from this host (for local cargo runs and pg_dump).
      - "127.0.0.1:55432:5432"
    volumes:
      - ops-pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ops -d ops_intelligence"]
      interval: 3s
      timeout: 3s
      retries: 20
    restart: unless-stopped

  server:
    image: ops-intelligence:local
    build: .
    container_name: ops-intelligence-server
    environment:
      DATABASE_URL: postgres://ops:${POSTGRES_PASSWORD:-ops_local_dev}@postgres:5432/ops_intelligence
      DEFAULT_ORGANIZATION_SLUG: ${DEFAULT_ORGANIZATION_SLUG:-pilot-org}
      # What the UI prints as the ingestion URL; must be reachable from Grafana.
      API_BASE_URL: ${API_BASE_URL:-http://localhost:8080}
      RUST_LOG: ${RUST_LOG:-info,sqlx=warn}
      AI_ENABLED: "false"
    ports:
      - "8080:8080"
    depends_on:
      postgres:
        condition: service_healthy
    restart: unless-stopped

  # One-shot tool, not a daemon:  docker compose run --rm worker pilot dataset list
  worker:
    image: ops-intelligence:local
    build: .
    profiles: ["tools"]
    entrypoint: ["ops-worker"]
    environment:
      DATABASE_URL: postgres://ops:${POSTGRES_PASSWORD:-ops_local_dev}@postgres:5432/ops_intelligence
      DEFAULT_ORGANIZATION_SLUG: ${DEFAULT_ORGANIZATION_SLUG:-pilot-org}
      RUST_LOG: ${RUST_LOG:-info,sqlx=warn}
      AI_ENABLED: "false"
    volumes:
      - ./pilot-data:/app/pilot-data:ro
    depends_on:
      postgres:
        condition: service_healthy

volumes:
  ops-pgdata:
```

- [ ] **Step 4: Smoke test the stack**

```sh
docker compose up -d --build
curl -s localhost:8080/health
curl -s localhost:8080/ | head -c 60
docker compose run --rm worker pilot dataset list
```

Expected: `{"status":"ok"}`, the start of the HTML document, and `no datasets yet — capture some signals, then: pilot dataset create <name>`. Then create a Grafana source through `http://localhost:8080/sources`, post the fixture to its URL (Task 3 Step 9), and run:

```sh
docker compose run --rm worker pilot dataset create smoke grafana-live
docker compose run --rm worker pilot replay smoke run-a
docker compose run --rm worker pilot replay smoke run-b
docker compose run --rm worker pilot compare run-a run-b
```

`grafana-live` is the name of the source created in the UI; naming it restricts the dataset to that source, so the counts hold even though the shared volume also holds the synthetic fixture. Expected: the dataset reports 2 signals, both replays report 2 events and 2 incidents, and compare reports no deltas.

- [ ] **Step 5: Document**

In `README.md` under "Running it", after the existing cargo block, add:

````markdown
### In Docker

```sh
docker compose up -d --build            # PostgreSQL + server (API and UI) on :8080
docker compose run --rm worker pilot dataset list
```

Set `API_BASE_URL` to the address Grafana will use (for example
`http://10.0.0.12:8080`) before starting, because it is what the UI prints as the
ingestion URL. The database port is bound to loopback only. The worker is a
one-shot tool under the `tools` profile, not a daemon.
````

In the Configuration block comments, add two lines:

```sh
# Address the UI prints as the ingestion URL. Must be reachable from Grafana.
API_BASE_URL=http://localhost:8080
# Where the server finds the built UI; unset means web/dist relative to the working directory.
WEB_DIST_DIR=web/dist
```

- [ ] **Step 6: Commit**

```bash
git add Dockerfile .dockerignore docker-compose.yml README.md
git commit -m "Handoff: Dockerfile and compose run the whole stack; database bound to loopback"
```

---

### Task 7: CI gate

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `scripts/check.sh`
- Modify: `README.md` "Checks" (point at the script)

**Interfaces:**
- Produces: a workflow named `ci` with jobs `rust`, `web`, `docker`; a script that runs the same gate locally. The workflow is inert until the repository has a GitHub remote; the script is the gate until then.

- [ ] **Step 1: The local script**

`scripts/check.sh`:

```sh
#!/usr/bin/env sh
# The CI gate, runnable locally. Set TEST_DATABASE_URL (database name ending in
# _test) to also run the PostgreSQL acceptance tests.
set -eu
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace

if [ -n "${TEST_DATABASE_URL:-}" ]; then
  cargo test -p ops-persistence -- --ignored
else
  echo "TEST_DATABASE_URL unset: skipped the PostgreSQL acceptance tests"
fi

( cd web && npm ci --no-audit --no-fund && npm run typecheck && npm run build )
echo "all checks passed"
```

Run: `chmod +x scripts/check.sh && sh scripts/check.sh`
Expected: ends with `all checks passed`.

- [ ] **Step 2: The workflow**

`.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
  pull_request:

jobs:
  rust:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:17-alpine
        env:
          POSTGRES_USER: ops
          POSTGRES_PASSWORD: ops
          POSTGRES_DB: ops_ci_test
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U ops -d ops_ci_test"
          --health-interval 3s
          --health-timeout 3s
          --health-retries 20
    env:
      TEST_DATABASE_URL: postgres://ops:ops@localhost:5432/ops_ci_test
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo build --workspace
      - run: cargo test --workspace
      - run: cargo test -p ops-persistence -- --ignored

  web:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: web
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
          cache-dependency-path: web/package-lock.json
      - run: npm ci --no-audit --no-fund
      - run: npm run typecheck
      - run: npm run build

  docker:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: docker build .
```

The `rust` job runs the three ignored PostgreSQL tests, so CI asserts the fixture outcomes, the ten must-not-correlate pairs, and replay determinism on every push.

- [ ] **Step 3: Document**

In `README.md` "Checks", before the existing command block, add:

```markdown
`sh scripts/check.sh` runs the whole gate; `.github/workflows/ci.yml` runs the same
gate plus the PostgreSQL acceptance tests on every push once the repo has a remote.
```

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml scripts/check.sh README.md
git commit -m "Handoff: CI workflow and local check script run the full gate including database tests"
```

---

### Task 8: Runbook, and the replay command prints how to view its tenant

**Files:**
- Create: `docs/pilot-runbook.md`
- Modify: `apps/worker/src/pilot.rs:88-90` (print the slug)

**Interfaces:**
- Consumes: everything above.
- Produces: a document the infra teammate follows without the author present.

- [ ] **Step 1: Print the replay tenant slug**

Replay tenants are created with slug `replay-<run id>` (see `adapters/persistence/src/pilot_store.rs`, `start_run`). In `apps/worker/src/pilot.rs`, after the line `println!("  tenant       {} (isolated)", run.target_organization_id);` add:

```rust
    println!("  ui slug      replay-{}", run.id);
```

Run: `cargo test -p ops-worker && cargo clippy -p ops-worker --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 2: Write the runbook**

`docs/pilot-runbook.md`:

````markdown
# Pilot runbook: capturing real Grafana alerts

For the infra engineer running the first real capture (phase P2). You do not need
to read the code. Every command below is copy-paste. If something here is wrong,
that is a bug in this document; note what happened and keep going.

## 0. What this does and does not do

- It receives every alert your Grafana sends to a new contact point, stores it
  untouched, and lets us replay the week through the engine as often as we want.
- It does not replace, silence, or delay any notification you already receive.
  Grafana keeps delivering to your existing contact points exactly as before.
- Nothing leaves the host you run it on. AI is off. No cloud calls.
- Stopping it is one Grafana click (section 7). Your alerting does not depend on it.

## 1. What you need

- A Linux host with Docker and Docker Compose, 2 vCPU and 4 GB are plenty, that
  Grafana can reach on port 8080, and that you can reach from your workstation.
- Five days where the host stays up. The capture is only as complete as the
  host's uptime, so this should not be a laptop.
- Restrict port 8080 to Grafana's address and your workstation. The product API
  has no login of its own for the pilot (tech sheet 20, option A). Example with ufw:

  ```sh
  sudo ufw allow from <grafana-ip> to any port 8080 proto tcp
  sudo ufw allow from <your-workstation-ip> to any port 8080 proto tcp
  ```

## 2. Start it

```sh
git clone <repo-url> ops-intelligence && cd ops-intelligence
export API_BASE_URL=http://<this-host-ip>:8080     # what the UI prints as the ingestion URL
export POSTGRES_PASSWORD=<pick-a-password>          # anything; it is only reachable on this host
docker compose up -d --build
curl -s localhost:8080/health
```

Expected: `{"status":"ok"}`. Open `http://<this-host-ip>:8080/` from your
workstation. You should see the Overview page with zeros.

Keep the two exported variables somewhere you can find them; use the same values every
time you run a compose command.

## 3. Create the Grafana source

1. Open `http://<this-host-ip>:8080/sources`.
2. Leave the type on "Grafana contact point", type a name such as `grafana-prod`, click Add source.
3. Copy the ingestion URL shown. It is displayed exactly once. If you lose it,
   disable that source and create another.

## 4. Point Grafana at it without touching existing notifications

In Grafana (Alerting → Contact points):

1. New contact point. Name `ops-intelligence`. Integration **Webhook**. URL: the
   ingestion URL from step 3. HTTP method **POST**. Leave "Send resolved" on:
   recoveries are half the data. Set **Max alerts** to `1000`. This is not
   optional: it is what guarantees a fleet-wide storm is truncated with a count
   instead of refused whole. If your alerts are unusually large, the rule is
   `Max alerts` times bytes per alert under 4 MiB with margin.
2. Click **Test** and send the test notification. Back on the Sources page, the
   source's "Last seen" should update within a few seconds. If it does not, the
   host is not reachable from Grafana; check the firewall before anything else.

Then (Alerting → Notification policies):

3. On the default policy, add a **nested policy** and move it to the top.
   Matcher: `alertname =~ .+` (matches everything). Contact point: `ops-intelligence`.
   Enable **Continue matching subsequent sibling nodes**. Save.
4. Trigger or wait for one real alert and confirm you still received it through
   your usual channel. If you did not, delete the nested policy immediately and tell
   the author; do not proceed.

## 5. During the capture (once a day, two minutes)

- Sources page: "Last seen" is recent. If it is more than a few hours old on a
  normal day, check `docker compose ps` and the firewall.
- Overview page: the "failed signals" tile. A non-zero count means some payloads
  could not be understood. Not an emergency: they are stored and will be handled at
  replay. Note the number.
- `docker compose logs server | grep -i truncated`: Grafana drops alerts from very
  large groups; a line here means it happened.

Do not run `process` or `correlate` against the live tenant during the capture.
The capture tenant stays raw; the analysis happens by replay (section 6). This is
what makes the week re-runnable.

## 6. End of capture

Back it up first. The captured week is the asset; the code is replaceable.

```sh
docker compose exec postgres pg_dump -U ops -Fc ops_intelligence > capture-$(date +%F).dump
```

Copy that file somewhere that is not this host.

Freeze the week into a dataset, replay it twice, and confirm the two runs agree:

```sh
docker compose run --rm worker pilot dataset create week-01 grafana-prod
docker compose run --rm worker pilot replay week-01 run-a
docker compose run --rm worker pilot replay week-01 run-b
docker compose run --rm worker pilot compare run-a run-b
```

Expected: both replays print the same counts, and compare reports no deltas.
Each replay prints a `ui slug` line such as `replay-4f2c…`. To look at a replay in
the UI, restart the server pointed at that tenant, then point it back afterwards:

```sh
DEFAULT_ORGANIZATION_SLUG=replay-<id> docker compose up -d server
# ... review at http://<this-host-ip>:8080/ ...
docker compose up -d server                     # back to the capture tenant
```

## 7. Stop or roll back

- Grafana: delete the nested policy (section 4 step 3). Notifications to the
  engine stop instantly; nothing else changes.
- Host: `docker compose down`. Data stays in the `ops-pgdata` volume. To remove
  it entirely: `docker compose down -v`, after the backup in section 6.
- To pause without removing: disable the source on the Sources page. Grafana
  will get a 400 and give up after its retries.

## 8. The review session (one hour, author present)

Walk every incident in run-a together and answer, per incident: right grouping,
wrong grouping, or missed grouping. Then the seven questions from the product
sheet (section 31, Day 7):

1. What did this show you that your existing tools did not?
2. What did it hide that you still needed?
3. Which grouping was wrong?
4. Which insight was useless?
5. What should be automatic next?
6. Would you keep this running tomorrow?
7. If it disappeared tomorrow, would you care?

Record the answers in `docs/pilot-01-evaluation.md`. Your corrections become the
golden cases for phase P3.

## 9. Known ceilings

- Request bodies over 4 MiB are refused whole and logged as
  `request body exceeds the ingest limit`. With `Max alerts` set as in section 4
  this cannot happen; if the line appears anyway, tell the author the same day.
- A group truncated by `Max alerts` is logged as
  `grafana dropped alerts from this notification group`, with the count. That is
  the expected behaviour in a large storm, not a fault; note the count for the
  review session.
- A batch whose group context multiplied across its alerts would pass 32 MiB is
  refused and logged as `grafana batch rejected`. At real Grafana shapes this is
  unreachable. If the line appears, tell the author: the notification was shaped
  unlike anything the engine expects.
- A generic-webhook body the engine cannot read is refused and logged as
  `webhook payload rejected`. Same instruction.
- Which alert labels mean environment, service and resource are fixed guesses
  (`environment`/`env`, `service`/`job`/`app`, `instance`/`host`/`resource`/`pod`).
  Real data will correct them; that is expected.
- Severity comes from a label named `severity`. Alerts without it are treated as
  warning while firing.
- Everything the engine computes is deterministic and re-runnable. Nothing you do
  in the UI changes the captured signals.
````

- [ ] **Step 3: Have someone else follow it**

Ask a colleague who has not seen the repo to run sections 2, 3 and 6 against a scratch host using the fixture instead of Grafana. Fix every step they stumble on. This step is the deliverable; do not skip it.

- [ ] **Step 4: Commit**

```bash
git add docs/pilot-runbook.md apps/worker/src/pilot.rs
git commit -m "Handoff: pilot runbook for the design partner; replay prints its UI slug"
```

---

### Task 9: Status table, layout, final gate

**Files:**
- Modify: `README.md:12-46` (status table and layout)

- [ ] **Step 1: Status and layout**

In the README "Grafana" section, the sentence calling 2.5 KB "the fattest measured alert shape" is wrong: the measured fat shape is about 1.9 KB with its share of the digest, and 2.5 KB is the margin the `Max alerts` arithmetic uses. Reword it to say so, with both numbers.

In the status table, change the P2 row to:

```markdown
| P2 | Real capture: Grafana adapter, Docker deployment, CI, runbook | **built; awaiting first live dataset** |
```

Replace the paragraph starting "No vendor adapter (Grafana / Azure Monitor / Email) is written yet" with:

```markdown
The Grafana adapter exists (`adapters/sources/grafana`); Azure Monitor and Email
will not be written until the real source inventory selects them — see
[decision 0001](docs/decisions/0001-synthetic-pilot-before-source-inventory.md).
```

In the Layout block, add these lines in place:

```
  sources/webhook/    Generic Webhook source adapter (canonical payload)
  sources/grafana/    Grafana Alerting webhook adapter (native payload)
  ai/                 reasoning providers behind the ReasoningProvider port
apps/server/          HTTP server: product API, ingestion, serves web/dist
apps/worker/          worker binary (`import`, `process`, `correlate`, `reason`, `pilot`)
web/                  React UI (Vite)
docs/pilot-runbook.md how the design partner runs the capture
scripts/check.sh      the CI gate, locally
```

- [ ] **Step 2: Final gate**

```sh
sh scripts/check.sh
docker compose up -d --build && curl -s localhost:8080/health && docker compose down
git status --short
```

Expected: `all checks passed`, `{"status":"ok"}`, and a clean tree after the commit below.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Handoff: README reflects the Grafana adapter, Docker run path, and P2 status"
```

---

## Self-review against the spec

- **Tech sheet 15, adapter responsibilities:** preserve original payload (Task 2, group + alert), extract status (state_raw), extract labels, extract timestamps (startsAt/endsAt), map severity (via `labels.severity` through core), derive service/resource where possible, map resolved into recovery (state `resolved` → `EventState::Resolved`). Covered.
- **Tech sheet 26 step 3, CI minimum:** fmt, clippy, tests, frontend typecheck and build. Covered in Task 7. Frontend lint is the typecheck script in `package.json`; there is no separate linter, which matches the repo.
- **Tech sheet 24, configuration:** `APP_HOST`, `APP_PORT`, `DATABASE_URL`, `RUST_LOG`, `AI_*` unchanged; `API_BASE_URL` and `WEB_DIST_DIR` documented in Task 6.
- **Tech sheet 20, authentication:** option A, network boundary, documented in the runbook section 1.
- **Tech sheet 33, definition of done:** "top two pilot integrations work" becomes one of two (Grafana). The second waits for the Day 0 interview by design. "CI passes" is Task 7. "pilot run completed" and "pilot evaluation documented" are enabled by Task 8 and happen with the design partner.
- **Tech sheet 6, must-have "failed processing visibility":** Task 4.
- **Placeholder scan:** no TBD, no "add validation", no "similar to Task N"; every code step carries its code.
- **Type consistency:** `AlertSignal { external_id: String, payload: Value }` is used identically in Tasks 2 and 3; `create_webhook(organization_id, source_type, name, token)` matches between port, persistence and server; `failed_signals: i64` matches core, SQL alias, DTO and TypeScript.

## Heli notes for executors

- Claim a write lease before the first edit: `node .heli-harness/heli.mjs task claim <task-id> --mode write` from the workspace root.
- Record each task's files under "Files expected to change" and each verification command under "Planned verification" in the task's `current-task.md`.
- Task 3 changes a shared port signature. Run `impact` before it if another task has an active lease on `adapters/persistence` or `apps/server`.

## Execution log

| Task | Commit | Notes |
|---|---|---|
| 1 | b9a9288 | as planned |
| 2 | 37f897a, 8af688d | executor added `MAX_ALERTS_PER_BATCH = 500`, not in the plan |
| 3 | 2f710bd, b712ac2 | executor added `MAX_BATCH_EXPANSION_BYTES = 32 MiB`; UI half verified in a browser by the author on a clean database |
| audit | | 2026-09-08, read-only: gate reproduced (100 workspace tests, 3 database tests, fmt, clippy `-D warnings`, web typecheck), tree clean. b712ac2 meets its stated intent but assumes a few-KB group; Grafana's `message` digest breaks that assumption, so Task 3b was added. |
| 3b | 937fb42 | as planned; the plan itself committed alongside. Audit 2026-09-08: gate reproduced (102 workspace tests, 3 database tests, fmt, clippy), diff matches the plan line for line. Executor's growth analysis of the remaining group fields (commonLabels and commonAnnotations are intersections, title carries a count) checked and agreed. |
| 3c | e46209b, 3d8d2bc, c843bf2 | cap removed as planned. Executor found the task's premise wrong: the digest is in the request body even though it is not stored, so 1 MiB admits 540 to 815 alerts, not 700 to 1,300; and a 413 from the body-limit layer was silent at the default log level. Fixed with measured numbers and a one-warn middleware. Audit 2026-09-08: gate reproduced (102 workspace tests, 3 database tests, fmt, clippy), diffs read, arithmetic agreed. |
| 3d | f115efc, b5ab8d3 | body limit raised, `Max alerts` documented, large-batch regime pinned. Executor measured all four numbers the task asserts before dispatching; all held. Found that axum's `Json` extractor carries its own 2 MiB default, so the raise was a no-op until `DefaultBodyLimit::disable()`; found that the 413 warning and the trace span both logged the literal path, which on the ingest route is the token; fixed both with `MatchedPath` and pinned the effective limit with a router-level test. Audit 2026-09-09: gate reproduced (105 workspace tests, 3 database tests, fmt, clippy), diffs read, layer order and extension availability checked. |

Executor's deferred-minor ledger, kept for the final review: partial mid-batch database failure commits earlier rows and returns 500 without touching last-seen (Grafana's retry deduplicates); the truncation warning fires before persistence, so it can name a group a later database failure never stored; the Task 2 minors as recorded by the executing session. Closed by Task 3b: the `_ =>` catch-all and the `truncatedAlerts` read on every payload. Closed by Task 3c: silent generic-webhook refusals, the unpinned `truncatedAlerts` preservation, and the README's imprecise description of the bound. Closed by Task 3d: no test in the above-500 regime, and the two "full-cap" test comments. Informational, kept: per-request insert count now scales with the body limit, a few thousand sequential inserts at most, seconds on the pilot host; revisit only if Grafana's webhook timeout is ever hit. From Task 3d, kept: the 413 warning's message says "ingest limit" on every route; the `LARGE_BATCH` test comment describes a body-limit regime the adapter crate cannot exercise; `http-body-util` is pinned in the server crate rather than the workspace table. Carried into Task 5: the fallback must be registered before the layer block (now written into that task). Carried into Task 9: the README's "fattest measured" wording.
