-- 0006_pilot_lab — capture real signals, freeze them into datasets, replay them.
--
-- The synthetic fixture proves the engine does what we designed. This proves it
-- does something useful to real traffic, which is a different question.

-- A frozen, named set of captured RawSignals.
--
-- Membership is by reference, never by copy: the dataset points at the original
-- evidence rather than duplicating it, so a dataset can never drift from what
-- was actually received.
CREATE TABLE pilot_datasets (
    id              UUID        PRIMARY KEY,
    organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    name            TEXT        NOT NULL,
    description     TEXT,
    -- Once frozen a dataset is immutable. A regression baseline that can be
    -- edited is not a baseline.
    frozen_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL,

    UNIQUE (organization_id, name)
);

CREATE TABLE pilot_dataset_signals (
    dataset_id    UUID   NOT NULL REFERENCES pilot_datasets (id) ON DELETE CASCADE,
    raw_signal_id UUID   NOT NULL REFERENCES raw_signals (id) ON DELETE RESTRICT,
    -- Capture order, so a replay reproduces arrival sequence as well as content.
    ordinal       BIGINT NOT NULL,

    PRIMARY KEY (dataset_id, raw_signal_id)
);

CREATE INDEX pilot_dataset_signals_order_idx ON pilot_dataset_signals (dataset_id, ordinal);

-- One replay of one dataset.
--
-- Each run targets its OWN organization. Ingestion is idempotent on
-- (source_id, external_id), so replaying into the capture tenant would insert
-- nothing on the second attempt. Giving every run a fresh tenant makes replay
-- repeatable and reuses the tenant isolation the engine already enforces
-- everywhere, instead of inventing a second isolation mechanism.
CREATE TABLE pilot_runs (
    id                     UUID        PRIMARY KEY,
    dataset_id             UUID        NOT NULL REFERENCES pilot_datasets (id) ON DELETE CASCADE,
    -- The throwaway tenant this run's signals, events and incidents live in.
    target_organization_id UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    label                  TEXT        NOT NULL,
    -- What produced this result, so two runs are comparable on purpose rather
    -- than by accident.
    engine_version         TEXT        NOT NULL,
    ai_enabled             BOOLEAN     NOT NULL DEFAULT FALSE,
    ai_provider            TEXT,
    ai_model               TEXT,
    status                 TEXT        NOT NULL DEFAULT 'running'
                               CHECK (status IN ('running', 'completed', 'failed')),
    -- Derived counts for the run: signals, events, incidents, relations.
    stats                  JSONB       NOT NULL DEFAULT '{}'::jsonb,
    error                  TEXT,
    started_at             TIMESTAMPTZ NOT NULL,
    finished_at            TIMESTAMPTZ
);

-- Labels address a run in `pilot compare <a> <b>`, so they must be unique.
CREATE UNIQUE INDEX pilot_runs_label_key ON pilot_runs (label);
CREATE INDEX pilot_runs_dataset_idx ON pilot_runs (dataset_id, started_at DESC);

-- Mark replay tenants so they are never mistaken for the real one in a query,
-- a dashboard, or a bill.
ALTER TABLE organizations ADD COLUMN is_replay BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX organizations_replay_idx ON organizations (is_replay) WHERE is_replay;
