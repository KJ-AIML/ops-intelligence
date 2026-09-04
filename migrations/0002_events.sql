-- Enforce tenant/source evidence linkage at the database boundary too.
ALTER TABLE sources ADD CONSTRAINT sources_org_id_key UNIQUE (organization_id, id);
ALTER TABLE raw_signals ADD CONSTRAINT raw_signals_source_tenant_fk
    FOREIGN KEY (organization_id, source_id) REFERENCES sources (organization_id, id);
ALTER TABLE raw_signals ADD CONSTRAINT raw_signals_evidence_key
    UNIQUE (organization_id, source_id, id);

CREATE TABLE events (
    id UUID PRIMARY KEY,
    organization_id UUID NOT NULL,
    source_id UUID NOT NULL,
    raw_signal_id UUID NOT NULL UNIQUE,
    occurred_at TIMESTAMPTZ NOT NULL,
    environment TEXT,
    service TEXT,
    resource TEXT,
    event_family TEXT NOT NULL CHECK (event_family IN (
        'availability', 'latency', 'error_rate', 'saturation_cpu', 'saturation_memory',
        'saturation_disk', 'connectivity', 'certificate', 'backup', 'job', 'unclassified')),
    severity TEXT NOT NULL CHECK (severity IN ('debug', 'info', 'warning', 'critical')),
    state TEXT NOT NULL CHECK (state IN ('firing', 'resolved', 'informational')),
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    message TEXT,
    labels JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(labels) = 'object'),
    external_id TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (organization_id, source_id, raw_signal_id)
        REFERENCES raw_signals (organization_id, source_id, id)
);
CREATE INDEX events_org_occurred_idx ON events (organization_id, occurred_at DESC);
CREATE INDEX events_org_source_occurred_idx ON events (organization_id, source_id, occurred_at DESC);
