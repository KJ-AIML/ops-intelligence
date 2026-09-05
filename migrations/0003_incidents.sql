-- Slice 3 keeps correlation work independently durable and terminal per Event.
ALTER TABLE events ADD COLUMN correlation_status TEXT NOT NULL DEFAULT 'received'
    CHECK (correlation_status IN ('received', 'processing', 'correlated', 'ignored'));
CREATE INDEX events_correlation_pending_idx ON events (organization_id, occurred_at, id)
    WHERE correlation_status = 'received';

CREATE TABLE incidents (
    id UUID PRIMARY KEY,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    environment TEXT,
    service TEXT,
    resource TEXT,
    event_family TEXT NOT NULL CHECK (event_family IN (
        'availability', 'latency', 'error_rate', 'saturation_cpu', 'saturation_memory',
        'saturation_disk', 'connectivity', 'certificate', 'backup', 'job', 'unclassified')),
    status TEXT NOT NULL CHECK (status IN ('open', 'recovered')),
    severity TEXT NOT NULL CHECK (severity IN ('debug', 'info', 'warning', 'critical')),
    started_at TIMESTAMPTZ NOT NULL,
    last_event_at TIMESTAMPTZ NOT NULL,
    recovered_at TIMESTAMPTZ,
    reopened_count INTEGER NOT NULL DEFAULT 0 CHECK (reopened_count >= 0),
    created_at TIMESTAMPTZ NOT NULL,
    CHECK ((status = 'open' AND recovered_at IS NULL) OR (status = 'recovered' AND recovered_at IS NOT NULL))
);
-- One current incident for an exact fingerprint. Past recovered records remain
-- history, enabling recurrence without rewriting previous evidence.
CREATE UNIQUE INDEX incidents_open_fingerprint_key ON incidents (organization_id, fingerprint)
    WHERE status = 'open';
CREATE INDEX incidents_lookup_idx ON incidents (organization_id, fingerprint, status, recovered_at DESC);

CREATE TABLE incident_events (
    incident_id UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    event_id UUID NOT NULL UNIQUE REFERENCES events(id) ON DELETE RESTRICT,
    relation TEXT NOT NULL CHECK (relation IN ('trigger', 'duplicate', 'recovery', 'update')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (incident_id, event_id)
);
CREATE INDEX incident_events_event_idx ON incident_events (event_id);
