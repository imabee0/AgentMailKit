-- Attachment bodies, as content-addressed blobs, keyed by the attachment id already on the wire.
--
-- The `attachments` JSONB column has carried attachment METADATA since 0005 -- filename, size,
-- content type, and an `attachment_id` this server mints. What it never carried is the bytes, so
-- `GET .../attachments/{attachment_id}` had nothing to serve.
--
-- WHY A SECOND COLUMN AND NOT A FIELD ON THE JSONB
--
-- `attachments` deserialises directly into `amk_types::message::Attachment`, which is a WIRE type
-- derived from the reference API. Adding a blob id to it would put an invented field on every
-- message response and the conformance diff would (correctly) flag it. Same reasoning, and the
-- same shape, as `raw_blob_id` in 0010: what is ours lives in its own column.
--
-- WHY A SEPARATE BLOB PER ATTACHMENT, RATHER THAN SLICING THE RAW ONE
--
-- The raw blob holds every attachment already, base64-encoded inside the MIME, so a download
-- could in principle re-parse it and cut out the part. That was rejected for two reasons. The
-- signed token names exactly one blob and its MAC covers that id (`amk_core::download`); serving
-- a slice would mean widening the token to carry an offset, which is a second thing an attacker
-- gets to tamper with for no gain. And re-parsing a multi-megabyte message on every attachment
-- fetch trades a cheap disk read for a parse, on the path most likely to be hit repeatedly.
--
-- Content addressing makes the duplication smaller than it looks: the same PDF sent to twenty
-- inboxes is one object, and the decoded body is about 3/4 the size of its base64 form.
--
-- SHAPE: {"<attachment_id>": "<sha256 hex>"}. A map rather than an array so a lookup by
-- attachment id is a JSONB `->>` rather than a scan, and so an attachment whose body could not be
-- stored is simply ABSENT -- distinguishable from one stored as an empty object, and the reason
-- the map may be sparse relative to `attachments`.
ALTER TABLE messages ADD COLUMN attachment_blobs JSONB;

-- No index, no foreign key, for exactly the reasons recorded in 0010: the lookup always runs from
-- (inbox_id, message_id, attachment_id) to a blob, never the other way, and blobs live on a
-- filesystem this database cannot reference.
COMMENT ON COLUMN messages.attachment_blobs IS
  'attachment_id -> sha256 hex of the decoded body in the blob store; NULL or sparse when bodies were not captured';
