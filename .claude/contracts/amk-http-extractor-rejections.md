# Contract — amk-http: every extractor rejection returns the error envelope

Scope-derivation: `./scripts/derive-request-extractors.sh` (committed with this contract) for the
sites, and `reference/fixtures/27-malformed-request-handling.txt` for the behaviour each site must
produce. The scope is the script's output, not a list anyone recalled; a reviewer re-runs the script
rather than reading the list. **Revised once already**: the first draft of this contract inferred
the target behaviour, the probe that became fixture 27 was run instead, and it reversed two of the
decisions below. What is written here now is observed.

**Revised a third time (2026-08-17), and this revision widened the writable set.** The first
dispatch against this contract could not satisfy it: the contract mandates a `max_body_bytes` field
on `AppConfig` but did not make `AppConfig`'s other construction site writable, and it mandates
`limit` behaviour that `crate::pagination`'s own types cannot express. Both are the SAME defect the
contract already lectures about — enumerating the *extractor* sites is not enumerating the sites
that a mandated change reaches. Two more derivations, run and pasted rather than recalled:

```
$ grep -rn "AppConfig\s*{" --include=*.rs crates/ | grep -v "pub struct"
crates/amk-http/src/config.rs:44:impl Default for AppConfig {          # the impl itself
crates/amk-http/tests/support/mod.rs:52:    let config = AppConfig {   # already writable
crates/amk-cli/src/config.rs:50:pub fn app_config() -> AppConfig {     # NOT writable -> E0063
crates/amk-cli/src/config.rs:51:    AppConfig {

$ grep -rn "\.resolve()" --include=*.rs crates/amk-http/src   # every consumer of the limit rules
crates/amk-http/src/handlers/pods.rs:42        crates/amk-http/src/handlers/api_keys.rs:46,59,72
crates/amk-http/src/handlers/inboxes.rs:89     crates/amk-http/src/pagination.rs (its own tests)
```

An exhaustive struct literal is not rescued by adding `impl Default`, so `crates/amk-cli` fails to
compile and the workspace gate can never go green. **The rule this buys: a contract that mandates a
change to a type must make every construction site of that type writable, or it is unsatisfiable by
construction.** Site enumeration is not variant enumeration, and it is not construction-site
enumeration either — this is now the third form of the same mistake in one dispatch.

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
- `crates/amk-http/tests/support/mod.rs` — REQUIRED, not conditional. `TestResponse` carries only
  `{status, json, body}` and cannot assert a content-type, which every edge case below demands; and
  `send()` always sets `content-type: application/json` for any `Some(body)`, so it cannot express a
  raw non-JSON body, a custom content-type, or a POST with no body and no header at all — cases 1,
  4 and 5. Extend it; say in your report exactly what you added.
- `crates/amk-http/src/config.rs` — the body-limit field only.
- `crates/amk-cli/src/config.rs` — **added in revision 3**, and ONLY to keep `app_config()`
  compiling against the new `AppConfig` field. It reads two environment variables and passes them
  through; that must stay true. Do not give `max_body_bytes` an environment variable of its own,
  do not add a third `AMK_*` name, and change nothing else in the file.
- `crates/amk-http/src/pagination.rs` — **added in revision 3**, for the `limit` rules below only.
  `page_token` and `ascending` handling, `DEFAULT_LIMIT`, and `direction_for` are NOT in scope.

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
- **`limit` must be parsed explicitly, not by serde's integer impl** — revision 3, and the reason
  the previous bullet was unreachable. Fixture 27 §1 requires `?limit=-1`, `?limit=` and `?limit=0`
  to produce *byte-identical* `too_small` bodies (`origin:"number"`, `minimum:0`,
  `inclusive:false`, `path:["limit"]`), while `?limit=abc` produces `invalid_type`/`received:"NaN"`.
  `Option<u64>` cannot express that distinction: `"-1"`, `""` and `"abc"` all fail `u64::from_str`
  identically, and `"0"` succeeds and never reaches a validator at all. So `limit` deserializes as
  a raw `Option<String>` and is classified by a function in `pagination.rs`:
  - empty, or an integer `<= 0` -> `too_small`;
  - anything else that is not a non-negative integer -> `invalid_type` / `received:"NaN"`;
  - otherwise accepted **verbatim, uncapped**, and echoed verbatim.
  This deletes the `limit` half of `body.rs`'s serde-message string matching rather than adding to
  it, which is the point: a structured classifier cannot be broken by an upstream reword.
  `MAX_LIMIT` and the clamp go with it — a clamp is what made `?limit=101` echo `100`. `ascending`
  keeps its existing `Option<bool>` + rejection-text path; it already produces fixture 27's body
  exactly, and widening it is out of scope.
