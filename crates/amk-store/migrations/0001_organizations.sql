-- Organizations: the top-level tenant. amk_types::Organization has no create endpoint on the
-- wire (GET /v0/organizations only — see reference/openapi.json) and no billing fields are
-- stored (AgentMailKit ships no billing surface); inbox_count/domain_count are computed at read
-- time from the inboxes/domains tables rather than tracked as columns, so they cannot drift.
CREATE TABLE organizations (
    organization_id TEXT PRIMARY KEY,
    inbox_limit BIGINT,
    domain_limit BIGINT,
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now()
);
