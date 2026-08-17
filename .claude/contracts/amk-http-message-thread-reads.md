# Contract — amk-http: the message and thread READ surface (P2, first slice)

Scope-derivation: the operation set is the intersection of what `reference/openapi.json` describes
and what `amk-store` already implements, both enumerated by command rather than recalled.

```
$ python3 -c "…paths matching /messages|/threads|/attachments|/drafts…"   # see below for the full list
  -> 44 operations across messages, threads, drafts and attachments

$ grep -n "^pub async fn " crates/amk-store/src/messages.rs crates/amk-store/src/threads.rs
  messages.rs:54   insert          messages.rs:200  get          messages.rs:288  list
  threads.rs:51    insert          threads.rs:147   get_with_messages          threads.rs:229  list

$ ./scripts/derive-implemented-paths.sh | tail -3
  spec describes 130 operations; this phase mounts 25 (105 out of scope for P1)
```

`amk-store` offers exactly `get` and `list` for both resources. This slice mounts exactly those,
at every mount the spec describes, and nothing else. **The write order is why**: `amk-store` before
`amk-http`, so an operation with no storage behind it does not get an endpoint first.

## The eight operations in scope

```
GET /v0/inboxes/{inbox_id}/messages                    -> messages::list
GET /v0/inboxes/{inbox_id}/messages/{message_id}       -> messages::get
GET /v0/threads                                        -> threads::list   (organization mount)
GET /v0/inboxes/{inbox_id}/threads                     -> threads::list   (inbox mount)
GET /v0/pods/{pod_id}/threads                          -> threads::list   (pod mount)
GET /v0/threads/{thread_id}                            -> threads::get_with_messages
GET /v0/inboxes/{inbox_id}/threads/{thread_id}         -> threads::get_with_messages
GET /v0/pods/{pod_id}/threads/{thread_id}              -> threads::get_with_messages
```

## Explicitly OUT of scope, each with the reason it is deferred rather than forgotten

