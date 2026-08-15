-- Threads. Labels are `text[]` with a GIN index rather than a join table — the schema decision in
-- the dispatch contract, forced by the admission rule: `NOT (labels && $excluded)` has to be one
-- index-backed predicate in the same WHERE clause as the keyset comparison, and a join table would
-- turn every list query into an anti-join whose row count is what leaks. Order is preserved by the
-- column (amk-core's apply_mutation is order- and duplicate-preserving) — nothing here sorts or
-- dedupes it.
--
-- The keyset index mirrors the message shape observed in fixture 04
-- (reference/fixtures/04-pagination.http: {message_id, inbox_id, timestamp}): for threads the
-- tiebreaker is the thread's own id, same structural shape, same scope-per-mount pinning.
CREATE TABLE threads (
    thread_id UUID PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organizations (organization_id),
    pod_id UUID NOT NULL REFERENCES pods (pod_id),
    inbox_id TEXT NOT NULL REFERENCES inboxes (inbox_id),
    labels TEXT[] NOT NULL DEFAULT '{}',
    "timestamp" TIMESTAMPTZ(3) NOT NULL,
    received_timestamp TIMESTAMPTZ(3),
    sent_timestamp TIMESTAMPTZ(3),
    senders TEXT[] NOT NULL DEFAULT '{}',
    recipients TEXT[] NOT NULL DEFAULT '{}',
    subject TEXT,
    preview TEXT,
    last_message_id TEXT NOT NULL,
    message_count BIGINT NOT NULL DEFAULT 0,
    size BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now()
);

CREATE INDEX threads_organization_id_idx ON threads (organization_id);
CREATE INDEX threads_pod_id_idx ON threads (pod_id);
CREATE INDEX threads_labels_gin_idx ON threads USING GIN (labels);
CREATE INDEX threads_keyset_idx ON threads (inbox_id, "timestamp", thread_id);
