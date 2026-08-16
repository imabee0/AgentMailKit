# amk-store — `inboxes::update`, and free-form control-plane text — dispatch contract

Scope-derivation: `awk` over `.bind(` in `crates/amk-store/src/{inboxes,pods,api_keys,organizations}.rs`,
excluding id-typed and numeric binds — five free-form caller-supplied fields across four functions,
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
Result<Option<Inbox>, StoreError>`. `Ok(None)` when the inbox does not resolve in that scope —
the same masking `get` already does, for the same reason. Match `get`'s existing scope-pinning
exactly; a missing pod pin is a cross-pod write.

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

Whether you express that as `(COALESCE(metadata,'{}') || $merge) - $deleted_keys::text[]` in one
statement or build it in Rust is yours; the observable behaviour is not.

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

### Free-form text guards — the five fields

The id-safety dispatch guarded every id-typed value and deliberately excluded content fields,
reasoning that hostile bytes in *mail* content are `amk-ingest`'s call in P2. These five are not
mail content — they are control-plane fields with no P2 owner, so the decision falls here:

| Function | Field | Column |
|---|---|---|
| `inboxes::create` | `display_name` | `TEXT` |
| `inboxes::create` | `metadata` | `JSONB` |
| `inboxes::update` | `display_name` | `TEXT` |
| `inboxes::update` | `metadata` | `JSONB` |
| `pods::create` | `name` | `TEXT` |
| `api_keys::create` | `name` | `TEXT` |

Each rejects a forbidden byte with `StoreError::InvalidValue(<field>)`, one distinct label per
field, using `amk_types::ids::has_forbidden_byte`. These are creates and updates: there is no
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
