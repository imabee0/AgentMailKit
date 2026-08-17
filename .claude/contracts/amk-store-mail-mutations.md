# Contract — amk-store: label update and delete for messages and threads

Scope-derivation: the gap is the difference between what `amk-http` needs to mount and what
`amk-store` offers, both enumerated by command.

```
$ grep -n "^pub async fn " crates/amk-store/src/messages.rs crates/amk-store/src/threads.rs
  messages.rs: insert, get, list          threads.rs: insert, get_with_messages, list

$ ./scripts/derive-implemented-paths.sh | sed -n '/reconciliation/,/^$/p'
  clean: all 29 mounted operations are described ...   # after the LIST slice

$ python3 -c "…openapi paths matching /messages|/threads…"    # the six still unmounted
  PATCH  /v0/inboxes/{inbox_id}/messages/{message_id}     DELETE  (same path)
  PATCH  /v0/inboxes/{inbox_id}/threads/{thread_id}       DELETE  (same path)
  PATCH  /v0/pods/{pod_id}/threads/{thread_id}            DELETE  (same path)
  PATCH  /v0/threads/{thread_id}                          DELETE  (same path)
```

`amk-http` cannot mount get-by-id for either resource until this lands: every get-by-id path in the
spec carries GET, PATCH **and** DELETE, so serving only the GET leaves two described operations
unserved on a mounted path — which `derive-implemented-paths.sh` reports and which would make the
P1 gate's path-derived schemathesis scope fuzz operations this server does not implement. **This
dispatch is the prerequisite, and the six mounts land in the one after it.**

## The evidence

`[SPEC:reference/fixtures/19-message-label-patch-gate.txt]` — **the authority, and it reverses a
reading two reviewers and this project's own orchestrator had agreed on.** The OpenAPI text says
"Cannot be system labels" on `UpdateThreadRequest` only, so messages were to be left ungated. Live:

```
PATCH …/messages/{mid} {"add_labels":["sent"]}       -> 400 "Cannot use system label: sent"
                              ["received"]           -> 400
                              ["bounced"]            -> 400
                              ["scheduled"]          -> 400
                              ["unread"|"spam"|"trash"|"blocked"|"unauthenticated"] -> 200
                              ["arbitrary-tag"]      -> 200
```

Three facts that are easy to get backwards, all in that fixture:

1. **The gate is exactly `{sent, received, bounced, scheduled}`** and applies to messages as well as
   threads. `amk-core::labels::is_system` / `system_label_violations` already model it.
2. **Restricted is not system.** A client MAY set `spam`/`trash`/`blocked`/`unauthenticated` on a
   message. Restricted governs who may SEE a label; system governs who may SET it. Different axes,
   neither implies the other.
3. **One bad label rejects the whole mutation**, not the valid part —
   `{"remove_labels":["spam","bounced"]}` failed 400 as a whole. So validation runs before any
   write, and the write is all-or-nothing.

`[SPEC:reference/fixtures/20-search-and-label-precedence.txt]` (C) — `remove` wins over `add` for a
label named in both, `[TESTED]` on a message. `amk-core::labels::apply_mutation` owns this; do not
re-implement the precedence in SQL.

`[SPEC:reference/openapi.json]` — `UpdateMessageResponse` / `UpdateThreadResponse` are
`{message_id|thread_id, labels}`, not the whole resource. Both already exist in `amk-types`.

## Writable paths

- `crates/amk-store/src/messages.rs` — `update` and `delete`.
- `crates/amk-store/src/threads.rs` — `update` and `delete`.
- `crates/amk-store/tests/` — the DB-backed tests for both.

Nothing else. In particular **not** `crates/amk-types/**` (frozen — `UpdateMessageRequest`,
`UpdateThreadRequest` and both response types already exist), **not** `crates/amk-core/**`
(`is_system`, `system_label_violations` and `apply_mutation` are the rules and are already written
and tested), **not** `crates/amk-http/**` (the mounts are the next dispatch), not `scripts/**`, not
the plan, not this contract.

## What to build

Four functions, each mirroring `inboxes::update`/`inboxes::delete`'s existing shape:

