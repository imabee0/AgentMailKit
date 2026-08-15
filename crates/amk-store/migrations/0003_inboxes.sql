-- Inboxes. `inbox_id` IS the email address (amk_types::ids::InboxId) and is stored in its
-- normalized (ASCII-lowercased) form per reference/fixtures/18-inbox-case-normalization.txt: the
-- live API lowercases the username at creation and resolves lookups case-insensitively. The
-- primary key is therefore the normalized value, and it is the ONLY casing kept — no functional
-- index on lower(inbox_id) over a mixed-case column, per the schema decision in the dispatch
-- contract: a second casing around invites a query that compares the wrong one.
--
-- That primary key is also what makes "two simultaneous creates of the same username" resolve to
-- exactly one winner: both INSERTs race on this constraint at the database level, not on an
-- application-level check-then-insert.
CREATE TABLE inboxes (
    inbox_id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organizations (organization_id),
    pod_id UUID NOT NULL REFERENCES pods (pod_id),
    client_id TEXT,
    display_name TEXT,
    metadata JSONB,
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now()
);

CREATE INDEX inboxes_organization_id_idx ON inboxes (organization_id);
CREATE INDEX inboxes_pod_id_idx ON inboxes (pod_id);

-- Idempotent creation, same shape as pods: replaying (organization_id, client_id) returns the
-- original row rather than raising the username collision.
CREATE UNIQUE INDEX inboxes_org_client_id_idx ON inboxes (organization_id, client_id)
    WHERE client_id IS NOT NULL;
