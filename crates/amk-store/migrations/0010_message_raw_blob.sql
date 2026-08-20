-- Raw MIME, stored as a content-addressed blob rather than in the row.
--
-- The message table already carries the PARSED view -- text, html, headers, attachments metadata
-- -- but the original bytes were dropped on the floor after parsing. That made
-- `GET /messages/{id}/raw` unimplementable, and it also meant a message could never be re-parsed
-- if the parser improved or a bug was found: the evidence was gone.
--
-- The blob id, not the bytes. A multi-megabyte BYTEA in a row that every list query touches makes
-- every unrelated read pay for it, and Postgres would TOAST it out of line anyway -- so this
-- stores the pointer and lets the filesystem hold the object, which is also what makes the
-- backup story (`docs/PLAN.md`:200) an incremental rsync of immutable files.
--
-- NULLABLE, deliberately. Every message inserted before this migration has no blob, and there is
-- no honest value to invent for them; `raw` on such a message is a 404, not a lie. It is also
-- nullable for messages whose raw was never available -- a draft that was composed rather than
-- received.
ALTER TABLE messages ADD COLUMN raw_blob_id TEXT;

-- No index. Nothing looks a message up BY its blob id -- the lookup always goes the other way,
-- from (inbox_id, message_id) to the blob -- and an unused index is write cost with no reader.
--
-- No foreign key either: blobs live on a filesystem, not in this database. That the two can drift
-- is the accepted cost of content-addressed storage, and it fails in the safe direction -- a row
-- pointing at a missing blob is a 404 on one endpoint, whereas a blob with no row is inert bytes
-- a sweep can reclaim.
COMMENT ON COLUMN messages.raw_blob_id IS
  'sha256 hex of the original RFC 5322 bytes in the blob store; NULL when no raw was captured';
