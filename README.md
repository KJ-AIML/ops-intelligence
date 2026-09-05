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
| 3 | Deterministic correlation → Incidents | **implemented; acceptance checks below** |
| 4 | Server: product API, Generic Webhook, deterministic insights | **done** |
| 5 | Operations UI | **done** |
| 6 | AI reasoner behind the port | **done; off by default** |

No vendor adapter (Grafana / Azure Monitor / Email) is written yet, and none will be until
the real source inventory selects the top two — see
[decision 0001](docs/decisions/0001-synthetic-pilot-before-source-inventory.md).

## Layout

```
crates/core/          domain + ports. No axum, no sqlx, no vendor SDK, no frontend.
adapters/
  persistence/        PostgreSQL via SQLx
  sources/csv/        CSV / spreadsheet source adapter
apps/worker/          worker binary (`import`, `process`, `correlate` subcommands)
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

# AI stays off until the pilot data boundary is agreed (source inventory 21).
# With this unset or false, no key is read and nothing leaves the deployment.
AI_ENABLED=false
# AI_API_KEY=
# AI_MODEL=claude-opus-5
# AI_EFFORT=low
```

The credentials above are local-development only and must not be reused anywhere else.

## Running it

```sh
docker compose up -d                                    # PostgreSQL on :55432
cargo run -p ops-worker -- import pilot-data/synthetic-alerts-v1.csv
cargo run -p ops-worker -- process
cargo run -p ops-worker -- correlate
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

`correlate` drains Events by `occurred_at`, never import or processing time. Its
fingerprint is tenant + environment + service + resource + canonical event family.
Events in distinct families remain distinct incidents even when their service,
resource and timestamps are identical. Informational/non-actionable Events and
orphan recoveries are explicitly marked ignored; linked Events receive exactly one
`trigger`, `duplicate`, `update`, or `recovery` relation. Re-running is idempotent.
For the pilot fixture it produces 22 Incidents: 8 open and 14 recovered, with
52 linked Events and 15 explicitly ignored Events. This includes 10 duplicate
relations, 16 recovery relations, one reopen, and no cross-family grouping.

The worker prints counts for every status and exits unsuccessfully if failures or
nonterminal work remain (including work another processor currently owns). Event
timestamps come exclusively from the source RFC3339 `timestamp`; import time is
only `received_at`. Missing/invalid source timestamps fail explicitly.

## API

```sh
cargo run -p ops-server            # listens on APP_PORT, default 8080
```

| Method | Path | Purpose |
|---|---|---|
| GET | `/health`, `/health/ready` | liveness; readiness also pings the database |
| GET | `/api/v1/operations/summary` | the Operations View payload — counts, noisiest sources, recurring patterns |
| GET | `/api/v1/incidents` | filter by `status`, `severity`, `service`, `resource`, `from`, `to`, `limit`; `sort=attention` for triage order |
| GET | `/api/v1/incidents/{id}` | incident plus its full evidence timeline, each entry carrying the original payload |
| POST | `/api/v1/incidents/{id}/acknowledge` | open → acknowledged |
| POST | `/api/v1/incidents/{id}/resolve` | → resolved (terminal) |
| GET | `/api/v1/events` | event explorer, for debugging and trust |
| GET/POST | `/api/v1/sources` | list, or create a Generic Webhook source |
| PATCH | `/api/v1/sources/{id}` | enable/disable |
| POST | `/api/v1/ingest/webhook/{token}` | Generic Webhook ingestion |

### Generic Webhook

Creating a source returns its ingestion URL and token **once**. The token is the
source's credential: it is never returned again, never logged, and it identifies
the tenant, so no request body can select an organization.

```sh
curl -X POST localhost:8080/api/v1/sources -H 'content-type: application/json'      -d '{"name":"grafana-live"}'

curl -X POST localhost:8080/api/v1/ingest/webhook/$TOKEN      -H 'content-type: application/json'      -d '{"timestamp":"2026-09-03T09:42:10+07:00","title":"API latency high",
          "severity":"warning","service":"payment-api","resource":"api-prod-01",
          "environment":"production","state":"firing","external_id":"grafana-123"}'
```

`timestamp` and `title` are the required minimum. Ingestion persists the
RawSignal and returns immediately — `202` for a new signal, `200` with
`"duplicate": true` for a redelivery, so a sender stops retrying. Normalization
and correlation happen in the worker, so a slow pipeline can never make a
source's alert delivery time out.

### Incident lifecycle

```
open ──acknowledge──> acknowledged ──┐
  │                                  ├── resolve ──> resolved  (terminal)
  └── recovery event ──> recovered ──┘
              │
              └── matching event inside the reopen window ──> open
```

`acknowledged` is **active**: someone looking at an incident does not stop it
collecting evidence, so correlation keeps attaching to it. `resolved` is
terminal and never reopens — a later matching event starts a new incident, which
is what makes the repeat visible as a recurrence. Reopening clears a prior
acknowledgement, because a repeat needs fresh eyes.

### Deterministic insights

`sort=attention` ranks incidents by a pure, explainable function
(`crates/core/src/insights.rs`), not a model: still-happening beats recovered,
unacknowledged beats acknowledged, critical beats warning, recurring beats
one-off, and recency breaks ties. Every incident's `attention_score` is returned
so the ordering can be checked rather than trusted.

## Operations UI

```sh
cd web && npm install && npm run dev     # http://localhost:5173
```

The dev server proxies `/api` to the server on port 8080, so the browser sees one
origin and there is no CORS or base-URL configuration to run the pilot locally.

| Route | What it answers |
|---|---|
| `/` | What happened, what needs attention, what keeps happening, where the noise comes from |
| `/incidents` | Filter by status and severity; sort by triage rank or recency |
| `/incidents/{id}` | Summary, recurrence, and the full evidence timeline down to each original payload |
| `/events` | Normalized event explorer — secondary, for debugging and trust |
| `/sources` | Source list, enable/disable, and creating a webhook source |

Colour carries meaning only for severity and status. There are no charts: v0.1
prioritises lists, timelines and counts, per tech sheet 19.

## Intelligence (optional, off by default)

```sh
cargo run -p ops-worker -- reason [limit]     # default limit 10
```

Generates one bounded explanation per incident that lacks one. **It does nothing
unless `AI_ENABLED=true` and `AI_API_KEY` are set** — see
[decision 0004](docs/decisions/0004-ai-disabled-by-default-anthropic-first-provider.md).
Every deterministic number in the product is unaffected either way.

What the model may see is one struct, `IncidentContext`: status, severity,
family, environment/service/resource, durations, counts, recurrence, event
**titles**, source names. **No raw payloads, no source messages, no log lines.**
Anything not copied into that struct cannot reach a provider, and a test asserts
it. That is what makes the source-inventory §21 boundary reviewable rather than
aspirational.

What comes back is schema-validated before it is stored: typed fields, a
three-value `actionability` enum, length bounds, and a check that **any
infrastructure identifier the model names appears in the supplied facts**. An
invalid response is retried once, then recorded as `status='failed'` with the
reason — raw model text is never persisted as a success. Failed attempts stay
visible and retryable.

AI never assigns an `event_family`, severity, status or fingerprint. Those are
deterministic, and correlation does not consult a model.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/incidents/{id}/insights` | interpretations for one incident |
| GET | `/api/v1/insights` | recent interpretations |

Swapping providers is one file implementing `ReasoningProvider`; nothing else
changes.

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
