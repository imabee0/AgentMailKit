# amk-store — hostile id bytes reaching SQL — dispatch contract

Scope-derivation: `grep -rn '\.bind(' crates/amk-store/src` — 120 bind sites, every one
classified; no dynamic SQL, so `.bind()` is provably the complete set of parameter paths.

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

**This must land before `amk-http`.** Every wire path below is unreachable today only because there
is no HTTP surface. `amk-http` is what makes them reachable, all at once.

## What this dispatch is

A caller-supplied identifier containing a byte Postgres cannot encode as `text` — a NUL, `0x00` —
reaches a bound query parameter and fails at *encoding*, before any comparison. The result is
`StoreError::Database` (SQLSTATE `22021`) surfacing as a 500-class error instead of the uniform
"not found" every other unresolvable id produces.

Two things make that a defect rather than a curiosity:

- **It is denial-distinguishing.** The contract requires scope and label denial to mask as
  `not_found` so a caller cannot learn that a resource exists. An error that fires only for
  *malformed* ids hands the caller a side channel the uniform path denies them.
- **It is wire-reachable.** `from_path_segment` rejected only invalid UTF-8, and `%00`
  percent-decodes to a perfectly valid UTF-8 string containing a NUL.

## Two doors, and only one of them is closed

**Fixing `from_path_segment` was not sufficient, and believing otherwise is the trap.** The review
panel checked the other constructors and found `MessageCursor::decode` / `ThreadCursor::decode`
(`crates/amk-store/src/pagination.rs`) build `InboxId`/`MessageId` through the raw `::new()`
constructor from base64(JSON) page-token fields — never through `from_path_segment` at all. A
tampered token carrying `"inbox_id":"abc\x00def"` was reproduced live reaching the keyset query and
erroring `22021` on parameter `$6`.

So an id newtype has **two independent wire-reachable entry points**, and a fix at one leaves the
other open while looking complete. The path-segment door is shut; the page-token door is yours.

**And a third door opens in P2.** `amk-ingest` will call `messages::insert` with a `MessageId`
parsed out of hostile MIME, and `amk-import` will call the same functions with values read from
Stalwart. Neither goes through a path segment or a page token. That is why this dispatch closes the
door *and* makes the store functions themselves total — see the layering decision below.

## Writable paths (exact)

`crates/amk-store/**`, plus the workspace `Cargo.lock` only if a sanctioned dependency is added.
Nothing else. `amk-types` is **frozen**, as always. If the work requires a path outside that tree,
**STOP and report** rather than widening scope.

**The `amk-types` half is already done and is not yours.** An earlier draft of this contract listed
`crates/amk-types/src/ids.rs` as writable, which was wrong twice over: the plan reserves `amk-types`
to the orchestrator and never fans it out, and the guard's rule 2 blocks any worktree write to that
tree regardless of the lock — so the dispatch would have been blocked at its first edit. The
orchestrator made that change directly instead (commit `59d5b20`). What exists on `main` for you:

- `amk_types::ids::has_forbidden_byte(&str) -> bool` — public, the single definition of the rule.
- `IdDecodeError::Nul` — a variant distinct from `Utf8`, because the two have different causes.
- `from_path_segment` on every `string_id!` type now rejects a NUL rather than passing it through.

**Use `has_forbidden_byte`. Do not write a second copy of the rule** — two modules independently
defining the same predicate is precisely the fan-out collision that cost this project a review round
(`labels.rs` vs `permissions.rs`).

## `[SPEC:*]` citations

- `[TESTED]` `reference/fixtures/04-pagination.http` — page tokens are `base64(JSON keyset cursor)`
  over `{message_id, inbox_id, timestamp}`. That is the door you are closing.
- `[TESTED]` `reference/fixtures/03-id-formats.http` — `message_id` is an RFC 5322 angle-bracket
  value carrying `<`, `>`, `@`. Whatever you reject must not reject a legitimate `message_id`.
- `[TESTED]` `reference/fixtures/18-inbox-case-normalization.txt` — `inbox_id` folds ASCII case and
  is compared via `InboxId::eq_normalized`. Your check runs *before* normalisation and must not
  disturb it.
- `[SPEC:openapi]` — ids appear in path segments across the 82 paths; none of the schemas constrain
  their byte content, so the rejection rule is **`[ASSUMED]`** and must be tagged as such.

## Decisions (settled — implement, do not relitigate)

### What is rejected

A NUL byte, unconditionally, via `has_forbidden_byte`. Reject the whole value — do not strip, trim
or sanitise it. Silently rewriting a caller's id is how two ids become equal that should not be.

### Two layers, because they serve different callers

- **The page-token door** — `MessageCursor::decode` and `ThreadCursor::decode` reject a NUL in
  `message_id` or `inbox_id` with a **new** `PageTokenError::ForbiddenByte(&'static str)` naming the
  field. Distinct from `WrongType` on purpose: the type is right and the bytes are not, and a caller
  that collapses them cannot tell a client "your token is corrupt" from "your token carries a byte
  we refuse". `thread_id` already parses as a UUID and is total — leave it.
