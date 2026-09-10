-- Task 10 Part B changed the correlator's draw order to
-- (occurred_at, external_id, title, id) so that two replays of one dataset
-- correlate identically. The pending-work index from migration 0003 covers
-- only (organization_id, occurred_at, id); with the new sort key the planner
-- fell back to the non-partial occurred_at index and filtered out every
-- already-correlated event on every draw, so a drain got slower as it went.
-- This index matches the new sort key exactly. It replaces the old one, whose
-- leading columns it shares, so nothing that used the old index loses it.
CREATE INDEX events_correlation_order_idx
    ON events (organization_id, occurred_at, external_id, title, id)
    WHERE correlation_status = 'received';

DROP INDEX events_correlation_pending_idx;
