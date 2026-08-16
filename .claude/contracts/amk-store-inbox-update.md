# amk-store — `inboxes::update`, and free-form control-plane text — dispatch contract

Scope-derivation: `awk` over `.bind(` in `crates/amk-store/src/{inboxes,pods,api_keys,organizations}.rs`,
excluding id-typed and numeric binds — **six** free-form caller-supplied fields across five functions,
listed in full below. Plus one missing function found by the pre-dispatch review of
`.claude/contracts/amk-http.md` against `amk-store`'s actual public surface.

Written by the orchestrator before dispatch. The design decisions here are settled; the implementer
resolves ordinary coding detail inside them and escalates anything else.

**`amk-http` cannot start until this lands.** Two of its 25 first-dispatch operations — `PATCH
/v0/inboxes/{inbox_id}` and `PATCH /v0/pods/{pod_id}/inboxes/{inbox_id}` — have no persistence path
at all: `amk-store::inboxes` exports `create`/`get`/`list`/`delete` and nothing else. This is the
same gap that produced the api-keys dispatch, found the same way: checking `amk-http`'s contract
against `amk-store`'s real surface **before** dispatching, rather than after an implementer stopped.

## What this dispatch is

1. A new `inboxes::update`, implementing `amk_types::inbox::UpdateInboxRequest`.
2. Guards on the free-form caller-supplied text the control plane binds, which the id-safety
   dispatch deliberately left alone and which has no other owner.

## `[SPEC:*]` citations

- `[SPEC:openapi]` `type_inboxes:UpdateInboxRequest`, verbatim: *"Metadata to merge into the inbox's
  existing metadata. Keys you include are added or overwritten; keys you omit are left unchanged. To
  remove a single key, send it with a null value. To clear all metadata, send `metadata` as null.
  Sending an empty object is rejected; use null to clear. Each update must include at least one of
  `display_name` or `metadata`."*
- `[TESTED]` `reference/fixtures/22-org-mount-and-delete-semantics.txt` — `DELETE /v0/inboxes`
  returns **202**, i.e. inbox mutation is not necessarily synchronous at the wire. Not your problem
  here (this is a store function, and it returns when the row is written), recorded so you do not
  invent an async path.
- `[TESTED]` `reference/fixtures/18-inbox-case-normalization.txt` — `inbox_id` folds ASCII case.
  `update` resolves its target exactly as `get` does.

`amk_types` is **frozen** and already models this completely — `MetadataUpdate` is a three-state
enum (`Unchanged` / `Clear` / `Merge`), not `Option<Option<…>>`. **Use it. Do not add a type.**

## Writable paths (exact)

`crates/amk-store/**`, plus the workspace `Cargo.lock` only if a sanctioned dependency is added
(none should be). Nothing else. If the work requires a path outside that tree, **STOP and report**.

## Decisions (settled — implement, do not relitigate)

### `inboxes::update` — signature and semantics

`update(pool, organization_id, pod_id: Option<PodId>, inbox_id, req: UpdateInboxRequest) ->
Result<Option<Inbox>, StoreError>`. `Ok(None)` when the inbox does not resolve in that scope.

**Do not "match `get`" — `get` is the thing that is wrong.** An earlier draft of this contract said
to copy `inboxes::get`'s scope pinning. The pre-dispatch review checked, and `get` and `delete` pin
**`organization_id` only**:

```
inboxes.rs:129   FROM inboxes WHERE organization_id = $1 AND inbox_id = $2
inboxes.rs:169   DELETE FROM inboxes WHERE organization_id = $1 AND inbox_id = $2
inboxes.rs:147   WHERE organization_id = $1 AND ($2::uuid IS NULL OR pod_id = $2)   <- list, correct
```

So a pod-scoped credential reaching a **sibling pod's** inbox in the same organization resolves it,
and can delete it. That is a cross-pod read and a cross-pod delete, and the plan requires scope
denial to mask as `not_found` **in the query**, never by post-filtering what came back.

