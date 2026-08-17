# Contract — amk-http: every extractor rejection returns the error envelope

Scope-derivation: `./scripts/derive-request-extractors.sh` (committed with this contract), plus the
live probe transcript below. The scope is the script's output, not a list anyone recalled. A
reviewer re-runs the script rather than reading the list.

## Why this dispatch exists

`.claude/contracts/amk-p1-divergences.md` asked "which typed **path** extractors can reject before
a handler runs?", enumerated 17 `Path<Uuid>` sites, and closed all 17 correctly. It never asked
about the request **body** or the **query string**, and axum rejects those the same way: a
`text/plain` body, a status the error catalog does not contain, and — for a deserialization
failure — serde's own message naming our internal Rust types.

The plan already records that a contract's scope is derived, never recalled. This dispatch
generalises it: **the derivation must enumerate the class, not the instance that prompted it.**
Every axum extractor with a `Rejection` type is a way out of the JSON error contract, and asking
about one member of that class left the other two open for the whole of P0 and P1.

Found by the P1 schemathesis run (`--mode all`, negative data), not by any of the 87 amk-http
tests — every one of which asserts `resp.code() == Some(...)` on a body it already knows is JSON.

## The evidence

`[SPEC:reference/fixtures/05-error-catalog.http]` — the two error shapes, and the ONLY two. Auth-layer
failures are a bare `{"message":…}` at 401/403; every application failure is the full envelope
`{name, code, message, …}`. A client branches on `code`. A body with no `code` and no `message` is
unbranchable by either contract.

`[SPEC:reference/fixtures/25-p1-gate-conformance.txt]` — divergence 3 established the pattern this dispatch
extends: `crates/amk-http/src/ids.rs`'s `PathPodId` / `PathPodIdString` wrap axum's own extractor
and set `type Rejection = AppError`. Do the same thing; do not invent a second mechanism.

Live probe, run against `amkd --role api` on a throwaway database at `2026-08-16`, root key
presented through a 0600 curl config (never argv). This is the complete observed behaviour today:

```
/v0/pods?limit=abc                    400 text/plain  Failed to deserialize query string: limit: invalid digit found in string
/v0/pods?limit=-1                     400 text/plain  Failed to deserialize query string: limit: invalid digit found in string
/v0/pods?page_token=%00               400 application/json  {"name":"ValidationError","code":"validation_error",...}   <- CORRECT, handler-level
POST /v0/pods  -d 'not json'          400 text/plain  Failed to parse the request body as JSON: expected ident at line 1 column 2
POST /v0/pods  -d '{"name":123}'      422 text/plain  Failed to deserialize the JSON body into the target type: name: invalid type: integer `123`, expected a string at line 1 column 11
POST /v0/pods  -H 'Content-Type: text/plain'   415 text/plain  Expected request with `Content-Type: application/json`
POST /v0/pods  (no body, no header)   415 text/plain  Expected request with `Content-Type: application/json`
```

Three defects, in order of severity:

1. **Information disclosure.** `invalid type: integer 123, expected a string` and the fuzzer's
   `data did not match any variant of untagged enum MetadataValue` name our internal Rust types and
   describe our deserialization structure, to any unauthenticated-shaped request on a public
   multi-tenant API. Nothing the reference API emits does this.
2. **415 is not in the catalog.** `ErrorCode::status()` yields exactly
   `{400, 401, 403, 404, 409, 422, 429, 500, 503}`. 415 is a status this API has no code for, so no
   client can branch on it.
3. **`text/plain` breaks both error contracts** and the conformance diff's `content-type` compare.
   The `?page_token=%00` row is the control: handler-level validation already produces the right
   envelope, so this is purely the extractors escaping.

## Writable paths

- `crates/amk-http/src/body.rs` — NEW. The wrapping extractors.
- `crates/amk-http/src/lib.rs` — the `mod body;` declaration only.
- `crates/amk-http/src/handlers/api_keys.rs`
- `crates/amk-http/src/handlers/inboxes.rs`
- `crates/amk-http/src/handlers/pods.rs`
- `crates/amk-http/tests/extractor_rejections.rs` — NEW. The tests.
- `crates/amk-http/tests/support/**` — only if a helper genuinely needs extending; say so if you do.

