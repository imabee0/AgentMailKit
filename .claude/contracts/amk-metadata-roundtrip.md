# Contract — metadata numbers must survive the storage round-trip, or be refused

Scope-derivation: the defect was found by the P1 gate's schemathesis conjunct
(`PATCH /v0/inboxes/{inbox_id}`, `not_a_server_error`, 1959 generated cases, 1 unique failure), and
the writable set below is derived by following the value, not by recall:

```
$ grep -rn "MetadataValue\|Metadata\b" --include=*.rs crates/ | grep -v "^crates/amk-types/src/inbox.rs"
crates/amk-types/src/lib.rs          # re-export only
crates/amk-store/src/inboxes.rs:56   # row -> Json<Metadata> decode  <- WHERE IT 500s
crates/amk-store/src/inboxes.rs:120  # create bind
$ grep -n "metadata" crates/amk-http/src/handlers/inboxes.rs
260  # create: req.metadata passed through
365  # update: validate_update's existing empty-merge rule   <- WHERE THE GUARD GOES
```

Every path that accepts a metadata number reaches one of two handlers, and both already have a
validation seam. Nothing else is in scope.

## The evidence

`[SPEC:reference/fixtures/27-malformed-request-handling.txt]` — the envelope every rejected request
must produce: **400, `application/json`, the full envelope, exactly one `errors[]` entry.** This
contract adds a new rejection, so it inherits that shape exactly and invents no new one.

Measured on our own build, 2026-08-17, `amkd --role api` on a throwaway database:

```
PATCH /v0/inboxes/{id}  {"metadata":{"a":1.7976931348623157e+308}}  -> 500 InternalError
server log: error occurred while decoding column "metadata": number out of range
```

Bisected with a **fresh inbox per case**, because the first bisect was invalid — one shared inbox
meant case 00's residue poisoned every later case and all ten appeared to fail:

```
{"a": 2.0974644638236597e-254}   200      {"a": null}  (delete)        200
{"a": -10000000.0}               200      {"": 1}      (empty key)     200
{"a": true}                      200      {"": true}       200
{"􁳨": false}          200      {"§ö": true}       200
{"a": 1.7976931348623157e+308}   500  <-- the only failure
```

### Root cause: the write path and the read path disagree about one number

1. `MetadataValue::Number(f64)` accepts `1.7976931348623157e308` — serde_json parses the exponent
   form without complaint.
2. Postgres `jsonb` normalises it to `numeric` and renders it back **with no exponent**: the digits
   `17976931348623157` followed by 292 zeros, a 309-digit integer literal. Verified directly:
   `select '{"a": 1.7976931348623157e+308}'::jsonb`.
3. Reading the row, serde_json parses that integer literal through its long-integer path and fails
   with `number out of range` — **even though the value is below `f64::MAX`**.

Measured, because the boundary is not where reasoning puts it:

| literal | parses as f64? |
|---|---|
| `1.7976931348623157e308` (exponent form, what the client sends) | ok |
| `1` + 308 zeros (`1e308` expanded) | ok |
| `17976931348623157` + 292 zeros (**what jsonb emits**) | ERR number out of range |
| `1` + 309 zeros | ERR number out of range |

serde_json's long-integer path is stricter than its float path, and jsonb's normalisation is
precisely what moves the value from one to the other.

### Severity: data corruption, not a transient 500

The row is written and then cannot be read. Every later `GET`, `PATCH` or list touching that inbox
hits the same decode and 500s — observed. One accepted request permanently removes an inbox from
the API.

## What to build

A **write-boundary guard**: a metadata number is accepted only if it survives the round-trip.

`MetadataValue::survives_storage_round_trip(&self) -> bool` in `crates/amk-types/src/inbox.rs`.
For `Number(v)`:

1. reject any non-finite `v` (defence in depth — JSON cannot carry NaN/Inf, so serde rejects it
   first, but the guard must not depend on that);
2. render `v` the way jsonb will — take Rust's shortest-round-trip `{}` form and expand any
   exponent into plain decimal notation, which is exactly what `numeric` output does;
3. accept iff `serde_json::from_str::<f64>` parses that rendering back.

**Do not hard-code a threshold.** The failing boundary is a property of serde_json's long-integer
parser, not a round number; a constant would be wrong the day either side changes. Deriving it by
construction is the point of this design, and the test table above is what pins it.

`String` and `Bool` always round-trip and return `true` without ceremony.

## Writable paths

- `crates/amk-types/src/inbox.rs` — the guard and its unit tests.
- `crates/amk-http/src/handlers/inboxes.rs` — call it from create and from `validate_update`.
- `crates/amk-http/tests/inboxes.rs` — the integration edge cases below.

