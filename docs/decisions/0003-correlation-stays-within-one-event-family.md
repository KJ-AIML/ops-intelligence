# 0003 — Correlation stays within one canonical `event_family`

- **Status:** Accepted
- **Date:** 2026-09-04
- **Deciders:** Product owner (bryan.c@evokehub.com)
- **Resolves:** Q1 in `pilot-data/README.md`
- **Affects:** architecture §3 (narrative example), tech sheet §10 (correlation)

---

## Context

The frozen documents disagreed with themselves on one point.

Architecture §3 narrates:

```
Grafana: API latency high
Azure:   CPU high
Grafana: API latency high
Azure:   CPU recovered
        ↓
1 Incident — "Payment API degradation"
```

But the tech sheet §10 fingerprint is:

```
organization + managed_customer? + environment? + service? + resource? + event_family
```

Those two cannot both hold. Latency and CPU are different canonical event
families, so the §10 fingerprint necessarily produces **two** incidents, not the
one the narrative describes.

The Slice 0 fixture surfaced this concretely as scenarios S01 (availability on
`api-prod-01`) and S02 (`saturation_cpu` on the *same* `api-prod-01`, two minutes
later), and recorded it as open question Q1 blocking Slice 3.

## Decision

**Core correlation is conservative. It never merges events with different
canonical `event_family` values.**

```
availability incident  ≠  saturation_cpu incident
```

Shared `service`, shared `resource` and close timestamps are **not** grounds for
merging across families. The §10 fingerprint stands exactly as written.

Cross-family relationships — *"CPU saturation may have contributed to the
availability incident"* — belong to the **Insight layer**, not to correlation.

## Rationale

The two failure modes are not symmetric.

**Under-merging** produces two incidents where an engineer would have preferred
one. The cost is a slightly longer list. Both incidents are real, both carry
correct evidence, and the relationship can still be surfaced as an insight.

**Over-merging** produces one incident that silently absorbs a second, unrelated
problem. The absorbed problem loses its own lifecycle: it cannot be
independently acknowledged, resolved or counted, and when the merged incident
recovers, the still-broken thing looks recovered too. That is a missed incident,
which is the exact failure the product exists to prevent.

Correlation is also deterministic and irreversible in the evidence chain. An
insight is interpretive, clearly labelled as such, and costs nothing if wrong.
Speculative causal reasoning belongs on the side of the boundary where being
wrong is cheap.

This also keeps architecture §2 intact: deterministic logic owns grouping, and
AI may interpret but never rewrites source facts or incident structure.

## Consequences

- S01 and S02 in the Slice 0 fixture are **two incidents**. The expected-outcome
  oracle already encodes this and does not change.
- Architecture §3's narrative example is superseded on this point. Its intent —
  showing an engineer one coherent story instead of four raw alerts — is served
  by an insight that links the incidents, not by merging them.
- A future `IncidentRelation` (`may_have_contributed_to`, `co_occurred_with`)
  may be introduced in the Insight layer. It must be additive: it links
  incidents, never merges them, and never alters incident lifecycle or status.
- Correlation quality is judged by the `must_not_correlate` pairs as much as by
  the positive groupings. R016/R018 is the canonical case.

## Revisit when

The pilot user reviews real grouped incidents and reports that the split costs
more attention than it saves — and even then, the first response is a better
insight linking them, not a looser fingerprint.
