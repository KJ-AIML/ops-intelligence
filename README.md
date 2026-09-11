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
| P1 | Pilot Lab: capture, datasets, replay, compare | **done** |
| P2 | Real capture: Grafana adapter, Docker deployment, CI, runbook | **built; awaiting first live dataset** |
| P3 | Golden cases + human-reviewed regression | not started |
| P4 | Shadow mode against live infrastructure | not started |

The Grafana adapter exists (`adapters/sources/grafana`); Azure Monitor and Email
will not be written until the real source inventory selects them — see
[decision 0001](docs/decisions/0001-synthetic-pilot-before-source-inventory.md).

## Layout

```
crates/core/          domain + ports. No axum, no sqlx, no vendor SDK, no frontend.
adapters/
  persistence/        PostgreSQL via SQLx
  sources/csv/        CSV / spreadsheet source adapter
  sources/webhook/    Generic Webhook source adapter (canonical payload)
  sources/grafana/    Grafana Alerting webhook adapter (native payload)
  ai/                 reasoning providers behind the ReasoningProvider port
apps/server/          HTTP server: product API, ingestion, serves web/dist
apps/worker/          worker binary (`import`, `process`, `correlate`, `reason`, `pilot`)
web/                  React UI (Vite)
migrations/           SQLx migrations
pilot-data/           synthetic dataset + expected-outcome oracle
docs/decisions/       architecture decision records
docs/pilot-runbook.md how the design partner runs the capture
scripts/check.sh      the CI gate, locally
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

# Address the UI prints as the ingestion URL. Must be reachable from Grafana.
API_BASE_URL=http://localhost:8080
# Where the server finds the built UI; unset means web/dist relative to the working directory.
WEB_DIST_DIR=web/dist

# Bearer token for the product API (not ingestion, which uses per-source tokens).
# Unset means no check: local development only. The pilot host must set it.
API_TOKEN=
```

The credentials above are local-development only and must not be reused anywhere else.

## Running it

```sh
docker compose up -d postgres                            # PostgreSQL on :55432
cargo run -p ops-worker -- import pilot-data/synthetic-alerts-v1.csv
cargo run -p ops-worker -- process
cargo run -p ops-worker -- correlate
```

Start only the `postgres` service for this flow — the compose file's default `docker
compose up -d` also starts `server`, which triggers the full release build and then
binds `:8080` itself, colliding with `cargo run -p ops-server`.

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

### In Docker

```sh
POSTGRES_PASSWORD=<pick-one> docker compose up -d --build   # PostgreSQL + server (API and UI) on :8080
docker compose run --rm worker pilot dataset list
```

On Windows PowerShell:

```powershell
$env:POSTGRES_PASSWORD = "<pick-one>"; docker compose up -d --build
```

Choose `POSTGRES_PASSWORD` before the *first* `up` — Postgres only applies it at
`initdb`, when `ops-pgdata` is created. Once the volume exists, changing the variable
only changes what `server`/`worker` try to connect with; the database's own password
does not change, so `server` (which restarts `unless-stopped`) crash-loops instead of
failing loudly. To rotate the password later, remove the `ops-pgdata` volume (destroys
all data) or update it inside Postgres directly. Leaving `POSTGRES_PASSWORD` unset
falls back to `ops_local_dev`, the same value documented above for local `cargo`
runs — fine for a closed pilot box, but pick your own for anything reachable by more
than one person.

Set `API_BASE_URL` to the address Grafana will use (for example
`http://10.0.0.12:8080`) before starting, because it is what the UI prints as the
ingestion URL. Both ports bind to loopback by default; set `BIND_ADDR=0.0.0.0` to publish 8080,
and set `API_TOKEN` before you do, because the product API can create ingestion
sources and acknowledge or resolve incidents. Ingestion authenticates with each
source's own token. Docker publishes ports underneath `ufw`, so a `ufw` rule does
not restrict them; `docs/pilot-runbook.md` section 1 has the working alternative.
The worker is a one-shot tool under the `tools` profile, not a daemon.

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

### Grafana

A source of type `grafana` accepts Grafana Alerting's native webhook contact-point
payload. One POST carries a notification group; every alert in it becomes its own
RawSignal with the group context attached, and `fingerprint:status:startsAt` is the
idempotency key, so Grafana's repeat notifications collapse while resolutions and
re-fires do not.

Grafana's rendered `message` digest is not stored — it is a rendering of
`alerts[]`, which is stored in full — but Grafana sends it in the same POST, so it
counts against the 4 MiB request body limit on the way in. There is no
alert-count cap in the adapter. Measured against the fixture, that limit admits
roughly 2,160 alerts of a fat ~1.3 KB shape or 3,260 of a slim ~685 B shape, digest
included. Above it the request is refused whole with a 413, logged as
`request body exceeds the ingest limit`; Grafana retries a few times and then
discards the notification, so that log line means a group was lost.

