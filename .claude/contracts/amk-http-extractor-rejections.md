# Contract — amk-http: every extractor rejection returns the error envelope

Scope-derivation: `./scripts/derive-request-extractors.sh` (committed with this contract) for the
sites, and `reference/fixtures/27-malformed-request-handling.txt` for the behaviour each site must
produce. The scope is the script's output, not a list anyone recalled; a reviewer re-runs the script
rather than reading the list. **Revised once already**: the first draft of this contract inferred
the target behaviour, the probe that became fixture 27 was run instead, and it reversed two of the
decisions below. What is written here now is observed.

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

`[SPEC:reference/fixtures/27-malformed-request-handling.txt]` — what the REFERENCE does for the
same inputs, which is the target. Read it in full before writing anything; the summary here is not
a substitute. Every malformed request is **400 + `application/json` + the full envelope with
exactly one `errors[]` entry**. There is no 415, no 422 and no plain text anywhere in that surface.

Live probe of OUR server, `amkd --role api` on a throwaway database, `2026-08-16`, root key
presented through a 0600 curl config (never argv). This is what it does today:

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
- `crates/amk-http/src/error.rs` — `with_issue` only, to emit the reference's issue kind for
  `page_token` instead of `custom`. Do not touch anything else in this file.
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

Decisions already made — each now cites the capture that settles it, not a judgement call:

- **Every extractor rejection is 400, `validation_error`, `application/json`, one `errors[]`
  entry.** Not 415, not 422, whatever axum's own rejection would have produced. This is the
  reference's behaviour for every malformed input probed, without exception.
- **Never echo the rejection's `Display`.** Serde's text names our internal Rust types
  (`data did not match any variant of untagged enum MetadataValue`) and describes our
  deserialization structure. That is the disclosure, and it is the only one — see the next point.
- **DO name the offending field.** `errors[0].path` is `["<field>"]` for a field-level failure and
  `[]` only for a body that is not JSON at all. The first draft forbade this as a "schema oracle";
  the reference is deliberately a schema oracle (`{"name":123}` -> `path:["name"]`,
  `expected:"string"`), and hiding it diverges on exactly the request a client debugging its own
  payload will send. The field name came from the caller; our type names did not.
- **Use the issue kind the reference uses**, via the constructors now on
  `amk_types::ValidationIssue` — `invalid_format("json_string", None, …)` for an unparseable body,
  `invalid_type(expected, received, Some(field), …)` for a wrong-typed value,
  `too_small("number", 0, false, Some(field), …)` for a non-positive `limit`,
  `invalid_value("stringbool", values, Some(field), …)` for a bad boolean. `custom` is for
  whole-body rules only.
- **Content-type is NOT enforced.** `POST` with `Content-Type: text/plain`, and with no body and no
  header at all, both return **200** on the reference and create the resource: every P1 request
  type has all-optional fields, so an absent body means `{}`. Our current 415 is wrong in both
  directions — wrong status AND wrong outcome. The wrapper must accept a missing or mismatched
  content-type and treat an empty body as `{}`.
- **An unknown query parameter is IGNORED** (`?nosuchparam=1` -> 200), and `?limit=101` is
  **accepted**, echoing `"limit":101`. Do not add a cap; the `agentmail-cli` help text documenting
  one is not enforced by the API this clones.
- **Fix `?page_token`'s existing issue while you are there.** `crates/amk-http/src/error.rs`'s
  `with_issue` emits `custom` with a field path; the reference emits
  `invalid_format`/`format:"base64url"`/`path:["page_token"]`. Same fixture, same defect class.

## Assigned edge cases

Each is a test in `tests/extractor_rejections.rs`. Every one asserts **status, content-type, and
the parsed envelope** — a test that checks only `code()` is what let this ship.

1. Body is not JSON at all (`not json`, `$'\x00'`, empty string with the JSON content-type).
2. Body is valid JSON of the wrong type (`{"name":123}`) -> 400, `invalid_type`,
   `expected:"string"`, `path:["name"]`. Assert the whole `errors[0]` object, not just the code.
3. Body is valid JSON naming a field the target type does not have. Assert what your wrapper does
   and say so in the test name; no capture covers it, so mark the choice `[INFERRED]` in a comment.
4. `Content-Type: text/plain`, and no `Content-Type` header at all: both **200**, resource created,
   matching the reference. Not 400, not 415.
5. No body at all on a `POST`: **200**, treated as `{}`.
6. Query: `?limit=abc` -> `invalid_type`/`received:"NaN"`; `?limit=-1`, `?limit=`, `?limit=0` ->
   `too_small`/`minimum:0`/`inclusive:false`; `?ascending=maybe` -> `invalid_value` with the full
   `values` list. Each asserts the whole `errors[0]`.
7. `?nosuchparam=1` -> 200, ignored. `?limit=101` -> 200 and the response echoes `limit:101`.
8. `?page_token=%00` -> 400 with `errors[0]` = `invalid_format`, `format:"base64url"`,
   `path:["page_token"]` — this CHANGES, it is no longer `custom`.
9. Every one of the 14 rewritten sites is reachable: one test per HTTP operation that takes a body
   or a query, at every mount, proving the wrapper is actually wired there and not just defined.
   A table-driven test is fine; 14 assertions are not optional.
10. Two bodies differing only in a field NAME produce identical response bodies (the oracle test).

## Prohibitions

- No changes to `amk-types`. If the envelope cannot express something you need, **STOP and report**.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` types anywhere.
- No Stalwart or JMAP concept, field, or name.
- No new dependency. `axum::extract::rejection::*` is already available.
- Do not change handler logic, store calls, or any success path — EXCEPT that an absent or
  wrong-typed `Content-Type` must now reach the handler as `{}` rather than being rejected, which
  by design turns four previously-failing requests into successful creates. That is the reference's
  behaviour and is the point.
- Do not "improve" the query-parameter parsing while you are in there.
- If the contract is ambiguous or appears wrong, **STOP and report**. Do not resolve it yourself.

## Reporting

Report the command you ran and its actual output. "Tests pass" without the output is not a report.

Required in the report:
- `cargo test -p amk-http` output, and `./scripts/check.sh` output.
- The re-run of `./scripts/derive-request-extractors.sh` showing section 4 now listing every
  wrapper, and sections 1–2 showing no bare `Json<`/`Query<` left in argument position.
- A re-run of the probe table against your build, pasted verbatim, showing every row matching
  `reference/fixtures/27-malformed-request-handling.txt` — including the two content-type rows now
  returning 200 rather than 415.
- Your mutation pass, **both directions**, on a **private scratch copy outside the worktree**:
  delete each wrapper's rejection arm (must kill a test) and widen it (map every rejection to the
  same message including the control's — must also kill a test). **Delete the scratch copy when the
  pass ends and confirm the deletion in your report.**
