-- API keys. `prefix` is the leading, non-secret segment of the minted key and the O(1) lookup
-- path (unique index); `hash` is an argon2id hash of the full presented secret and never the
-- secret itself — see api_keys.rs's module doc for the minting format.
--
-- Scope is derived from which of pod_id/inbox_id is set, not stored as a separate enum column
-- (dispatch contract): both null is an organization-scoped key, pod_id alone is pod-scoped,
-- inbox_id alone is inbox-scoped. The two are never set together — a row naming both has no
-- representation in amk-core::scope::Scope (Organization/Pod/Inbox are the only three shapes, and
-- an inbox-scoped credential's pod is looked up through its inbox at the Identity-building layer,
-- not stored redundantly here) — so the CHECK below rejects that combination at the database,
-- never merely in application code.
--
-- `permissions` is nullable and the nullability is load-bearing: SQL NULL is the absent
-- permissions object (grants everything, per amk_types::api_key::KeyGrants::from_wire) and
-- '{}'::jsonb is the present-but-empty one (grants nothing). Collapsing the two is a privilege
-- bug in both directions.
--
-- No ON DELETE clause on any FK, matching every other table in this crate: deleting a pod or
-- inbox that still owns keys is rejected outright (the default NO ACTION) rather than silently
-- orphaning or cascading through them.
CREATE TABLE api_keys (
    api_key_id UUID PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organizations (organization_id),
    pod_id UUID REFERENCES pods (pod_id),
    inbox_id TEXT REFERENCES inboxes (inbox_id),
    name TEXT NOT NULL,
    prefix TEXT NOT NULL,
    hash TEXT NOT NULL,
    permissions JSONB,
    used_at TIMESTAMPTZ(3),
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    CONSTRAINT api_keys_scope_not_both_pod_and_inbox
        CHECK (NOT (pod_id IS NOT NULL AND inbox_id IS NOT NULL))
);

-- The O(1) lookup path amk-http's auth layer needs.
CREATE UNIQUE INDEX api_keys_prefix_idx ON api_keys (prefix);

CREATE INDEX api_keys_organization_id_idx ON api_keys (organization_id);
CREATE INDEX api_keys_pod_id_idx ON api_keys (pod_id);
CREATE INDEX api_keys_inbox_id_idx ON api_keys (inbox_id);