- `…/messages/search`, `…/threads/search` — **FTS is deferred by decision** (`CLAUDE.md`, and the
  plan's own deferral table). Mounting a search endpoint backed by `LIKE` would be a shape the
  reference does not have.
- `…/attachments/{attachment_id}`, `…/messages/{message_id}/raw` — **blobs and signed download URLs
  are deferred by decision**. There is no blob store to serve from.
- `…/messages/batch-get`, `…/messages/batch-update` — the plan parks batch endpoints ("not needed by
  v1 consumers") under *Full parity*.
- `…/messages/send`, `…/reply`, `…/reply-all`, `…/forward` — need `amk-outbound`, which is later in
  the P2 write order.
- `PATCH`/`DELETE` on messages and threads — **`amk-store` has no update or delete for either**.
  Writing the endpoint first is exactly the inversion the write order exists to prevent. Fixture 19
  (`19-message-label-patch-gate.txt`) is the evidence that governs them when they are dispatched.
- All `/drafts` operations — **P3**.

## The evidence

`[SPEC:reference/openapi.json]` — the response shapes. `amk-types::message` and `amk-types::thread`
already carry `Message`, `MessageItem`, `Thread`, `ThreadItem` and their list envelopes; **this
dispatch adds no type**. If a field appears missing, STOP — do not add it.

`[SPEC:reference/fixtures/04-pagination.http]` — page tokens are `base64(JSON keyset cursor)` and
the token is **absent** on the last page. `amk-store::pagination` already owns the cursor types;
reuse them exactly as `inboxes` does.

`[SPEC:reference/fixtures/03-id-formats.http]` — `message_id` **is** an RFC 5322 angle-bracket
Message-ID (`<…@…>`), so a path segment carrying one is percent-encoded (`<`, `>`, `@`).
`crate::ids::decode_segment` is the existing mechanism; a NUL-bearing segment masks as not-found,
never as a distinct error shape. Do not invent a second decoder.

`[SPEC:reference/fixtures/20-search-and-label-precedence.txt]` — restricted-label mail
(`unauthenticated`, `spam`, `blocked`, `trash`) is **excluded from list endpoints** and reachable by
id. `amk-store`'s `list`/`get` already take `excluded_labels` / `LabelAccess`; the composed rule is
`amk-core::labels`' to own (register B3). **Admission must stay a storage-layer predicate** — a
page filtered after fetch discloses how many rows were hidden, which is the leak B3 names.

`[SPEC:reference/fixtures/05-error-catalog.http]` — a missing message or thread is the ordinary
`not_found` envelope; an out-of-scope one is indistinguishable from an absent one.

## Writable paths

- `crates/amk-http/src/handlers/messages.rs` — NEW.
- `crates/amk-http/src/handlers/threads.rs` — NEW.
- `crates/amk-http/src/handlers/mod.rs` — the two `mod` declarations only.
- `crates/amk-http/src/lib.rs` — the `.route()` calls only. Touch nothing else in `router()`.
- `crates/amk-http/tests/messages.rs` — NEW.
- `crates/amk-http/tests/threads.rs` — NEW.
- `crates/amk-http/tests/support/mod.rs` — only if a seed helper for messages/threads is missing;
  say exactly what you added.
- `crates/amk-http/src/pagination.rs` — **for one addition only**: a `ListMailQuery` carrying the
  four `include_*` flags alongside the existing `limit`/`page_token`/`ascending`. Four of the eight
  operations here take those flags (`amk-core::labels::LabelAccess::list`'s own doc names exactly
  which four), and `ListQuery` cannot express them. It must **reuse `parse_limit`** rather than
  re-derive the `limit` rules — two representations of one rule is the `ApiKeyPermissions`
  collision the plan already records. Change nothing else in the file; `ListQuery`,
  `ListQueryNoDirection`, `DEFAULT_LIMIT` and `direction_for` stay exactly as they are.

  *Added in revision 2, before dispatch.* The first draft omitted it while mandating endpoints that
  cannot be written without it — the same defect that made the extractor contract unsatisfiable
  twice. Recording it here rather than quietly widening the list: a contract that mandates a
  parameter must make the type that carries it writable.

Nothing else. In particular **not** `crates/amk-types/**` (frozen), **not** `crates/amk-store/**`
(its `get`/`list` are the contract, not something to extend here), not `crates/amk-core/**`, not
`scripts/**`, not the plan, not this contract.

## What to build

Mirror `handlers/inboxes.rs` exactly — it is the worked example for a resource mounted three ways:
`organization_window` for the org mount, `settle_pod_mount` for the pod mount,
`window_for_pod_own_resource` for a pod-mounted resource fetched by id, and `PathPodId` /
`PathPodIdString` for the typed path extractors. Query strings use `QueryParams<ListQuery>` and
bodies would use `JsonBody<T>` — **never bare `Query<T>`/`Json<T>`**, per
`.claude/contracts/amk-http-extractor-rejections.md`, which `./scripts/derive-request-extractors.sh`
enforces.

Permissions: `message_read` and `thread_read` gate their respective resources, via
`permissions::require`, in the same position `inboxes.rs` puts it — **after** mount settlement and
**before** the store call.

## Assigned edge cases

Each asserts status, content-type and the parsed body.

1. A `message_id` with `<`, `>` and `@` percent-encoded in the path resolves; the same id
   unencoded does not silently resolve to something else.
2. A NUL byte (`%00`) in `message_id` or `inbox_id` is **not-found**, never a 500 and never a
   distinct error code — the masking rule `malformed_path_ids.rs` already pins for inboxes.
3. Restricted-label mail is **absent from every list mount** and **present by id** — assert both
   halves against the same seeded row, or the test proves nothing.
4. `?limit=1` walked across the cursor: the last page **omits** `next_page_token`. Seed at least two
   rows explicitly; a page boundary that depends on rows an earlier test left behind is what the
   SDK smokes did, and it made a green depend on leftover state.
5. An inbox-scoped credential listing threads sees only its own inbox's; a pod-scoped one sees the
   pod's; an organization-scoped one sees the organization's. One test per mount per scope.
6. A thread id belonging to another organization is **not-found**, not 403 — scope misses mask.
7. `GET /v0/threads/{thread_id}` returns the thread **with its messages**; assert the message list
   is populated, not merely that the thread exists.
8. Boundary and one unit either side on `limit`, per the plan's testing rules: `limit=0` returns an
   empty page (`amk-store` guards this explicitly), `limit=1`, `limit=2` across two seeded rows.

## Prohibitions

- No new type in `amk-types`, and no new column or query in `amk-store`. If either seems necessary,
  **STOP and report** — that is a different dispatch, and the write order says it comes first.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` type in any signature; no
  Stalwart or JMAP concept, field or name.
- No post-fetch filtering of restricted labels. Storage-layer predicate or nothing.
- Do not mount any operation from the out-of-scope list above, "while you are in there", including
  a stub that returns 501 — an unmounted path and a path that answers wrongly are different, and
  `derive-implemented-paths.sh` reconciles the mounted set against the spec on every run.
- Do not resolve register **C2** (whether a thread's labels are a strict union of its members').
  It is the one open boundary question; the fail-closed choice is implemented and marked
  `[INFERRED]` in `amk-core::labels`. Leave it.
- If the contract is ambiguous or appears wrong, **STOP and report**. Do not resolve it yourself.

## Reporting

Report the command run and its actual output; "tests pass" without the output is not a report.

- `cargo test -p amk-http` and `./scripts/check.sh`.
- `./scripts/derive-implemented-paths.sh`, showing the mounted set grown by exactly these eight
  operations and still reconciling clean against `openapi.json`.
- `./scripts/derive-request-extractors.sh`, showing no bare `Json<`/`Query<` in argument position.
- A mutation pass in **both directions** on a scratch copy outside the tree: delete the
  restricted-label exclusion (must kill a test) and widen the scope window to the organization on a
  pod-mounted read (must also kill a test). Delete the scratch copy and confirm it.
