# Pilot Data — Synthetic Correlation Fixture v1

**Status:** Slice 0 deliverable. This is the acceptance criteria for Slices 2–4.
**Depends on:** `operations-intelligence-architecture-v0.1.md` (FROZEN), `operations-intelligence-product-tech-sheet-v0.1.md` §10, §29.

---

## What this is

A synthetic 24-hour operational day (**2026-09-02**, `+07:00`) from four fictional alert
sources, plus the exact result the pipeline is expected to produce from it.

```
synthetic-alerts-v1.csv   ← input:  69 raw alert rows
expected-outcomes.json    ← oracle: what RawSignal → Event → Incident must produce
```

It exists so that the deterministic correlation engine is written **against a stated
expected result**, rather than the expected result being written afterwards to describe
whatever the code happened to do.

This is the "deterministic evaluation dataset" from tech sheet §29 and the beginning of
the permanent regression suite from §28.

## What this is not

- Not real customer or production data. Every host, service and alert is invented.
- Not a substitute for the Design Partner #0 source inventory. See
  [0001-synthetic-pilot-before-source-inventory.md](../docs/decisions/0001-synthetic-pilot-before-source-inventory.md).
- Not a test of vendor payload parsing. See **Limitations** below.

---

## How it was built (and why the numbers are trustworthy)

Scenarios were defined **first** — each with explicit rows and explicit incident
membership, stating which row is the trigger, which are duplicates, which is the
recovery. Every summary count in `expected-outcomes.json` (`expected_incidents`,
`expected_open_incidents`, `expected_duplicate_relations`, …) was then **computed from
those memberships**.

No summary total was picked first and backfilled with data. The `totals` block is a
derived view of the scenario table below, nothing more.

Two invariants are enforced at generation time and would have failed the build:

1. **No silent loss** — every one of the 69 rows is accounted for exactly once, as either
   an incident member, an explicitly-reasoned no-incident event, or an
   ingestion-suppressed redelivery.
2. **Declared severity equals derived severity** — each incident's severity is
   recomputed as the max across its triggering members and checked against the declaration.

---

## Scenario breakdown

| ID | Scenario | Rows | Events | Incidents | Status |
|---|---|---:|---:|---:|---|
| S01 | Critical multi-source outage, recovers | 5 | 5 | 1 | recovered |
| S02 | CPU saturation on the same resource as S01 | 3 | 3 | 1 | recovered |
| S03 | Recurring API latency, 4 occurrences | 9 | 9 | 4 | recovered |
| S04 | Recovered then re-fired inside reopen window | 3 | 3 | 1 | open |
| S05 | Unresolved memory leak, fires outside dup window | 3 | 3 | 1 | open |
| S06 | Email redelivery ×3 (idempotency) | 3 | **1** | 1 | open |
| S07 | Four unrelated alerts within 16 seconds | 5 | 5 | 4 | open ×3, recovered ×1 |
| S08 | Same service + family, different resource | 3 | 3 | 2 | open ×1, recovered ×1 |
| S09 | Staging mirror of a production incident | 2 | 2 | 1 | recovered |
| S10 | Informational noise — operational chatter | 7 | 7 | 0 | — |
| S11 | Orphan recovery — UP with no preceding DOWN | 1 | 1 | 0 | — |
| S12 | Flap storm — six fires in five minutes | 7 | 7 | 1 | recovered |
| S13 | Recurrence pattern #2 — memory, 3 occurrences | 6 | 6 | 3 | recovered |
| S14 | Severity escalation on an open incident | 3 | 3 | 1 | open |
| S15 | Informational noise — digests and advisories | 7 | 7 | 0 | — |
| S16 | Fast flap — down and up in 45 seconds | 2 | 2 | 1 | recovered |
| | **Total** | **69** | **67** | **22** | 8 open, 14 recovered |

Coverage of the requested cases:

| Requested case | Scenarios |
|---|---|
| exact duplicates | S01, S02, S06, S12, S14 |
| DOWN → UP recovery | S01, S03, S04, S07, S12, S16 |
| repeated CPU / latency warnings | S03, S05, S13 |
| one critical multi-source incident | S01 |
| near-simultaneous but unrelated | S02, S07, S08, S09 |
| informational / noisy signals | S10, S11, S15 |
| unresolved incidents | S04, S05, S06, S07, S08, S14 |
| recurring incidents across the day | S03 (×4), S13 (×3) |

---

## Two kinds of duplicate — deliberately both present

These are different mechanisms at different layers, and conflating them is an easy bug.

| | S06 — redelivery | S12 — flap storm |
|---|---|---|
| `external_id` | identical (`EM-88213`) | all different |
| Payload | byte-identical | differs (changing metric value) |
| Collapsed at | **ingestion** (RawSignal, §9) | **correlation** (Incident, §10) |
| Rows → Events | 3 → **1** | 7 → **7** |
| Result | 2 rows never become Events | 5 `duplicate` relations on one incident |

This is why `expected_events` (67) is less than `csv_rows` (69).

---

## Correlation rules this fixture assumes

Encoded in `expected-outcomes.json → correlation_rules_assumed`. Windows:
`duplicate_window = 600s`, `reopen_window = 1800s`.