Nothing else. In particular **not** `crates/amk-types/**` (frozen — the envelope already has
everything needed; `ErrorEnvelope::new(ErrorCode::ValidationError, msg)` now routes `msg` into
`errors[0]` for you), not `crates/amk-store/**`, not `scripts/**`, not the plan, not this contract.

## What to build

A `JsonBody<T>` and a `QueryParams<T>` in `body.rs`, each wrapping axum's own extractor with
`type Rejection = AppError`, replacing `Json<T>` at the 8 sites and `Query<T>` at the 6 sites the
derivation lists. Mirror `ids.rs`'s existing shape — same crate, same idea, one mechanism.

Decisions already made; do not re-open them:

- **Status is 400, `validation_error`, for every rejection variant** — including the ones axum
  gives 415 and 422. 400 is what the catalog has for a malformed request, `validation_error` is the
  code a client branches on, and collapsing the variants is deliberate: the *reason* belongs in
  `errors[]`, not in the status line.
- **Never echo the rejection's `Display`.** Not truncated, not sanitised, not "just the first
  line" — serde's text is the disclosure. Write our own message per variant.
- **The message must not distinguish a field that exists from one that does not.** `{"name":123}`
  and `{"nosuchfield":123}` may not produce different text: that is a schema oracle. One message
  per *variant* (syntax / shape / content-type / missing body), not per field.
- **`errors[0].path` stays `[]`.** Per-field paths would require mapping serde's path back to the
  request, which is the disclosure again. Whole-body rule, empty path — matching `reference/fixtures/05-error-catalog.http`.
- **Query and body get different `errors[0].message` text** (a caller must be able to tell which
  half of the request was wrong) but neither names a type or a field.

## Assigned edge cases

Each is a test in `tests/extractor_rejections.rs`. Every one asserts **status, content-type, and
the parsed envelope** — a test that checks only `code()` is what let this ship.

1. Body is not JSON at all (`not json`, `$'\x00'`, empty string with the JSON content-type).
2. Body is valid JSON of the wrong shape (`{"name":123}`) — and the response must be
   byte-identical to case 3's, since both are "shape wrong".
3. Body is valid JSON naming a field the target type does not have.
4. `Content-Type: text/plain`; and no `Content-Type` header at all. Both 400, not 415.
5. No body at all on a `POST` that requires one.
6. Query: `?limit=abc`, `?limit=-1`, `?limit=` (empty value), `?ascending=maybe`.
7. Query: an unknown parameter (`?nosuch=1`) — assert the CURRENT behaviour, whatever it is, and
   say in the test name which it is. Do not change it in this dispatch.
8. The control, which must keep working unchanged: `?page_token=%00` still returns the handler's
   own `validation_error` envelope with `errors[0].code == "custom"`.
9. Every one of the 14 rewritten sites is reachable: one test per HTTP operation that takes a body
   or a query, at every mount, proving the wrapper is actually wired there and not just defined.
   A table-driven test is fine; 14 assertions are not optional.
10. Two bodies differing only in a field NAME produce identical response bodies (the oracle test).

## Prohibitions

- No changes to `amk-types`. If the envelope cannot express something you need, **STOP and report**.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` types anywhere.
- No Stalwart or JMAP concept, field, or name.
- No new dependency. `axum::extract::rejection::*` is already available.
- Do not change handler logic, store calls, or any success path. This dispatch changes what happens
  when an extractor rejects, and nothing else.
- Do not "improve" the query-parameter parsing while you are in there.
- If the contract is ambiguous or appears wrong, **STOP and report**. Do not resolve it yourself.

## Reporting

Report the command you ran and its actual output. "Tests pass" without the output is not a report.

Required in the report:
- `cargo test -p amk-http` output, and `./scripts/check.sh` output.
- The re-run of `./scripts/derive-request-extractors.sh` showing section 4 now listing every
  wrapper, and sections 1–2 showing no bare `Json<`/`Query<` left in argument position.
- A re-run of the probe table above against your build, showing every row now
  `400 application/json` with the envelope — pasted verbatim.
- Your mutation pass, **both directions**, on a **private scratch copy outside the worktree**:
  delete each wrapper's rejection arm (must kill a test) and widen it (map every rejection to the
  same message including the control's — must also kill a test). **Delete the scratch copy when the
  pass ends and confirm the deletion in your report.**
