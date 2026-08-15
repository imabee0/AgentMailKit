# amk-store — dispatch contract

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

**Do not start until `amk-core` is merged and `./scripts/check.sh` is green on `main`.**

## What this crate is

Postgres persistence (sqlx), migrations, the blob store, full-text search, and signed download
URLs. It is the only crate that talks to the database. It depends on `amk-types` and `amk-core`,
and on nothing else in the workspace. Nothing depends on it except `amk-http` and, at P6,
`amk-import`.

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
