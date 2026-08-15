-- Pods. `client_id` is the idempotency key for creation (amk_types::pod::CreatePodRequest):
-- replaying the same (organization_id, client_id) must return the original row, never a
-- duplicate, so the uniqueness that guarantees it lives in the schema, not in application code.
CREATE TABLE pods (
    pod_id UUID PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organizations (organization_id),
    client_id TEXT,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now()
);

CREATE INDEX pods_organization_id_idx ON pods (organization_id);

-- Partial: client_id is optional (CreatePodRequest), and only supplied client_ids need to
-- collide with one another.
CREATE UNIQUE INDEX pods_org_client_id_idx ON pods (organization_id, client_id)
    WHERE client_id IS NOT NULL;
