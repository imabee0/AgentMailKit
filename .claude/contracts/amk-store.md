# amk-store — dispatch contract

Scope-derivation: n/a — greenfield crate, no existing surface to enumerate. Dispatch complete and
merged; retained as the record of what was agreed.

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

**Do not start until `amk-core` is merged and `./scripts/check.sh` is green on `main`.**

## What this crate is

Postgres persistence (sqlx), migrations, the blob store, full-text search, and signed download
URLs. It is the only crate that talks to the database. It depends on `amk-types` and `amk-core`,
and on nothing else in the workspace. Nothing depends on it except `amk-http` and, at P6,
`amk-import`.

## Writable paths (exact)

`crates/amk-store/**` and nothing else. `Cargo.lock` may change as a side effect of `cargo` doing
its job; that is not a licence to edit another crate's `Cargo.toml`. A hook enforces this at write
time, keyed on the writer and on the target, so neither a stray absolute path nor a shell sitting
in the wrong directory gets through.

If the work genuinely requires a path outside that tree, **STOP and report**. Do not widen scope on
your own judgement — one authorised exception this phase (`scripts/check.sh`) was granted by the
orchestrator in writing, and that is the only shape an exception takes.

## `[SPEC:*]` citations governing every shape here

Every storage model derives from these. Where a fixture and the spec text disagree, **the fixture
wins and you report the contradiction** — that has already happened once on this project, when the
OpenAPI descriptions said the system-label gate applied only to threads and a live capture proved
it applies to messages too.

- `[TESTED]` `reference/fixtures/04-pagination.http` — the page token is `base64(JSON)` of the
  keyset `{message_id, inbox_id, timestamp}`; **absent** on the last page, never `""`.
- `[TESTED]` `reference/fixtures/18-inbox-case-normalization.txt` — `inbox_id` folds ASCII case:
  `{"username":"AmkCase"}` stores `amkcase@…` and any casing resolves it. Also the source for
  `limit_exceeded`'s `resource`/`limit` extras and the fact that the quota is organization-wide.
- `[TESTED]` `reference/fixtures/09b-unauthenticated-variant.txt` — restricted-label rows are
  excluded from list endpoints **with no gap in the page sequence**; the live API leaks neither a
  count nor a cursor. This is why admission is a `WHERE` predicate, not a post-filter.
- `[TESTED]` `reference/fixtures/20-search-and-label-precedence.txt` — three access modes:
  list-with-include-flags, search (permission only, restricted mail IS returned), get-by-id.
- `[TESTED]` `reference/fixtures/03-id-formats.http` — `message_id` is an RFC 5322 angle-bracket
  value, stored exactly as received; `thread_id` is a UUID.
- `[TESTED]` `reference/fixtures/06-download-url-expiry.txt` — ~1h TTL, **403** once expired.
  (Signed downloads are a later slice; cited so the shape is not re-derived when it arrives.)
- `[SPEC:openapi]` / `[SPEC:sdk]` — reached **only** through `amk-types`. This crate never
  re-derives a wire shape; if a needed type is absent there, STOP and report.

## The two rules that come from the P0 review, not from taste

### 1. Restricted-label admission is a QUERY predicate, never a post-filter

The review panel proved that filtering an already-fetched page leaks the thing it hides. With
`?limit=1`, every page whose single row was hidden returns `count: 0` **with** a
`next_page_token`; walking the cursor counts the hidden rows exactly, and the tokens disclose
their ids and timestamps. `reference/fixtures/09b-unauthenticated-variant.txt` shows the live API
leaks neither — `count=3`, no gaps — because the exclusion happens inside the query.

So: `amk-core` owns the *rule* and hands this crate a predicate; this crate pushes it into the
`WHERE` clause. A row the credential may not see is never fetched, so it cannot be counted, cannot
consume a page slot, and cannot appear in a cursor. `amk-core::labels::retain_visible` exists for
non-paginated collections (thread membership) only — if you find yourself calling it on a page,
the query was wrong.

### 2. Scope coordinates are pinned in the query too

Every query applies all pinned coordinates from `ScopeFilter` (`organization_id` always;
`pod_id`/`inbox_id` when pinned). Cross-tenant rows must not enter a result set, for the same
reason: what cannot be fetched cannot leak through a count, a total, or a cursor. Do not fetch and
then check.

## Decisions (settled — implement, do not relitigate)

- **Postgres via sqlx**, compile-time-checked queries where practical. Bind parameters only; never
  format a value into SQL, including for `ORDER BY` direction — map it to a fixed set of clauses.
- **Keyset pagination, not OFFSET.** The cursor is `base64(JSON)` of the last row's sort key. For
  messages the observed keyset is `{message_id, inbox_id, timestamp}`
  (`reference/fixtures/04-pagination.http`). Use `amk_types::page::Cursor`. The token is **absent**
  on the last page — never an empty string. SDKs treat it as opaque, so our bytes need not match
  theirs, but the scheme does.
- **`inbox_id` is stored and compared lowercased** (`reference/fixtures/18-inbox-case-normalization.txt`).
  Store the normalized form; a unique index on it is what makes collisions collide. Never index or
  join on the raw-cased value.
