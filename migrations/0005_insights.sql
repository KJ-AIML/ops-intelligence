-- 0005_insights — bounded AI interpretation, stored beside the facts it reads.
--
-- An Insight never replaces a source fact. It is an interpretation layered on
-- top of deterministic output, and it is always attributable: `source` says who
-- produced it and `model_metadata` says exactly which model, prompt version and
-- request did (architecture 2, tech sheet 11).

CREATE TABLE insights (
    id                UUID        PRIMARY KEY,
    organization_id   UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    incident_id       UUID        REFERENCES incidents (id) ON DELETE CASCADE,
    insight_type      TEXT        NOT NULL CHECK (insight_type IN (
                          'incident_explanation', 'actionability', 'suggested_investigation',
                          'recurring_pattern', 'noise_pattern', 'daily_brief')),
    -- 'deterministic' insights are computed; 'ai' insights are reasoned. Keeping
    -- them in one table lets the UI show both without caring which is which,
    -- while the column keeps the provenance honest.
    source            TEXT        NOT NULL CHECK (source IN ('deterministic', 'ai')),
    -- A failed reasoning attempt is recorded, not dropped: no silent loss
    -- applies to intelligence too, and a held result must stay visible.
    status            TEXT        NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'failed')),
    title             TEXT        NOT NULL,
    summary           TEXT,
    -- Only ever a schema-validated object. Raw model text is never stored here
    -- as a success (tech sheet 11).
    structured_payload JSONB,
    schema_version    INTEGER     NOT NULL DEFAULT 1,
    model_metadata    JSONB       NOT NULL DEFAULT '{}'::jsonb,
    error             TEXT,
    created_at        TIMESTAMPTZ NOT NULL,

    CHECK ((status = 'ok') = (structured_payload IS NOT NULL)),
    CHECK ((status = 'failed') = (error IS NOT NULL))
);

-- One successful insight of a given type per incident. Re-running the reasoner
-- is therefore idempotent and cannot quietly double the bill; failed attempts
-- are excluded so they stay retryable.
CREATE UNIQUE INDEX insights_incident_type_key
    ON insights (incident_id, insight_type)
    WHERE incident_id IS NOT NULL AND status = 'ok';

CREATE INDEX insights_org_created_idx ON insights (organization_id, created_at DESC);
CREATE INDEX insights_incident_idx ON insights (incident_id);
