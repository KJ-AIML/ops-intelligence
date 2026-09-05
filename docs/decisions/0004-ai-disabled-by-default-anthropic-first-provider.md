# 0004 — AI is disabled by default; Anthropic is the first provider adapter

- **Status:** Accepted
- **Date:** 2026-09-05
- **Affects:** architecture §2 (AI is bounded reasoning), §7 (AI ports), tech sheet §11, §24, §33; source inventory §21 (data boundary)

---

## Context

Slice 6 adds the intelligence layer. Two things about it are unusual compared
with every slice before it: it **spends money per call**, and it **sends data to
a third party**.

The frozen documents constrain both. The tech sheet's decision log lists the
exact AI provider and model as **not frozen** — to be "decided from pilot
constraints rather than architecture preference". Source inventory §21 is
stricter still:

> Before sending any real incident context to an external model: identify
> sensitive fields, define redaction, define allowed context, confirm provider
> policy, confirm company approval if required.
>
> The system must be able to run deterministic correlation with AI disabled.

None of those five preconditions is satisfied yet. The Design Partner #0
interview has not happened, so we do not know what a real alert payload
contains, let alone which fields are sensitive.

## Decision

**Three parts.**

**1. AI is off by default and the product is complete without it.**
`AI_ENABLED` defaults to `false`. With it unset, `DisabledProvider` is selected:
no key is read, no request is made, and no data leaves the deployment. Every
number the Operations View shows — unresolved, recurring, noisiest source,
triage rank — is deterministic and unaffected. `ops-worker reason` reports that
AI is disabled and exits successfully, because "AI is off" is a normal state,
not an error.

**2. The data boundary is a struct, not a policy document.**
`IncidentContext` in `crates/core/src/intelligence/reasoner.rs` is the complete
set of facts the model can ever see. It carries derived values only —
status, severity, family, environment/service/resource, durations, counts,
recurrence, event **titles** and source names. It carries **no raw payloads, no
source messages, no log lines**. Anything not copied into that struct is
structurally incapable of reaching a provider, and a test asserts it.

**3. Anthropic is the first provider adapter, behind the port.**
`adapters/ai/anthropic` implements `ReasoningProvider` over the documented
Messages API using structured outputs. It was chosen as a working default so the
slice could be finished and tested, **not** as the frozen answer to a question
the tech sheet deliberately left open.

## Rationale

### Why disabled by default

Shipping this enabled would send incident context to a third party before anyone
agreed that was acceptable, and would start billing on a system nobody has
evaluated. Both are decisions for the product owner, and neither is reversible
after the fact.

Disabled-by-default also keeps the architecture's own claim honest: §33 requires
"system works with AI disabled" as a definition-of-done item. The only way to
know that is true is for it to be the default path, exercised every day, rather
than a fallback nobody runs.

### Why the reasoner owns prompt, schema and validation

The provider owns transport and credentials. The reasoner owns everything that
determines what the model is allowed to say. This split makes every rule
testable with a stub provider — no key, no network, no cost. The whole
intelligence layer is covered by tests that never make an API call.

Validation is enforced, not requested:

- the response must parse into the typed schema, or it is rejected;
- `actionability` must be one of three values;
- fields must be non-empty and bounded in length;
- **any infrastructure-looking identifier in the output must appear in the
  supplied facts**, which catches the specific failure that would destroy trust
  in this product — a model inventing a hostname.

An invalid response is retried once, then recorded as `status='failed'` with the
reason. Raw model text is never persisted as a success (tech sheet §11).

### Why the model choice is what it is

`claude-opus-5` at `effort: low` is the default: a short, well-specified,
schema-constrained explanation is not a hard reasoning task, and low effort keeps
per-incident cost down. Both are environment variables (`AI_MODEL`, `AI_EFFORT`).
Server-side refusal fallbacks are enabled so a policy decline re-runs on a
fallback model within the same call rather than losing the insight.

### What AI still may not do

Unchanged from architecture §2 and guardrail §34.4. AI does not assign an
`event_family`, a severity, a status, a fingerprint, or any other value that
correlation depends on. It does not suppress anything. It cannot reopen, merge or
resolve an incident. It produces one interpretive artifact, clearly labelled with
its provider, model and prompt version, stored beside the facts and never
replacing them.

## Consequences

### Accepted

- Insights are absent until someone turns AI on. That is the intended state.
- The identifier-grounding check is a heuristic, not a semantic guarantee. It
  catches invented hosts and services; it says nothing about invented prose. It
  is marked as such in the code.
- Cost is bounded by `ops-worker reason [limit]` (default 10) and by the
  one-successful-insight-per-incident unique index, so re-running is idempotent
  and cannot silently double the bill. There is no automatic scheduling.

### Required before enabling AI on real data

1. Complete the Design Partner #0 interview and the §21 data-boundary review.
2. Re-read `IncidentContext` against real payloads and confirm every field in it
   is safe to send.
3. Confirm the provider's data-retention policy meets whatever the company
   requires.
4. Get explicit approval to enable it.

### Rejected alternatives

**Ship with AI on.** Sends data and spends money on decisions nobody made.

**Build only the port and no adapter.** Leaves the slice unverifiable end to end
and the port shaped by imagination rather than by one real implementation.

**Make the provider pluggable across several vendors now.** Speculative. One
adapter behind a port is enough; adding OpenAI or Azure OpenAI later is one file
implementing `ReasoningProvider`, and nothing else changes.

## Revisit when

- The data boundary is agreed and AI is enabled on real incidents.
- Insight quality is evaluated against tech sheet §30 (explanation correctness,
  no invented facts, usefulness), which needs real incidents to judge.
- The pilot user says whether the interpretation is worth its cost — the only
  test that actually matters.
