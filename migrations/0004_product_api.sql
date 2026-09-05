-- 0004_product_api — webhook ingestion tokens and the manual incident lifecycle.

-- Generic Webhook ingestion. The token is the source's credential, so it must be
-- unguessable and is never logged (tech sheet 21).
ALTER TABLE sources ADD COLUMN ingest_token TEXT;
CREATE UNIQUE INDEX sources_ingest_token_key ON sources (ingest_token)
    WHERE ingest_token IS NOT NULL;

-- Manual lifecycle: open -> acknowledged -> recovered -> resolved (architecture 3).
-- `acknowledged` is an ACTIVE status: an engineer looking at an incident does not
-- stop it collecting evidence, so correlation keeps attaching to it.
-- `resolved` is terminal and never reopens; a later matching event starts a new
-- incident, which is what makes it visible as a recurrence.
ALTER TABLE incidents ADD COLUMN acknowledged_at TIMESTAMPTZ;
ALTER TABLE incidents ADD COLUMN resolved_at TIMESTAMPTZ;

ALTER TABLE incidents DROP CONSTRAINT incidents_status_check;
ALTER TABLE incidents ADD CONSTRAINT incidents_status_check
    CHECK (status IN ('open', 'acknowledged', 'recovered', 'resolved'));

-- recovered_at must be present exactly when the incident reached recovery, and
-- absent while it is still active. A resolved incident may or may not have
-- recovered first: an engineer can resolve something that never sent an UP.
ALTER TABLE incidents DROP CONSTRAINT incidents_check;
ALTER TABLE incidents ADD CONSTRAINT incidents_check CHECK (
    (status IN ('open', 'acknowledged') AND recovered_at IS NULL)
 OR (status = 'recovered' AND recovered_at IS NOT NULL)
 OR (status = 'resolved')
);
ALTER TABLE incidents ADD CONSTRAINT incidents_resolved_at_check CHECK (
    (status = 'resolved') = (resolved_at IS NOT NULL)
);
ALTER TABLE incidents ADD CONSTRAINT incidents_acknowledged_at_check CHECK (
    acknowledged_at IS NULL OR status <> 'open'
);

-- At most one ACTIVE incident per exact fingerprint. Widened from status='open'
-- so acknowledging an incident cannot let a second one open alongside it.
DROP INDEX incidents_open_fingerprint_key;
CREATE UNIQUE INDEX incidents_active_fingerprint_key ON incidents (organization_id, fingerprint)
    WHERE status IN ('open', 'acknowledged');

-- Operations View reads incidents by recency within a tenant.
CREATE INDEX incidents_org_last_event_idx
    ON incidents (organization_id, last_event_at DESC);