- **Uncapping `limit` is a considered, bounded risk, not an oversight.** Fixture 27 observed 101
  accepted and the reference's own ceiling was deliberately not probed. `limit` reaches Postgres as
  a `LIMIT` bound, so a pathological value returns at most the caller's own scoped rows and buys no
  amplification; it is not the unbounded *buffer* the body-size limit above exists to prevent, and
  the two must not be conflated. If a ceiling is ever wanted it is a plan decision with a fixture
  behind it, not something to reintroduce quietly here.
- **Body size: 413 is a third off-catalog status, and the limit itself diverges.** `JsonRejection`
  composites `BytesRejection` -> `FailedToBufferBody` -> `LengthLimitError`, which axum-core marks
  `#[status = PAYLOAD_TOO_LARGE]` against an unconditional 2 MB `DEFAULT_LIMIT` that applies whether
  or not a `DefaultBodyLimit` layer is installed (this crate installs none). Ours answers a 3 MB
  body with `413 text/plain`; the reference buffers and parses the same 3 MB body and returns the
  ordinary 400 syntax error. So: install an explicit `DefaultBodyLimit` on the router, set it from
  `AppConfig` with a default of **8 MiB**, and map the length-limit rejection to the same 400
  envelope as every other variant, with `ValidationIssue::custom`. The 8 MiB default is
  **`[INFERRED]`** — mark it so in the code — and here is the whole reasoning, because it is a
  number nobody observed: the reference accepts 3 MB, its true ceiling was deliberately not probed
  (finding it means firing progressively larger payloads at someone else's production API), and the
  one size this project has actually measured is the ~5.95 MB inline attachment threshold
  `[SPEC:repo agentmail-toolkit]`, which P2 bodies must clear. 8 MiB clears it with headroom and
  still bounds the buffer. Do NOT remove the limit: unbounded body buffering is a denial-of-service
  primitive on a public endpoint, and "match the reference exactly" is not worth that.
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
10. A body naming a field that exists and one naming a field that does not: assert what each
    returns. They may legitimately differ now — the reference is a schema oracle — so this test
    records the behaviour rather than forbidding a difference.
11. Body size: at the limit, one byte under, and one byte over (the boundary and one unit either
    side, per the plan's testing rules). Over the limit is 400 with the envelope, never 413, never
    `text/plain`. Use a body that is INVALID JSON so an oversized request that slips through the
    limit still cannot create anything — the same construction fixture 27 §5 used.

## Prohibitions

- No changes to `amk-types`. If the envelope cannot express something you need, **STOP and report**.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` types anywhere.
- No Stalwart or JMAP concept, field, or name.
- No new dependency. `axum::extract::rejection::*` and `axum::extract::DefaultBodyLimit` are
  already available.
- Match `JsonRejection` and `QueryRejection` EXHAUSTIVELY — no `_ =>` arm. Both are
  `#[non_exhaustive]`, so a catch-all is unavoidable at the end; write it as an explicit arm with a
  comment naming that fact, never as a silent fallthrough, and make it produce the envelope too. The
  413 variant reached production precisely because nobody enumerated the variants, only the sites.
- Do not change handler logic, store calls, or any success path — EXCEPT that an absent or
  wrong-typed `Content-Type` must now reach the handler as `{}` rather than being rejected, which
  by design turns four previously-failing requests into successful creates. That is the reference's
  behaviour and is the point.
- Do not "improve" the query-parameter parsing while you are in there. The `limit` classifier above
  is the ONE carve-out, and it is mandated rather than optional: `page_token` and `ascending` keep
  the parsing they have.
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
