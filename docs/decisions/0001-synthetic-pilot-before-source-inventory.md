# 0001 — Start with a synthetic, source-agnostic pilot before the real source inventory

- **Status:** Accepted
- **Date:** 2026-09-03
- **Deciders:** Product owner (bryan.c@evokehub.com)
- **Affects:** `operations-intelligence-product-tech-sheet-v0.1.md` §38 (Ready-to-Dev Gate), §31 Day 0

---

## Context

The frozen v0.1 documents gate implementation behind a completed Design Partner #0
interview. Tech sheet §38 currently reads:

```
Pilot user available       ⏳
Real source inventory      ⏳
Top 2 integrations chosen  ⏳
```

and states plainly:

> The correct next action is **not coding yet**.
> The next action is: conduct the Design Partner #0 source/workflow interview.

`pilot-source-inventory-v0.1.md` exists but is an empty template — no source has been
recorded, no sample payload collected, no severity vocabulary observed.

At the same time, the real production integrations are not yet accessible, and the
first Job To Be Done is already well understood and does not depend on which vendor
the alerts came from:

> Given many alerts from different infrastructure systems, help one infrastructure
> engineer understand which real incidents happened, what is still unresolved, what
> keeps recurring, and what deserves attention first.

Waiting for the interview would leave the core of the product — the deterministic
correlation engine — unwritten and unvalidated, for reasons that do not actually apply
to it.

## Decision

**We begin implementation now, against a synthetic CSV dataset, and treat the §38 gate
as satisfied for source-agnostic work only.**

Specifically:

1. The first ingestion path is a **CSV/spreadsheet importer**, plus a **Generic Webhook**
   as the first live-style interface. Both are adapters that write `RawSignal`.
2. Everything downstream of `RawSignal` — normalization, correlation, incidents,
   deterministic insights, Operations UI — is built and validated against
   `pilot-data/synthetic-alerts-v1.csv` and its expected-outcome fixture.
3. **No vendor-specific adapter** (Grafana, Azure Monitor, Email) is written until the
   source inventory is complete and the top two integrations are chosen from evidence.

The gate is therefore **narrowed, not waived**. Its unchecked items still block exactly
what they were written to block.

## Rationale

The §38 gate exists to prevent one specific failure: **guessing integrations**. Building
a Grafana adapter before seeing the user's Grafana produces an adapter shaped by
imagination.

A CSV importer guesses no integration. It commits to no vendor, no payload shape, no
auth model, no delivery channel. If the pilot user turns out to run Zabbix and PRTG
instead of Grafana and Azure Monitor, nothing built under this decision is wasted —
because the vendor boundary sits at `RawSignal`, and everything validated here lives
downstream of it.

The risk the gate protects against is real but is confined to the adapter layer, which
point 3 keeps closed.

## Consequences

### Accepted

- The correlation engine will initially be tuned against invented data. Fingerprint
  components and correlation windows may need adjustment once real alerts arrive.
- Severity mappings and the `event_family` taxonomy are placeholders. Source inventory
  §11 says explicitly not to fix these before seeing real data; we are provisionally
  fixing them anyway, so they must stay cheap to change (see
  [0002-event-family-derivation.md](0002-event-family-derivation.md)).
- Synthetic results are **not** pilot evidence. Architecture §16 is unambiguous here:
  internal pilot evidence, synthetic demonstrations and external customer results must
  never be presented as if they are the same thing. Nothing produced from
  `pilot-data/` may be described as a pilot result.

### Required to keep this decision safe

- Vendor-specific parsing stays inside `adapters/sources/<vendor>/`. Canonical taxonomy
  and normalization stay reusable and testable outside any vendor code.
- Severity maps and correlation windows are **configuration**, not constants compiled
  into core.
- When the source inventory is completed, this decision is revisited and the §38 gate
  items are checked off for real before any vendor adapter is merged.

### Rejected alternatives

**Wait for the interview.** Leaves the highest-risk component — deterministic
correlation — unbuilt and unvalidated, for a reason that does not apply to it.

**Build a Grafana adapter speculatively.** Exactly what §38 exists to prevent.

**Use recorded real payloads instead of synthetic data.** Preferable, but unavailable:
no payloads have been collected, and source inventory §21 requires an agreed data
boundary before any real payload is retained.

## Revisit when

- The Design Partner #0 interview is complete and `pilot-source-inventory.md` is filled in.
- The top two integrations are ranked from evidence (source inventory §18).
- Any real alert payload becomes available — at which point real fixtures should be
  added alongside, not instead of, the synthetic ones.