- **Blobs behind a `BlobStore` trait**, content-addressed on a filesystem tree, S3-capable later.
  Raw MIME and attachments are immutable once written, which is what makes incremental backup
  cheap. The trait must not leak `std::path` into its interface.
- **Signed download URLs are ours, HMAC, ~1h TTL.** `reference/fixtures/06-download-url-expiry.txt`
  measured the reference at ~1h, returning **403** once expired. An expired or tampered token
  returns 403 — not 404, not a redirect. Do not presign to a third party.
- **Jobs table + `SELECT … FOR UPDATE SKIP LOCKED`.** One durable mechanism, no Redis. A worker
  crashing mid-send must not double-send on restart; test that, do not assume it.
- **Idempotency lives here**, keyed by `(organization_id, key)`, storing the request-body hash and
  the original response. Same key + same body → the stored response; same key + different body →
  409; TTL 24h after the send completes.
- **Search: Postgres FTS + `pg_trgm`** for the substring filters, `ts_headline` for highlights. No
  additional service.

## Schema decisions (settled by the orchestrator; the rest you derive from `amk-types`)

Table and column names mirror `amk-types` — that crate already derives from AgentMail's artifacts,
so re-deriving them here is duplication, not diligence. What follows is only what a reasonable
implementer could get wrong in a way that matters.

- **Labels are a `text[]` column with a GIN index**, not a join table. This is forced by the
  admission rule: the exclusion has to be expressible as a predicate in the same `WHERE` clause as
  the keyset comparison, and `NOT (labels && $excluded)` is one index-backed test. A join table
  turns every list query into an anti-join whose row count is what leaks. Label order is preserved
  (`apply_mutation` in amk-core is order-preserving and duplicate-preserving; the column must not
  silently sort or dedupe).
- **`inbox_id` is stored lowercased** with the unique index on that stored form. Do not add a
  functional index on `lower(inbox_id)` over a mixed-case column: the normalized value is the
  identity, and keeping a second casing around invites a query that compares the wrong one.
- **The keyset index on messages is `(inbox_id, timestamp, message_id)`**, matching the cursor
  observed in fixture 04. Same shape per scope for the pod- and org-level mounts. The cursor's
  tiebreaker is `message_id`, so the index must include it or pagination is non-deterministic on
  equal timestamps — which the live API's millisecond precision makes likely, not theoretical.
- **`message_id` is stored with its angle brackets**, exactly as received. It is an RFC 5322
  header value, not an identifier we mint; stripping and re-adding brackets is a normalization
  nobody asked for and it breaks byte-equality with the wire.
- **Timestamps are `timestamptz` stored at millisecond precision.** `amk_types::Timestamp`
  truncates to milliseconds so the in-memory value always equals what will be serialized; a column
  with microsecond precision reintroduces the drift that type exists to prevent.
- **Blobs are content-addressed by SHA-256 of the raw bytes.** The database stores the digest, size
  and content type; bytes live in the blob tree. Deduplication is a consequence, not a goal — the
  reason is that immutable objects make the backup incremental and the restore drill cheap.
- **Idempotency records store the request-body hash, not the body.** Enough to detect a mismatch,
  nothing more retained than needed.
- **Every table carrying tenant data has `organization_id` NOT NULL**, and every query pins it.
  A nullable tenant column is a row that matches nothing and therefore leaks nothing — until
  someone writes `WHERE organization_id IS NOT DISTINCT FROM $1`.

## Migrations

Plain SQL files, forward-only, checked in, applied by `amk migrate`. Every migration is runnable on
an empty database and on the previous release's schema. No migration framework beyond sqlx's.

## Assigned edge cases (write the test before the code it targets)

From the plan's Testing section — these are the ones that land in this crate:

- A page token that is tampered, truncated, invalid base64, from a **different scope**, or from a
  deleted resource. A token replayed after the underlying rows changed.
- Two simultaneous creates of the same inbox username → exactly one wins, the other gets the
  collision error. (Concurrency, not a check-then-insert race.)
- Concurrent label mutations on one message.
- Job worker crash mid-send → no double-send on restart. Test SKIP LOCKED semantics; do not assume.
- Restricted-label rows absent from a paginated walk **with no gap in the page sequence** — the
  regression for rule 1 above.
- A `message_id` containing `<`, `>`, `@`, `+`, `%`, `/`, and non-ASCII, round-tripping through
  storage and retrieval.
- Case-variant `inbox_id` resolving to one row, and two case-variant usernames colliding.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export. Those crates belong to `amk-ingest`/`amk-outbound`.
- No JMAP, Sieve, RocksDB, or mailbox-role concept — not even as an optional or legacy column.
  Storage models derive from AgentMail's artifacts. (A hook blocks this at write time.)
- No dependency on `amk-import`. That direction is the translation boundary and must not invert.
- Do not edit `amk-types`, `amk-core`, the plan, or another crate's files. If you need a type that
  does not exist, **STOP and report** — do not add a field that obviously belongs.

## Reporting

Report the command you ran and its actual output. `cargo test -p amk-store` and
`./scripts/check.sh` — "tests pass" without the output is not a report. Name anything you did not
do and why.