- **The store functions themselves** — `amk-store` is a library whose callers are not only
  `amk-http`. A public function that 500s on a byte its parameter type permits is a defect in that
  function, not in its caller. Make them total, per the table below.

Both layers are required and they are not redundant: the first gives a precise error to a wire
client, the second gives a uniform result to every caller that will ever exist.

### How the store functions become total

| Kind | Functions | Behaviour on a NUL-bearing id |
|---|---|---|
| lookup / delete | `inboxes::{get,delete}`, `messages::{get,list}`, `threads::{get_with_messages,list}` | the function's own existing not-found result — `Ok(None)`, `Ok(false)`, an empty `Page`. **Never `Err`.** |
| insert / create | `messages::insert`, `threads::insert`, `inboxes::create`, `pods::create` | a **new** `StoreError::InvalidValue(&'static str)` naming the field |

**Use an early return, not a `None` bind.** The api-keys dispatch made `api_key_id` total by binding
`Option<Uuid>` and letting `($3::uuid IS NULL OR …)` absorb it, and that idiom is *wrong here*:
`filter.inbox_id()` is bound as a **pin**, where NULL means "no pin" and would widen the query
across every inbox in the org. A dropped pin is a cross-tenant read; a missed early return is a 500.
Choose the option whose failure mode is smaller, and do not mix the two idioms in one function.

### `client_id` — decided, and it is a rejection

`pods::create` and `inboxes::create` bind a caller-supplied `client_id` into an `INSERT`, so a NUL
fails at the bind rather than at a lookup. It gets `StoreError::InvalidValue`, **not** the not-found
treatment and **not** a silent NULL, because `ON CONFLICT (organization_id, client_id) WHERE
client_id IS NOT NULL` is the idempotency key: nulling it stops the conflict target firing and
turns an idempotent create into a duplicating one. Rejecting is the only option that preserves the
semantics the `client_id` contract promises.

### `organization_id` gets no guard, deliberately

It is bound in nearly every function and is never caller-supplied — it comes from the resolved
credential, i.e. a value this crate itself stored. Guarding it would imply a threat that does not
exist and add a test that can never fail. Recorded here so its absence reads as a decision rather
than an oversight; if you disagree, **STOP and report** rather than adding it.

### Do not make `::new()` fallible

It is used throughout the codebase on values that are already ours — a row read back from our own
database, a freshly minted id — and making it `Result` would ripple into every call site for no
safety gain. The untrusted paths are the doors named above.

### `api_key_id` keeps its `Option<Uuid>` binding

Already total, already reviewed across five rounds. Do not rewrite it to use the new check.

## Assigned edge cases (write the test before the code it targets)

- A page token whose decoded JSON carries a NUL in `inbox_id`, and one in `message_id` → assert the
  error **type** is `ForbiddenByte` with the right field, not merely that it failed.
- A page token that is otherwise valid and NUL-free still decodes, and a keyset walk across two
  pages still resumes correctly — the regression that matters most, because an over-broad rejection
  breaks real pagination.
- **One test per row of the table above, calling that function directly.** Not one test behind a
  shared helper: the api-keys dispatch shipped a regression test that guarded `get` while
  `delete`'s call site went unmutated, and an uppercase rendering really deleted the row. A shared
  helper with one test behind it looks covered and is not.
- A legitimate percent-encoded `message_id` from fixture 03 and a mixed-case `inbox_id` from
  fixture 18 still resolve, unchanged, through every function you touch.
- `pods::create` and `inboxes::create` with a NUL-bearing `client_id` → `InvalidValue`; and a
  **second** create with the same clean `client_id` still replays to the original resource, proving
  the idempotency path is intact.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export. No JMAP, Sieve, RocksDB, or mailbox-role concept.
- Do not change what an id *means* — no normalisation, no case folding, no trimming beyond what the
  types already do. This dispatch rejects bytes; it does not redefine equality. That distinction
  cost the api-keys dispatch three review rounds.
- Do not widen the rejection beyond NUL without evidence. Control characters, newlines and
  over-long ids are all arguably hostile, but no fixture governs them and `message_id` is a
  famously permissive grammar. If you believe another byte class must be rejected, **STOP and
  report** with the reasoning rather than adding it.
- Do not edit `amk-types`, `amk-core`, the plan, any contract file, or `scripts/hooks/**`.
- Do not commit `.amk-task.md` or `.amk-scope`; they are gitignored dispatch scaffolding.

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-store`, `./scripts/check.sh`,
and the mutation table. **`cargo-mutants` does not mutate string literals**, so mutate the rejection
predicate and every SQL scope pin you touch by hand. Deleting the check must kill a test; so must
inverting it; so must deleting it at **each individual call site** in the table. Name anything you
did not do and why.
