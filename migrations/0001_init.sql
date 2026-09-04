-- 0001_init — organizations, sources, raw_signals
--
-- Scope note: this migration creates only the tables Slice 1 actually writes to.
-- events / incidents / incident_events / insights arrive in later migrations,
-- each alongside the code that uses it. Tech sheet 27 permits combining the
-- suggested per-table migrations.
--
-- gen_random_uuid() is built into PostgreSQL 13+, so pgcrypto is not required.
-- IDs are generated in application code; the DEFAULT is a safety net only.

CREATE TABLE organizations (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL,
    slug        TEXT        NOT NULL UNIQUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sources (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    source_type     TEXT        NOT NULL CHECK (source_type IN (
                        'csv_import', 'generic_webhook', 'grafana', 'azure_monitor', 'email')),
    name            TEXT        NOT NULL,
    enabled         BOOLEAN     NOT NULL DEFAULT TRUE,
    config          JSONB       NOT NULL DEFAULT '{}'::jsonb,
    last_seen_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (organization_id, name)
);

CREATE INDEX sources_organization_idx ON sources (organization_id);

CREATE TABLE raw_signals (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id   UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    source_id         UUID        NOT NULL REFERENCES sources (id) ON DELETE CASCADE,
    received_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    external_id       TEXT,
    content_type      TEXT        NOT NULL,
    payload           JSONB       NOT NULL,
    payload_hash      TEXT        NOT NULL,
    processing_status TEXT        NOT NULL DEFAULT 'received' CHECK (processing_status IN (
                          'received', 'queued', 'processing', 'processed', 'failed', 'ignored')),
    processing_error  TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotency, in the priority order given by tech sheet 9:
--   1. a stable source-side event id, when the source provides one
--   2. otherwise the payload hash
-- Two partial unique indexes rather than one combined constraint, so that a
-- source WITH external ids can still legitimately re-fire an identical payload
-- later (different external_id => different signal), while a source WITHOUT
-- them falls back to content identity.
CREATE UNIQUE INDEX raw_signals_source_external_id_key
    ON raw_signals (source_id, external_id)
    WHERE external_id IS NOT NULL;

CREATE UNIQUE INDEX raw_signals_source_payload_hash_key
    ON raw_signals (source_id, payload_hash)
    WHERE external_id IS NULL;

CREATE INDEX raw_signals_org_received_at_idx
    ON raw_signals (organization_id, received_at DESC);

-- No-silent-loss accounting: every signal must reach a terminal status, so the
-- worker needs a cheap scan of the ones that have not.
CREATE INDEX raw_signals_pending_idx
    ON raw_signals (processing_status, received_at)
    WHERE processing_status IN ('received', 'queued', 'processing');