**So this dispatch also fixes `get` and `delete`.** All three take `pod_id: Option<PodId>` and pin it
the way `list` already does — `($n::uuid IS NULL OR pod_id = $n)`, where `None` means the org mount
legitimately spans pods. `PodId` is UUID-typed, so the NUL-widening hazard that made a `None` bind
catastrophic for `inbox_id` does not apply here; this is the one place the `IS NULL OR` idiom is
correct, and `api_keys::{get,list,delete}` are the pattern to copy. Existing callers are tests only
— `amk-http` does not exist yet — so the signature change is contained.

Write the cross-pod test **before** the pin: seed two pods in one organization, then assert that
`get`, `delete` and `update` at pod A all return not-found for pod B's inbox **and that B's row is
unchanged afterwards**. A denial that still writes is the defect.

The three `MetadataUpdate` states map to three different SQL effects:

| State | Wire | Effect on the `metadata` JSONB column |
|---|---|---|
| `Unchanged` | field absent | column untouched |
| `Clear` | `"metadata": null` | set to SQL `NULL` |
| `Merge(map)` | `"metadata": {…}` | merge non-null keys, **delete** null-valued keys |

### The merge trap — derived, not assumed

Postgres `||` does **not** implement the wire semantics. Measured against the dev database:

```
'{"a":1}'::jsonb || '{"b":null}'::jsonb   →   {"a": 1, "b": null}
'{"a":1,"b":2}'::jsonb - 'b'              →   {"a": 1}
```

**A null-valued key survives `||` as a stored JSON null; it does not delete.** The spec says a null
value *removes* that key. So `Merge` is two operations, not one: concatenate the non-null entries,
then remove each null-valued key. Reaching for `||` alone silently stores nulls where a caller asked
for deletion, and no round-trip test that only checks the non-null keys would notice.

**And the obvious expression of that is also wrong, in a way that only shows on a NULL column.**
The first draft of this contract offered `(COALESCE(metadata,'{}') || $adds) - $dels::text[]`. The
review ran it: starting from `metadata = NULL`, a merge that nets to nothing yields `{}`, not NULL —
so a call this contract's own edge cases define as a **no-op** silently changes the column, and it
is wire-visible, because `Inbox.metadata` is `Option<Metadata>` with `skip_serializing_if =
"Option::is_none"`: `None` is omitted, `Some({})` serialises as `"metadata":{}`. A row that omitted
the field starts emitting an empty object after a request that asked for nothing.

Split the merge map into `adds` (entries with a value) and `dels` (keys mapped to null), and guard:

```sql
CASE WHEN metadata IS NULL AND $adds = '{}'::jsonb THEN NULL
     ELSE (COALESCE(metadata,'{}') || $adds) - $dels::text[] END
```

Verified against the dev database, all five cases:

```
NULL            + {}       - {}     =>  NULL          (no-op stays NULL)
NULL            + {}       - {x}    =>  NULL          (deleting from nothing is nothing)
NULL            + {"a":1}  - {}     =>  {"a": 1}
{"a":1}         + {}       - {}     =>  {"a": 1}      (untouched)
{"a":1,"b":2}   + {"c":3}  - {a}    =>  {"b": 2, "c": 3}
```

**It must be one atomic `UPDATE` statement.** Do not read the current metadata, merge it in Rust,
and write it back: two concurrent `Merge` requests touching different keys would lose one, and none
of the assigned tests is concurrent, so nothing here would catch it. This crate already designs
against exactly that class — `inboxes::create` uses a real `ON CONFLICT` rather than
check-then-insert for the same reason. If you believe the merge cannot be expressed in one
statement, **STOP and report** rather than reaching for read-modify-write.

### Wire validation is **not** yours

"Sending an empty object is rejected" and "each update must include at least one of `display_name`
or `metadata`" are request-validation rules that produce a `validation_error` envelope, and
`amk-store` has no business constructing wire errors. They belong to `amk-http`. Here,
`Merge(empty)` is a **no-op on metadata** — not an error, not a clear. If `display_name` is also
absent the whole call is a no-op that still returns the current row. Say so in a test, because "the
store rejects it too" is the tempting wrong answer and it would double-own the rule.

### `updated_at`

Bump it when anything changed. A no-op update must **not** bump it — `updated_at` is on the wire
(`Inbox.updated_at`, required) and a client polling it would see phantom changes.