| | Rule |
|---|---|
| R1 | Match an **open** incident by fingerprint alone, with no time bound. (`find_open_by_fingerprint` in tech sheet §12 takes no window parameter.) |
| R2 | firing + open incident → attach; `duplicate` if within `duplicate_window` of the last event, otherwise `update` |
| R3 | firing + no match → create incident, relation `trigger` |
| R4 | resolving + open incident → attach as `recovery`, incident → `recovered` |
| R5 | firing + recovered incident **within** `reopen_window` → reopen, relation `trigger` |
| R6 | firing + recovered incident **outside** `reopen_window` → new incident — *this is what produces a recurrence* |
| R7 | resolving with no matching incident → no incident, event still accounted |
| R8 | normalized severity `info`/`debug` → no incident, event still accounted |
| — | Incident severity = **max** normalized severity across `trigger`/`duplicate`/`update` members (S14) |

R6 is the load-bearing one: it is the difference between "4 latency incidents forming a
recurring pattern" (correct, S03) and "1 incident that lasted 16 hours" (wrong).

---

## Fingerprint

```
organization_id + environment + service + resource + event_family
```

Three scenarios exist purely to defend individual components of this tuple:

- **S08** defends `resource` — same rule, same service, two hosts, 48s apart → 2 incidents.
- **S09** defends `environment` — byte-identical alert title 34s apart, staging vs production → 2 incidents.
- **S02** defends `event_family` — same env, service *and* resource as S01, different family → 2 incidents.

---

## `must_not_correlate` — the anti-correlation guard

Ten pairs that a naive correlator will wrongly merge. A correlator that groups on
timestamp proximity, or on alert title, passes the positive cases and fails these.

| A | B | Why they must stay apart |
|---|---|---|
| R016 | R018 | Same env/service/resource, different `event_family` (see **Q1**) |
| R025 | R026 | Same alert rule and service, 48s apart, different resource |
| R028 | R040 | Same family (`saturation_disk`), different service and resource |
| R035 | R036 | Identical alert title 34s apart, different environment |
| R039 | R040 | 4s apart, unrelated |
| R039 | R041 | 8s apart, unrelated |
| R039 | R042 | 16s apart, unrelated |
| R040 | R041 | 4s apart, unrelated |
| R040 | R042 | 12s apart, unrelated |
| R041 | R042 | 8s apart, unrelated |

---

## Normalization is part of the oracle

The CSV carries **vendor-shaped** severity and state values. Each source speaks its own
dialect, exactly as the real ones do:

| Source | `severity` values | `state` values |
|---|---|---|
| `grafana` | `critical`, `warning`, `info` | `alerting`, `ok`, *(empty)* |
| `azure_monitor` | `Sev0`–`Sev4` | `Fired`, `Resolved`, *(empty)* |
| `uptime_kuma` | *(always empty)* | `Down`, `Up`, *(empty)* |
| `email` | `CRITICAL`, `WARNING`, `INFO` | `FAILED`, `OK`, *(empty)* |

`expected-outcomes.json → normalization_expectations` gives the expected canonical
`event_family`, `severity` and `state` **per row**, so the normalization layer is testable
on its own, before any correlation code exists. See
[0002-event-family-derivation.md](../docs/decisions/0002-event-family-derivation.md).

---

## Derived insight expectations

Deterministic — no AI required to produce any of these.

| Insight | Expected |
|---|---|
| Unresolved | 8 incidents |
| Recovered | 14 incidents |
| Critical | 6 incidents |
| Recurring | `payment-api / api-prod-01 / latency` ×4; `web-frontend / web-prod-01 / saturation_memory` ×3 |
| Noisiest source | `grafana` — 31 of 69 rows (44.9%) |
| Signal compression | 69 rows → 67 events → 22 incidents |

Source share: grafana 31 (44.9%), azure_monitor 21 (30.4%), uptime_kuma 9 (13.0%), email 8 (11.6%).

---

## Open questions this fixture surfaces

Full text in `expected-outcomes.json → known_design_questions`.

**Q1 — blocks Slice 3.** Architecture §3 narrates *"Grafana latency + Azure CPU → one
incident: Payment API degradation"*. The §10 fingerprint **cannot** produce that, because
the event families differ. This fixture encodes the §10 rule, so **S01 and S02 are two
incidents, not one**. If service-level grouping across families is wanted, it is a new
correlation rule and needs an architecture decision before the correlator is built.

**Q2.** S16 recovers in 45 seconds. Source inventory §10 asks whether sub-minute
recoveries should behave differently. Currently treated as a normal incident.

**Q3.** `uptime_kuma` has no severity field, so `Down` maps to `critical` for every
monitor — which makes the internal wiki being down "critical" (S07). Per-source severity
defaults must therefore live in source **configuration**, not in core.

---

## Limitations

- **Pre-flattened identity fields.** `service`, `resource` and `environment` arrive clean.
  Real Grafana label maps and Azure ARM resource IDs need extraction in the vendor adapter.
  This fixture exercises the normalization layer (severity/state/event_family), **not**
  vendor resource-ID parsing — that arrives with real payload fixtures (tech sheet §28).
- **Invented severity mappings.** Source inventory §11 says explicitly not to fix the
  mapping before seeing real source data. The mappings here are placeholders chosen to
  make the fixture deterministic, and are expected to be replaced.
- **One organization.** Multi-tenant isolation is not exercised. Add a second
  `organization_id` fixture when tenant-scoping is implemented.

---

## Using it

Slice 3 acceptance is: import `synthetic-alerts-v1.csv`, then assert the pipeline output
matches `expected-outcomes.json` — including all ten `must_not_correlate` pairs.

Every future correlation bug should be added here as a new scenario with its expected
outcome, and never removed.
