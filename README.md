# Operations Intelligence Engine

An operational intelligence layer above existing monitoring tools. It turns noisy alert
signals into a small number of traceable incidents and insights.

```
RawSignal  →  Event  →  Incident  →  Insight  →  Human Action
```

Architecture is frozen at v0.1 — see `../../resources/operations-intelligence-architecture-v0.1.md`.

## Status

| Slice | Scope | State |
|---|---|---|
| 0 | Synthetic pilot dataset + expected-outcome fixture | **done** |
| 1 | Workspace, PostgreSQL, RawSignal, CSV importer | **done** |
| 2 | Normalization → Events | **done; acceptance checks below** |
| 3 | Deterministic correlation → Incidents | not started |
| 4 | Server: APIs, Generic Webhook, deterministic Insights | not started |
| 5 | Operations UI | not started |
| 6 | AI reasoner behind the port | not started |

No vendor adapter (Grafana / Azure Monitor / Email) is written yet, and none will be until
the real source inventory selects the top two — see
[decision 0001](docs/decisions/0001-synthetic-pilot-before-source-inventory.md).

## Layout

```
crates/core/          domain + ports. No axum, no sqlx, no vendor SDK, no frontend.
adapters/
  persistence/        PostgreSQL via SQLx
  sources/csv/        CSV / spreadsheet source adapter
apps/worker/          worker binary (`import` and `process` subcommands)
migrations/           SQLx migrations
pilot-data/           synthetic dataset + expected-outcome oracle
docs/decisions/       architecture decision records
```

The dependency rule is one-directional: adapters depend on `core`, `core` depends on
nothing outside itself. That is what keeps the pipeline source-agnostic.

## Configuration

There is no `.env.example` in the repo — the workspace guardrail blocks `.env`-shaped
files. Copy this block into a local `.env` (which `.gitignore` excludes) or export it:

```sh
APP_ENV=local

# Matches docker-compose.yml. Port 55432 avoids clashing with a local PostgreSQL.
DATABASE_URL=postgres://ops:ops_local_dev@localhost:55432/ops_intelligence

# Single-tenant pilot boundary. The server derives organization_id from config,
# never from request data (tech sheet 20).
DEFAULT_ORGANIZATION_SLUG=pilot-org

RUST_LOG=info,ops_worker=debug,sqlx=warn

# AI stays off until the data boundary is agreed (source inventory 21).
# The deterministic pipeline must work fully with AI disabled.
AI_ENABLED=false
```

The credentials above are local-development only and must not be reused anywhere else.

## Running it

```sh
docker compose up -d                                    # PostgreSQL on :55432
cargo run -p ops-worker -- import pilot-data/synthetic-alerts-v1.csv
cargo run -p ops-worker -- process
```

Migrations run automatically on startup, from an empty database upward.

Expected output on a clean database:

```
  csv rows read     69
  raw signals new   67
  duplicates        2  (suppressed at ingestion)
```

69 → 67 is correct: two rows are a byte-identical email redelivery sharing one
`external_id`, collapsed at ingestion per tech sheet §9. The import is idempotent, so a
second run inserts 0 and reports 69 duplicates.

`process` consumes only `received` RawSignals for `DEFAULT_ORGANIZATION_SLUG`.
On this fixture it creates **67 Events**, with 67 `processed`, zero `failed`, and
zero pending signals. A second processing run creates zero Events. Informational,
unclassified and recovery Events are retained. No correlation is performed.

Each transaction locks one RawSignal, moves it through `processing`, and commits
the Event and `processed` status together. Invalid payloads or unsupported source
types commit `failed` with `processing_error`. Database errors roll back the claim
to `received` and exit unsuccessfully; rerun after correcting the database error.
Failed payloads remain terminal for inspection; there is no automatic retry or
evidence rewriting. Concurrent workers skip each other's locked rows.

The worker prints counts for every status and exits unsuccessfully if failures or
nonterminal work remain (including work another processor currently owns). Event
timestamps come exclusively from the source RFC3339 `timestamp`; import time is
only `received_at`. Missing/invalid source timestamps fail explicitly.

## Checks

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

The default tests include all 67 per-row normalization expectations and malformed
input checks without a database. The opt-in SQLx acceptance test uses a separate
local database whose name must end in `_test`. With the compose database running:

```sh
docker exec ops-intelligence-db createdb -U ops ops_normalization_test
export TEST_DATABASE_URL=postgres://ops:ops_local_dev@localhost:55432/ops_normalization_test
cargo test -p ops-persistence --test normalization -- --ignored
```

In PowerShell set `$env:TEST_DATABASE_URL` instead of `export`. Create the test DB
once; tests use fresh tenant IDs on each run and retain results for inspection.
They verify 69 rows → 67 RawSignals → 67 Events, historical timestamps, matching
family/severity/state, evidence linkage, reruns, competing processors, two-tenant
isolation, persisted failures, and rollback after a database write failure.

## Idempotency

Two partial unique indexes, in the priority order from tech sheet §9:

| Source provides | Deduplicated on |
|---|---|
| a stable `external_id` | `(source_id, external_id)` |
| nothing | `(source_id, payload_hash)` |

Split rather than combined so that a source *with* external ids can still legitimately
re-fire an identical payload later, while a source *without* them falls back to content
identity.

## Ground rules

- Vendor parsing stays in `adapters/sources/<vendor>/`. The canonical taxonomy and
  normalization stay reusable and testable without vendor code — see
  [decision 0002](docs/decisions/0002-event-family-derivation.md).
- Correlation windows are computed from the event's own `occurred_at`, never the wall
  clock. Time is injected through the `Clock` port. A historical CSV import would
  otherwise collapse a whole day into a single window.
- Deterministic logic owns normalization, identity, deduplication, correlation, recovery
  pairing and incident lifecycle. AI is bounded to interpretation and never assigns a
  fingerprint input.
- Every signal must reach a terminal processing status. No silent loss.