Nothing else. In particular **not** `crates/amk-store/**` — the store is where the 500 surfaces but
not where the defect is; a value that never reaches storage cannot corrupt a row, and adding a
second guard there would be two representations of one rule (the `ApiKeyPermissions` collision
that the plan already records). Not `scripts/**`, not `reference/fixtures/**`, not the plan.

## The one decision that is `[INFERRED]`, and why

**No fixture covers what the reference does with an out-of-range metadata number**, and
`conformance/manifest.json` is 18 GETs plus one DELETE probe, so no existing capture can answer it.
The user was shown this and directed the work to continue, so the choice is recorded here rather
than left open:

- **Status and envelope: 400 `validation_error`**, matching fixture 27's shape for every other
  refused input. Not 500, which is what it does today and is wrong under any reading.
- **Issue kind: `custom`**, with `path: ["metadata", "<key>"]`. `custom` is chosen precisely
  because it claims nothing about the schema — every other kind in fixture 27 §3(a) carries
  kind-specific extras (`expected`, `minimum`, `values`, `format`) that would assert a vocabulary
  the reference has never shown us for this case. This deviates from the extractor contract's
  "custom is for whole-body rules only", and that deviation is deliberate: that rule was written to
  describe observed cases, and this case is unobserved.
- **`path` is two segments**, naming the offending key inside `metadata`. Fixture 27 §3(b) makes
  `path` a field path and the reference is deliberately a schema oracle; a one-segment
  `["metadata"]` would hide which key the caller must fix.

**One live request settles all three** and should replace this block when the key is available:
`PATCH /v0/inboxes/{id}` with `{"metadata":{"a":1.7976931348623157e+308}}` against
api.agentmail.to. Until then every one of the three bullets is `[INFERRED]` and must be marked so
in the code.

## Assigned edge cases

Unit, in `amk-types` — the table above is the specification:

1. `1.7976931348623157e308` (f64::MAX's shortest form) is **refused**.
2. `1e308` is **accepted** — this is the case that proves the guard is not a blunt magnitude cap.
3. `1e307`, `-1e307`, `0.0`, `-0.0`, `2.0974644638236597e-254` are accepted.
4. `f64::MAX` and `f64::MIN` are refused; the negative case must be tested, not assumed from the
   positive one.
5. `String` and `Bool` are always accepted, including a string that looks like a huge number.

Integration, in `amk-http` — each asserts **status, content-type and the whole `errors[0]`**:

6. `POST /v0/inboxes` with an out-of-range metadata number -> 400, and **no inbox is created**.
7. `PATCH /v0/inboxes/{id}` with one -> 400, and the inbox is **still readable afterwards** — this
   is the assertion that actually encodes the bug, so it must fail if the guard is removed.
8. A metadata object mixing one good key and one bad key -> 400, `path` names the **bad** key.
9. The exact schemathesis payload from the crash report -> 400, not 500.
10. Boundary and one unit either side, per the plan's testing rules: `1e308` accepted,
    `1.7976931348623157e308` refused.

## Prohibitions

- No new dependency, and **no serde_json feature flags** — `arbitrary_precision` would change how
  every number in this workspace serialises, to fix one input.
- No change to `MetadataValue`'s variants or its wire shape. It stays `String | Number | Bool`,
  untagged.
- No `mail_parser::` / `mail_auth::` / `mail_send::` / `smtp_proto::` types; no Stalwart or JMAP
  concept, field or name.
- Do not "fix" this in `amk-store` by making the decode lossy or by clamping on read — a value
  silently changed between write and read is worse than the 500 it replaces.
- Do not widen the guard into a general metadata size/shape policy. Numbers that cannot round-trip,
  and nothing else.
- If the contract is ambiguous or appears wrong, **STOP and report**. Do not resolve it yourself.

## Reporting

Report the command run and its actual output; "tests pass" without the output is not a report.

- `cargo test -p amk-types -p amk-http` and `./scripts/check.sh`.
- The minimised repro replayed against the new build, showing 400 where it showed 500, **and** a
  follow-up `GET` of the same inbox returning 200 — the corruption half, not just the status half.
- The schemathesis conjunct re-run: `not_a_server_error` clean on `PATCH /v0/inboxes/{inbox_id}`.
- A mutation pass in **both directions** on a scratch copy outside the tree: delete the guard (must
  kill a test) and widen it to refuse everything (must also kill a test — the accept-path tests are
  what pin it in the direction that breaks real traffic). Delete the scratch copy and confirm it.
