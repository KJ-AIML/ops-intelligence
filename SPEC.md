# Slice 2: RawSignal to Event

Scope: complete normalization only; Q1 is resolved by decision 0003. No correlation,
incidents, Insight implementation, AI, UI, or live vendor adapters.

Slices 0 and 1 already exist as separate commits dfe9d71 and 9804708.
Preserve existing uncommitted domain/normalization code and Q1 documentation.

## Contracts

- Core owns Event, EventId, Severity, EventState, EventFamily and pure normalization.
- `ports::SignalNormalizer: Send + Sync` exposes
  `normalize(&self, signal: &RawSignal, source_type: SourceType, created_at: DateTime<Utc>) -> Result<Event, DomainError>`.
- CSV adapter implements `CsvNormalizer` and extracts `SourceFacts`/`FamilyHints`.
  It parses the source RFC3339 timestamp; missing/invalid timestamps fail, never use received_at.
  Existing per-origin configuration maps dialects; classification remains in core.
- `ports::EventRepository: Send + Sync` exposes async
  `process_next(&self, organization_id: OrganizationId, normalizer: &dyn SignalNormalizer, clock: &dyn Clock) -> Result<Option<ProcessingStatus>, DomainError>` and
  `list(&self, organization_id: OrganizationId) -> Result<Vec<Event>, DomainError>`.
- `normalization::process_received(repository, organization_id, normalizer, clock)`
  drains process_next, returning `ProcessingReport { processed: usize, failed: usize }`.
  Persistence errors propagate, never count as successful processing.
- PgStore claims one received row with tenant-scoped FOR UPDATE SKIP LOCKED within
  a transaction. received -> processing -> processed/failed, Event insert and terminal
  status commit atomically. Crash/DB failure rolls back to received. No separate durable
  processing lease or queue. Normalizer runs synchronously without external I/O.
- Invalid payloads/unsupported sources persist failed with a reason and no Event.
  Validate returned Event tenant/source/raw IDs against the locked signal.
- events has unique raw_signal_id and composite foreign keys enforcing matching tenant,
  source and evidence. Add matching source/RawSignal composite constraints in migration 0002.
- Worker `process` drains configured tenant, prints processed/failed and all status counts,
  and exits unsuccessfully when failures or nonterminal backlog remain.

## Verification and boundaries

Fixture is the oracle: all 69 rows checked individually; ingestion suppresses two
redeliveries, leaving 67 RawSignals/Events. All canonical values and historical times
must match. DB integration test uses a dedicated local test database, exercises full
pipeline, repeat processing, competing processors, failure persistence, rollback and
tenant isolation. No shared database cleanup. Run fmt, clippy, build, all tests.

Rollback: stop worker; retain raw evidence and existing migrations. Revert Slice 2 code
before processing new inputs if needed. Do not delete raw evidence or alter Slice 1 migration.