```rust
messages::update(pool, filter: &ScopeFilter, inbox_id, message_id, add, remove) -> Result<Option<Vec<String>>, StoreError>
messages::delete(pool, filter: &ScopeFilter, inbox_id, message_id)              -> Result<bool, StoreError>
threads::update (pool, filter: &ScopeFilter, thread_id, add, remove)            -> Result<Option<Vec<String>>, StoreError>
threads::delete (pool, filter: &ScopeFilter, thread_id)                         -> Result<bool, StoreError>
```

- **The system-label gate is NOT this crate's job.** It is a request-boundary rule
  (`system_label_violations`' own doc says so: the pipeline applies system labels directly through
  `apply_mutation`, which is why the gate lives at the boundary). `amk-store` applies what it is
  given. Do not add a second copy of the rule here — two representations of one rule is the
  `ApiKeyPermissions` collision the plan records.
- **Label precedence comes from `amk_core::labels::apply_mutation`**, called in Rust, not
  reimplemented as SQL array arithmetic. Read the row's labels, apply, write back — in **one
  statement** where possible, or in a transaction where not. A read-then-write across two
  round-trips without a transaction is a lost update under concurrent PATCH.
- **Scope is a predicate in the WHERE clause**, never a post-fetch check: a row outside the
  filter's window must be indistinguishable from an absent one, and must not be counted or
  cursor-exposed. Same rule as every list in this crate.
- **A NUL-bearing `inbox_id`/`message_id`, or a NUL-bearing `filter.inbox_id()` pin, masks as
  not-found** — `Ok(None)` / `Ok(false)`, never `StoreError::Database` (SQLSTATE 22021). All three
  bound values are checked independently; `messages::get`'s existing guard is the worked example
  and its comment explains why one guard is not enough.

## Assigned edge cases

DB-backed, each seeding its own rows:

1. Adding a label that is already present does not duplicate it, and does not reorder the existing
   list — `apply_mutation`'s documented behaviour, asserted through storage.
2. A label in both `add` and `remove` ends up **absent** (fixture 20 C).
3. An empty `add` and empty `remove` is a no-op that still returns the current labels — not an
   error, and not a rewrite of the row.
4. A row outside the scope filter's window returns `Ok(None)`/`Ok(false)`, and the row is
   **unchanged afterwards** — assert the second half, or the test only proves the return value.
5. A NUL byte in each of `inbox_id`, `message_id` and the filter's pin, one test each, all masking
   as not-found. Three separate assertions: a single guard passes one and fails the others.
6. `delete` returns `true` once and `false` on the second call for the same id.
7. Deleting a thread does **not** orphan its messages into a visible state — assert what the schema
   actually does (`0008_inbox_delete_cascades.sql` is the precedent to read first) and record the
   observed behaviour rather than asserting a rule no fixture states. If the answer is not
   determined by the schema, **STOP and report** — thread/message delete cascade is unobserved.
8. Concurrent PATCHes against the same row do not lose an update: two `add_labels` applied in
   sequence both survive. If the implementation cannot make that true, say so rather than deleting
   the test.

## Prohibitions

- No new type in `amk-types`, no new rule in `amk-core`. Both already have what this needs; if
  something seems missing, **STOP and report**.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` type in any signature; no
  Stalwart or JMAP concept, field or name.
- Do not implement the system-label gate here, and do not implement label precedence in SQL.
- Do not mount anything in `amk-http` — that is the next dispatch, and mounting a PATCH whose
  sibling DELETE is unwritten reintroduces exactly the half-served path this dispatch exists to
  clear.
- Do not resolve register **C2** (whether a thread's labels are a strict union of its members').
  A thread PATCH touching member labels is precisely where that question bites; the fail-closed
  choice is implemented and marked `[INFERRED]`. Leave it, and say in the report if this dispatch
  made the question sharper.
- If the contract is ambiguous or appears wrong, **STOP and report**. Do not resolve it yourself.

## Reporting

Report the command run and its actual output; "tests pass" without the output is not a report.

- `cargo test -p amk-store` and `./scripts/check.sh`, with a live database (the DB-backed tests
  skip silently without one — `AMK_REQUIRE_DB=1` turns that skip into a failure).
- A mutation pass in **both directions** on a scratch copy outside the tree: drop the scope
  predicate from the WHERE clause (must kill a test) and make `update` a no-op that returns the
  current labels unchanged (must also kill a test). Delete the scratch copy and confirm it.