Set **Max alerts** on the Grafana contact point so that can never happen. That
same fat shape, once its share of the digest is folded in, costs about 1.9 KB;
`Max alerts` at 1,000 uses a deliberately conservative 2.5 KB per alert, so a
body is at most about 2.5 MB. Beyond 1,000 alerts Grafana
truncates the group itself before sending; the truncation is warn-logged
(`grafana dropped alerts from this notification group`) and the `truncatedAlerts`
count is preserved into every stored alert's group context, pinned by a test, so
the loss is visible and bounded instead of silent and total. Re-derive the number
from your own alert sizes: `Max alerts` times bytes per alert must stay under
4 MiB with margin.

A batch whose group context multiplied across its alerts would separately exceed
32 MiB is refused with a 400 and logged as `grafana batch rejected`; at realistic
Grafana payload shapes this is not expected to trigger and exists only as a
safety net.

```sh
curl -X POST localhost:8080/api/v1/sources -H 'content-type: application/json' \
     -d '{"name":"grafana-live","source_type":"grafana"}'
```

Point a Grafana Webhook contact point at the returned `ingest_url` (method POST).
The response reports `inserted` and `duplicates` per batch. The fixture in
`adapters/sources/grafana/tests/fixtures/` is written from Grafana's documented
format; replace it with a real payload from the pilot environment when one exists.

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

## Pilot Lab

The synthetic fixture proves the engine does what we designed. The Lab asks the
different question: **does it do something useful to real traffic?**

```sh
# 1. CAPTURE — create a source of type `grafana` (or `generic_webhook`) in the UI and
#    point the Grafana contact point at its ingestion URL. See docs/pilot-runbook.md.

# 2. Freeze what arrived into an immutable dataset
cargo run -p ops-worker -- pilot dataset create infra-week-01 [source-name]
cargo run -p ops-worker -- pilot dataset list

# 3. REPLAY the same signals through the real pipeline, as often as you like
cargo run -p ops-worker -- pilot replay infra-week-01 run-a
cargo run -p ops-worker -- pilot replay infra-week-01 run-b --ai

# 4. COMPARE two runs
cargo run -p ops-worker -- pilot compare run-a run-b
```

**Each run gets its own tenant.** Ingestion is idempotent on
`(source_id, external_id)`, so replaying into the capture tenant would insert
nothing the second time. A fresh throwaway organization per run makes replay
repeatable and reuses the tenant isolation the engine already enforces
everywhere, instead of inventing a second isolation mechanism. Replay tenants
are flagged `is_replay` so they are never mistaken for the real one.

The capture tenant keeps the evidence and is never processed — replays are
disposable, captures are not. Both properties are asserted by
`adapters/persistence/tests/pilot_replay.rs`: two replays of one frozen dataset
produce identical statistics, and the capture tenant ends with 0 events and
0 incidents.

A replay runs the **real** pipeline: the same normalizer, the same correlator,
the same reasoner. If the Lab had its own copy it would be measuring the wrong
engine.

`compression` (events per incident) is reported next to correctness, never
instead of it. A correlator that merges everything scores best on compression
and is useless — which is exactly why `compare` says "fewer incidents is not
automatically better".

### Running a local model

The pilot cannot send real incident context to a cloud provider until the
source-inventory §21 boundary is agreed. A model on localhost has no boundary to
cross, so reasoning can be evaluated against **real captured signals** now, and
the cloud decision made later on evidence:

```sh
AI_ENABLED=true AI_PROVIDER=local AI_MODEL=<model loaded in LM Studio/Ollama>   cargo run -p ops-worker -- pilot replay infra-week-01 run-local --ai
```

`AI_BASE_URL` defaults to `http://127.0.0.1:1234/v1` (LM Studio); Ollama is
`:11434/v1`, vLLM `:8000/v1`. Sampling is fixed at `temperature: 0` so two runs
over one dataset stay comparable. It is the same `ReasoningProvider` port — the
reasoner cannot tell local from cloud, which is the point of having the port.

### Not built yet, deliberately

**Real capture (P2).** The first frozen dataset from live Grafana traffic on the
design partner's infrastructure. Everything below needs it. Plan:
`docs/plans/2026-09-08-pilot-handoff.md`.

**Golden cases (P3).** A dataset with human-approved expected groupings. The
system proposes, the engineer corrects, and *that* becomes the oracle. A model
must never write its own answer key.

**Shadow mode (P4).** Real signals flowing alongside the existing workflow, with
the capture tenant processed live so the UI shows incidents as they happen.
Needs P2 first.

## Checks

`sh scripts/check.sh` runs the whole gate; `.github/workflows/ci.yml` runs the same
gate plus the PostgreSQL acceptance tests on every push once the repo has a remote.

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