**"Changed" means a field was present, not that its value differs.** A caller resending
`display_name` byte-identical to the stored value bumps `updated_at`; an absent field and a
`Merge` that nets to nothing do not. Presence, not value-equality — settled here because nothing in
`amk_types` settles it and the two readings are equally defensible.

### Free-form text guards — the five fields

The id-safety dispatch guarded every id-typed value and deliberately excluded content fields,
reasoning that hostile bytes in *mail* content are `amk-ingest`'s call in P2. These five are not
mail content — they are control-plane fields with no P2 owner, so the decision falls here:

| Function | Field | Column |
|---|---|---|
| `inboxes::update` | `inbox_id` (the lookup) | `TEXT` — returns `Ok(None)`, not `InvalidValue` |
| `inboxes::create` | `display_name` | `TEXT` |
| `inboxes::create` | `metadata` | `JSONB` |
| `inboxes::update` | `display_name` | `TEXT` |
| `inboxes::update` | `metadata` | `JSONB` |
| `pods::create` | `name` | `TEXT` |
| `api_keys::create` | `name` | `TEXT` |

Each rejects a forbidden byte with `StoreError::InvalidValue(<field>)`, one distinct label per
field, using `amk_types::ids::has_forbidden_byte` — **except the first row**: `update`'s own
`inbox_id` is a lookup, so it returns `Ok(None)` exactly as `get` and `delete` already do. It is in
this table because the id-safety contract's table named only the functions that existed then, and
inheriting the rule "by matching `get`" is precisely how that project already shipped a guarded
`get` beside an unguarded `delete` that survived a mutation with the suite green. These are creates and updates: there is no
not-found to mask into, and silently stripping a byte changes what the caller stored.

**Metadata is exposed through both its keys and its values,** which is easy to half-fix. Measured:

```
('{"k":"a\0b"}')::jsonb   →   ERROR 54000  null character not permitted
('{"a\0b":"v"}')::jsonb   →   ERROR 54000  null character not permitted
```

So check every key **and** every value, at every nesting level `MetadataValue` permits. A test with
the NUL only in a value would pass while keys stayed open.

`api_keys::create`'s `permissions` needs no guard: its JSON keys are the fixed 36 flag names and its
values are booleans, so no caller string reaches the column. Recorded so its absence is a decision.

## Assigned edge cases (write the test before the code it targets)

- Each of the three `MetadataUpdate` states, asserted by reading the row back: absent leaves prior
  metadata byte-identical; `Clear` sets SQL NULL (assert NULL, not `{}`); `Merge` adds, overwrites,
  and **deletes on a null value**.
- `Merge` with a null value for a key that does not exist — a no-op, not an error.
- `Merge(empty)` and a fully-empty request: no error, no metadata change, `updated_at` unchanged.
- `update` on an inbox in another pod → `Ok(None)`, and the row is **unmodified** — assert the
  target row afterwards, not just the return value. A scope miss that still writes is the defect.
- Mixed-case `inbox_id` resolves the same inbox (fixture 18).
- One test per row of the five-field table, calling that function directly, plus a **clean-path
  test per field** so a widened guard cannot pass.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export. No JMAP, Sieve, RocksDB, or mailbox-role concept.
- Do not edit `amk-types`, `amk-core`, the plan, any contract file, or `scripts/hooks/**`.
- Do not add a metadata type, a validation error type, or a wire shape. If something you need does
  not exist, **STOP and report**.
- Do not commit `.amk-task.md` or `.amk-scope`.

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-store`, `./scripts/check.sh`,
and a **two-directional** mutation table. `cargo-mutants` does not mutate string literals, so mutate
by hand. Every guard gets both: delete it (must kill a test) **and** widen it — `is_some_and(pred)`
→ `is_some()`, `any(pred)` → `!is_empty()` — which must also kill a test. A deletion-only pass is
structurally blind to an over-broad guard; that is how a live mutant survived twenty mutations in
the previous dispatch. Also mutate the merge: replacing the delete step with plain `||` must kill a
test. Name anything you did not do and why.
