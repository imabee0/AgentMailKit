-- Messages. `message_id` is stored WITH its angle brackets, byte-exact as received — it is an
-- RFC 5322 header value, not an identifier we mint (dispatch contract schema decision). Primary
-- key is (inbox_id, message_id): the keyset cursor observed in fixture 04
-- (reference/fixtures/04-pagination.http) is {message_id, inbox_id, timestamp}, i.e. a
-- Message-ID is only guaranteed unique within one inbox, not globally.
--
-- `from_address` and `message_references` are renamed off their wire names (`from`, `references`)
-- because both are reserved words in SQL; the wire shape itself lives in amk_types::MessageItem
-- and is unaffected by the storage column name.
--
-- `attachments` and `headers` are stored as JSONB reusing amk_types::message::Attachment's own
-- Serialize/Deserialize impl (via sqlx::types::Json<Vec<Attachment>>) rather than a second,
-- hand-rolled shape — the blob bytes themselves are out of this dispatch's scope (BlobStore
-- trait, P1), so only the metadata amk_types::Attachment already carries is persisted here.
CREATE TABLE messages (
    inbox_id TEXT NOT NULL REFERENCES inboxes (inbox_id),
    message_id TEXT NOT NULL,
    organization_id TEXT NOT NULL REFERENCES organizations (organization_id),
    pod_id UUID NOT NULL REFERENCES pods (pod_id),
    thread_id UUID NOT NULL REFERENCES threads (thread_id),
    labels TEXT[] NOT NULL DEFAULT '{}',
    "timestamp" TIMESTAMPTZ(3) NOT NULL,
    from_address TEXT NOT NULL,
    to_addresses TEXT[] NOT NULL DEFAULT '{}',
    cc_addresses TEXT[],
    bcc_addresses TEXT[],
    subject TEXT,
    preview TEXT,
    attachments JSONB,
    in_reply_to TEXT,
    message_references TEXT[],
    headers JSONB,
    smtp_id TEXT,
    size BIGINT NOT NULL,
    reply_to TEXT[],
    text TEXT,
    html TEXT,
    extracted_text TEXT,
    extracted_html TEXT,
    created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
    PRIMARY KEY (inbox_id, message_id)
);

CREATE INDEX messages_organization_id_idx ON messages (organization_id);
CREATE INDEX messages_pod_id_idx ON messages (pod_id);
CREATE INDEX messages_thread_id_idx ON messages (thread_id);
CREATE INDEX messages_labels_gin_idx ON messages USING GIN (labels);
CREATE INDEX messages_keyset_idx ON messages (inbox_id, "timestamp", message_id);
