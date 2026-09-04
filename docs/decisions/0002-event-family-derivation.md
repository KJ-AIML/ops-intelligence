# 0002 — `event_family` derivation: adapters extract, normalization classifies, core consumes

- **Status:** Accepted
- **Date:** 2026-09-03
- **Affects:** tech sheet §8 (Event), §10 (Correlation), §12 (Ports), architecture §7 (Ports and Adapters)

---

## Context

The correlation fingerprint is:

```
organization_id + managed_customer? + environment? + service? + resource? + event_family
```

`event_family` is load-bearing. Duplicate detection, recovery pairing and recurrence
detection all key off it. If it is derived inconsistently, two alerts describing the same
condition land in different incidents, and the product's central promise fails.

The frozen documents name `event_family` and give examples (`latency`), but **specify no
derivation rule**. Nothing states how to get from a free-text alert title such as:

```
[FIRING:1] PaymentAPIHealthCheck (payment-api api-prod-01)
CPU percentage greater than 90 (api-prod-01)
[payment-api] [Down] Connection refused
ALERT: Nightly backup FAILED on db-prod-01
```

to a canonical family. This is the largest unspecified area in the design and it cannot
be deferred past the first correlation slice.

The obvious implementation — let each vendor adapter decide the family — is wrong. It
would put the same classification policy in four places, drift between them, make the
taxonomy untestable without vendor code, and let vendor concepts leak into the domain
in violation of architecture §7 and coding guardrail §34.3.

## Decision

**Split the responsibility across three layers, with a canonical taxonomy that is
independently testable.**

```
Source Adapter          →  extracts raw source FACTS and HINTS
                           (vendor-specific parsing; no taxonomy knowledge)
        ↓
Normalization layer     →  maps facts/hints to a CANONICAL event_family
                           (vendor-agnostic; owns the taxonomy; testable alone)
        ↓
Core correlation        →  consumes canonical event_family only
                           (never sees a vendor string)
```

### Layer 1 — Source adapter

Owns vendor mechanics only. Parses the payload and emits a neutral hint structure:

```
FamilyHints {
    title, message,
    labels,                  // vendor label/tag map, flattened
    metric_name?,            // e.g. Grafana rule metric, Azure signal name
    monitor_type?,           // e.g. uptime_kuma http/tcp/ping
    vendor_category?,        // vendor's own category, verbatim, never trusted as canonical
}
```

Adapters must **not** contain a keyword table mapping to canonical families, and must not
import the family enum for classification purposes.

### Layer 2 — Normalization

Owns the taxonomy and the classifier. Pure, deterministic, no I/O:

```
classify(hints, source_type_config) -> EventFamily
```

Resolution order, first match wins:

1. **Explicit label** — an operator-supplied `event_family` label on the alert. Always wins.
2. **Per-source mapping rules** — configured on the `Source` record. This is where a real
   Grafana rule name or Azure signal name maps to a family once observed.
3. **Canonical keyword patterns** — the shared default table, vendor-independent.
4. **`unclassified`** — never guess. An unclassified event still becomes an Event, still
   correlates on the rest of the fingerprint, and is surfaced so the mapping can be fixed.

Layer 3 rules live in **one** table used by every source. Layer 2 rules live in source
configuration, so a new mapping is a config change, not a code change.

### Layer 3 — Core

Consumes `EventFamily` as a domain value. Core never sees `Sev1`, `alerting`, `Fired`
or `Down`.

## Canonical taxonomy v1

| Family | Covers |
|---|---|
| `availability` | up/down, unreachable, health check failing, connection refused |
| `latency` | slow response, p95/p99 thresholds, timeouts under load |
| `error_rate` | 5xx ratios, exception rates, failed request ratios |
| `saturation_cpu` | CPU utilization thresholds |
| `saturation_memory` | memory/RSS/heap utilization thresholds |
| `saturation_disk` | disk space, inode, volume pressure |
| `connectivity` | DNS, network path, connection pool exhaustion, replication link |
| `certificate` | TLS/certificate expiry and validation |
| `backup` | backup and restore job outcomes |
| `job` | scheduled job, deployment, autoscale lifecycle events |
| `unclassified` | explicit fallback — never a guess |

Deliberately flat and small. Hierarchy (`saturation.cpu`) is not introduced until a real
query needs to roll up across a parent, per architecture §18.

The `saturation_*` families are split rather than one `saturation` family because they are
fingerprint components: CPU pressure and disk pressure on the same host are different
operational problems and must not collapse into one incident.

## Testability requirement

The classifier must be testable with **no vendor code, no database and no HTTP**:

```
classify(FamilyHints { title: "CPU percentage greater than 90 (api-prod-01)", .. })
    == EventFamily::SaturationCpu
```

`pilot-data/expected-outcomes.json → normalization_expectations` provides the expected
canonical `event_family`, `severity` and `state` for **every** fixture row. That makes the
normalization layer verifiable before any correlation code exists, and is the test oracle
for this decision.

## Severity and state normalization

Same three-layer split, same reasoning. Adapters pass vendor values through untouched;
normalization maps them; core sees only `debug | info | warning | critical` and
`firing | resolved | informational`.

Mappings are **per-source configuration**, not constants. Two reasons:

- Source inventory §11 says explicitly not to fix severity mappings before seeing real
  source data. Ours are placeholders.
- `uptime_kuma` carries no severity at all, so a per-source default is unavoidable — and
  the current default makes every `Down` critical, including the internal wiki. That is
  a tuning problem, and tuning must not require a recompile.

## Consequences

### Accepted

- One extra hop between adapter and core. Deliberate: it is the seam that keeps the
  taxonomy reusable and lets a new vendor be added without touching classification.
- The v1 keyword table will misclassify real alerts. Expected. `unclassified` is a valid
  outcome and should be **counted and surfaced** as a data-quality signal, not hidden.
- Classification is deterministic and rule-based. Per architecture §2 and guardrail §34.4,
  an LLM is not permitted to assign `event_family` — it is a fingerprint input, and
  fingerprints are deterministic.

### Non-negotiable

- No vendor adapter contains the canonical taxonomy or a mapping to it.
- No canonical family is added because one vendor happens to name something that way.
- Changing a family assignment is a correlation change and requires a regression fixture
  (guardrail §34.7).

## Revisit when

- Real payloads arrive and the v1 keyword table is measured against them.
- `unclassified` volume becomes material.
- A real query needs to roll up across families (would motivate hierarchy).
- Q1 is resolved by decision 0003: core correlation stays within one family;
  cross-family relationships belong in the Insight layer.
