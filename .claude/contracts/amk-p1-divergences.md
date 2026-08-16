# P1 conformance divergences — dispatch contract

Scope-derivation: `scripts/derive-p1-divergences.sh`, which enumerates (1) the `Organization`
fields `amk-types` now emits, the single constructor outside that crate, and the table as it
stands, (2) every place the error envelope is built and rendered, (3) **every typed path extractor
that can reject before a handler runs**, (4) every place an `ApiKey` response is built plus the
scope columns and their `CHECK`, and (5) the existing tests that pin behaviour these changes alter.
Its raw output is pasted below and is the scope. **A reviewer re-runs the script; it does not read
the list.**

Section 3 is why this is derived rather than recalled: the divergence was observed on *one* route,
and the enumeration found **17 `Path<Uuid>` extraction sites**, each able to reject the same way.

## Derivation output (verbatim)

```
== 1. the Organization fields amk-types now emits, and where a value must come from ==
  field: organization_id
  field: inbox_count
  field: domain_count
  field: name
  field: inbox_limit
  field: domain_limit
  field: daily_send_limit
  field: five_minute_send_limit
  field: first_day_recipient_limit
  field: first_week_recipient_limit
  field: tracking_allowed
  field: authentication_id
  field: authentication_type
  field: billing_id
  field: billing_type
  field: billing_subscription_id
  field: updated_at
  field: created_at
  --- the only constructor outside amk-types ---
  organizations.rs:41:    Ok(Organization {
  --- the organizations table as it stands ---
  CREATE TABLE organizations (
      organization_id TEXT PRIMARY KEY,
      inbox_limit BIGINT,
      domain_limit BIGINT,
      created_at TIMESTAMPTZ(3) NOT NULL DEFAULT now(),
      updated_at TIMESTAMPTZ(3) NOT NULL DEFAULT now()
  );

== 2. every place the error envelope is built (the 'fix' field lands in all of them) ==
9://! 2. **Application layer** → the full envelope `{name, code, message, fix?, docs?}`, plus
81:    pub docs: Option<String>,
95:            docs: Some(code.docs_url()),
123:/// The documented code catalog (docs.agentmail.to/errors), with statuses corrected where the
124:/// live API disagreed with the docs — see the note on [`ErrorCode::AlreadyExists`].
140:    /// The docs' `resource_taken`/409 and the SDK-derived 422 were both wrong
155:    /// **Observed at HTTP 409**, not the 403 the docs imply
158:    /// original 403 came from the docs page rather than the spec, and the live capture beats both.
244:        format!("https://docs.agentmail.to/errors#{}", self.as_str())
269:            "docs":"https://docs.agentmail.to/errors#not_found"}"#;
282:            "docs":"https://docs.agentmail.to/errors#already_exists"}"#;
296:            "docs":"https://docs.agentmail.to/errors#limit_exceeded"}"#;
319:            "fix":"...","docs":"https://docs.agentmail.to/errors#validation_error"}"#;
329:        // Check for the KEYS, not substrings: the docs URL legitimately contains "errors".
335:        assert_eq!(v.keys().collect::<Vec<_>>(), ["code", "docs", "message", "name"]);
  --- and the http layer's rendering of it ---
1://! The two error shapes, wired to axum's `IntoResponse`.
11:use amk_types::{ErrorCode, ErrorEnvelope, GatewayError};
13:use axum::response::{IntoResponse, Response};
33:impl IntoResponse for GatewayFailure {
42:/// Boxed rather than inline: `ErrorEnvelope` carries several `String`/`Vec` fields (clippy's
46:pub struct AppError(pub Box<ErrorEnvelope>);
50:        Self(Box::new(ErrorEnvelope::new(code, message)))
59:impl IntoResponse for AppError {
67:impl From<ErrorEnvelope> for AppError {
68:    fn from(e: ErrorEnvelope) -> Self {
81:        Self(Box::new(ErrorEnvelope::new(d.code(), d.to_string())))

== 3. every typed path extractor that can reject before reaching a handler ==
  handlers/api_keys.rs:54:    Path(pod_id): Path<Uuid>,
  handlers/api_keys.rs:66:    Path(raw_inbox_id): Path<String>,
  handlers/api_keys.rs:113:    Path(pod_id): Path<Uuid>,
  handlers/api_keys.rs:124:    Path(raw_inbox_id): Path<String>,
  handlers/api_keys.rs:171:    Path(raw_api_key_id): Path<String>,
  handlers/api_keys.rs:180:    Path((pod_id, raw_api_key_id)): Path<(Uuid, String)>,
  handlers/api_keys.rs:189:    Path((raw_inbox_id, raw_api_key_id)): Path<(String, String)>,
  handlers/inboxes.rs:73:    Path(pod_id): Path<Uuid>,
  handlers/inboxes.rs:152:    Path(pod_id): Path<Uuid>,
  handlers/inboxes.rs:282:    Path(raw_inbox_id): Path<String>,
  handlers/inboxes.rs:292:    Path((pod_id, raw_inbox_id)): Path<(Uuid, String)>,
  handlers/inboxes.rs:334:    Path(raw_inbox_id): Path<String>,
  handlers/inboxes.rs:345:    Path((pod_id, raw_inbox_id)): Path<(Uuid, String)>,
  handlers/inboxes.rs:408:    Path(raw_inbox_id): Path<String>,
  handlers/inboxes.rs:418:    Path((pod_id, raw_inbox_id)): Path<(Uuid, String)>,
  handlers/pods.rs:119:    Path(pod_id): Path<Uuid>,
  handlers/pods.rs:148:    Path(pod_id): Path<Uuid>,
  ids.rs:3://! `pod_id` is not here: it is a UUID and is extracted with axum's own `Path<Uuid>` (the
  ids.rs:11://! does not hand a handler that: both `Path<String>` and `RawPathParams` (checked against the
  ids.rs:56:    /// Simulates exactly what axum's `Path<String>` does to a route segment: one percent-decode.
  --- the router's existing method/fallback handling, which this must match ---
  104:        .fallback(not_found_fallback)
  107:        .method_not_allowed_fallback(not_found_fallback)
  116:async fn not_found_fallback() -> AppError {

== 4. every place an ApiKey response is built (pod_id must appear on inbox-scoped keys) ==
134:pub struct NewApiKey {
239:    Ok(ApiKey {
  --- the api_keys scope columns and their CHECK ---
  5:-- Scope is derived from which of pod_id/inbox_id is set, not stored as a separate enum column
  6:-- (dispatch contract): both null is an organization-scoped key, pod_id alone is pod-scoped,
  7:-- inbox_id alone is inbox-scoped. The two are never set together — a row naming both has no
  10:-- not stored redundantly here) — so the CHECK below rejects that combination at the database,
  24:    pod_id UUID REFERENCES pods (pod_id),
  25:    inbox_id TEXT REFERENCES inboxes (inbox_id),
  33:        CHECK (NOT (pod_id IS NOT NULL AND inbox_id IS NOT NULL))
  40:CREATE INDEX api_keys_pod_id_idx ON api_keys (pod_id);
  41:CREATE INDEX api_keys_inbox_id_idx ON api_keys (inbox_id);
  --- the wire type's scope fields ---
  pub struct ApiKey {
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub organization_id: Option<OrganizationId>,
      pub api_key_id: ApiKeyId,
      pub prefix: String,
      pub name: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub pod_id: Option<PodId>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub inbox_id: Option<InboxId>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub used_at: Option<Timestamp>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub permissions: Option<ApiKeyPermissions>,
      pub created_at: Timestamp,

== 5. tests that pin any behaviour these four changes alter ==
  crates/amk-store/tests/api_keys.rs
  crates/amk-store/tests/control_plane.rs
  crates/amk-store/tests/messages_and_threads.rs
  crates/amk-http/tests/auth.rs
  crates/amk-http/tests/not_found.rs
  crates/amk-http/tests/scope.rs
2://! full envelope, `code: "not_found"`, HTTP 404. There is no 405 anywhere in this crate.
13:    // No credential at all — the fallback must not require auth to say a route doesn't exist.
19:    assert_eq!(v["code"], "not_found");
24:async fn a_matched_path_with_the_wrong_method_is_404_never_405() {
34:    assert_eq!(resp.status, 404, "must never be 405: body: {}", resp.body);
35:    assert_eq!(resp.code(), Some("not_found"));
39:    assert_eq!(resp.status, 404, "must never be 405: body: {}", resp.body);
40:    assert_eq!(resp.code(), Some("not_found"));
```

## Where this came from

Not from reading. `reference/fixtures/25-p1-gate-conformance.txt` is an executed dual-target
conformance run — `api.agentmail.to` against a throwaway localhost deployment — and these four are
what survived after two harness defects and one state-parity artifact were removed from its output.
`./scripts/p1-gate.sh` reproduces it end to end.

**The P1 gate (`p1-gate-conformance`) is what closes this dispatch**, and like P0's it is asserted
by evidence: re-run `scripts/p1-gate.sh` and the diff must report `0 skipped, 0 with structural
diffs`. Append that run's verbatim output to fixture 25 under a `SECOND RUN — AFTER THE FIX`
heading. Writing code alone does not flip the ledger line.

## `[SPEC:*]` and `[TESTED]` citations

- `[TESTED]` `reference/fixtures/25-p1-gate-conformance.txt` — all four divergences, with the exact
  key sets observed on each side.
- `[SPEC:openapi]` `type_organizations:Organization` — 12 documented properties. The live response
  carries 17. **The live capture wins**; this project has been here before (fixture 19's system
  labels; `openapi.json` 0-for-3 on DELETE statuses).
- `[SPEC:docs errors]` + plan register B1 — the envelope is `{name, code, message, fix?, docs?}`.
- `[TESTED]` fixture 05 — auth-layer failures return a **bare** `{"message":…}`; app errors get the
  full envelope. That asymmetry is already built and must not be disturbed.

`amk-types` is **frozen**. It already carries every field this dispatch needs (commit `31f3591`).
If something you need is not there, **STOP and report** — do not add it.

## Writable paths (exact)

`crates/amk-store/**`, `crates/amk-http/**`, `crates/amk-cli/**`,
`reference/fixtures/25-p1-gate-conformance.txt` (append only — never rewrite the first run),
and `scripts/p1-gate.sh`. Nothing else. If the work requires a path outside those, **STOP and
report**.

## Divergence 1 — `GET /v0/organizations` emits 5 of 17 fields

`amk-types::Organization` already has the fields; nothing gives them a value. `organizations.rs`
sets all ten to `None` with a comment saying this dispatch owns them.

- Migration `0009`: add `name TEXT`, `daily_send_limit BIGINT`, `five_minute_send_limit BIGINT`,
  `first_day_recipient_limit BIGINT`, `first_week_recipient_limit BIGINT`, `tracking_allowed
  BOOLEAN`, `authentication_id TEXT`, `authentication_type TEXT`. All nullable, no defaults.
- Hydrate them in `organizations::hydrate_row` / the constructor the derivation names.
- `NewOrganization` gains `name: Option<String>` only. **The limits are not settable at creation**
  — there is no endpoint that sets them and inventing one is out of scope. They are operator
  configuration, reachable today only by a direct `UPDATE`, and that is the honest state.
- `amk init` sets `name` from `AMK_PRODUCT_NAME` when set, otherwise leaves it `None`.

**Absent stays omitted.** A deployment that configures no limits emits no limit fields — never
`null`, never `0`. `0` would mean "send nothing", which is the opposite of "unlimited", so a
default here is a live outage waiting to happen.

Do **not** add `billing_plan_id` or `clerk_organization_id`. They are excluded by decision, the
exclusion is pinned by a test in `amk-types`, and re-adding them fails that test.

## Divergence 2 — the error envelope omits `fix`

`ErrorEnvelope` has the field and `ErrorCode` already knows its own `docs_url()`. Give each code a
`fix` string the same way, and emit it. The reference's `fix` is human guidance ("No route matches
this path and HTTP method. …"), so ours should be equally actionable — one sentence naming what to
do, not a restatement of the code.

Fixture 05 carries real `fix` strings for several codes; use them verbatim where present, and write
one in the same register where not. **The gateway (401/403) body stays bare** — `fix` must never
appear there, and fixture 05's test already pins that.

## Divergence 3 — a malformed path segment escapes the error contract

```
GET /v0/pods/not-a-uuid   ref=404 application/json   cand=400 text/plain
```

axum's `Path<Uuid>` rejection is reaching the client directly from a server whose entire error
contract is a JSON envelope. `GET /v0/pods/<well-formed-but-absent uuid>` **passes**, so the
handler is correct — the extractor is what escapes, and no test caught it because every existing
test reaches the handler.

Match the reference: **404 with the full `not_found` envelope**, exactly what the router's
`not_found_fallback` already produces for an unknown path. A malformed id and an absent id are
indistinguishable to a client, which is also the right disclosure answer — it reveals nothing about
which ids are well-formed.

The derivation lists 17 extraction sites. Fix this **once**, centrally — a per-handler fix is 17
chances to miss one, and the 18th site added later would be wrong by default. A custom rejection
handler or an extractor wrapper are both acceptable; choose one and say why.

Assign a test to *every* route shape in the derivation's section 3, not to `/v0/pods/{pod_id}`
alone. That is the whole reason the scope was enumerated.

## Divergence 4 — an inbox-scoped api key must also carry `pod_id`

Observed live: one key returns `organization_id`, `pod_id` **and** `inbox_id` together.

This does **not** contradict the `CHECK` at `0007_api_keys.sql:33`, and the constraint stays.
`inbox_id` alone is still the *scope*; the emitted `pod_id` is the **containing pod** — the same
denormalised provenance `organization_id` already is on every object. Derive it from the inbox at
read time; do not add a column and do not relax the `CHECK`.

Every `ApiKey` response for an inbox-scoped key gains `pod_id`. Org-scoped and pod-scoped responses
are unchanged.

## Assigned edge cases (write the test before the code it targets)

- Each of the eight new organization columns: set → emitted; unset → **absent from the JSON**,
  not `null` and not `0`.
- `inbox_limit`/`domain_limit` unchanged in behaviour, still omitted when unset.
- `amk init` with `AMK_PRODUCT_NAME` set → `name` emitted; unset → `name` absent.
- `fix` present on an app-layer error; `fix` **absent** from a 401 and a 403 gateway body.
- A malformed id at **every** route shape from the derivation's section 3 → 404, `application/json`,
  `code: "not_found"`, full envelope. Include the two-segment routes (`Path<(Uuid, String)>`),
  where only the first segment is malformed and where only the second is.
- A malformed id must be indistinguishable from an absent one: same status, same body shape.
- An inbox-scoped key: response carries `pod_id` **and** `inbox_id`, and the `pod_id` is the pod
  that actually contains the inbox — assert against a second pod's id to prove it is not constant.
- A pod-scoped key still carries `pod_id` and **no** `inbox_id`; an org-scoped key carries neither.
- The `CHECK` still rejects a row naming both — pin it, since this dispatch is the obvious moment
  for someone to "helpfully" relax it.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type. No JMAP,
  Sieve, RocksDB, or mailbox-role concept.
- **No edits to `amk-types`.** It is frozen and already complete for this work.
- No `billing_plan_id`, no `clerk_organization_id`, no billing surface of any kind.
- Do not relax or drop the `api_keys` scope `CHECK`.
- No new dependency. If you believe you need one, **STOP and report**.
- Do not rewrite fixture 25's first run — append the second.
- Do not edit the plan, other contracts, or `scripts/hooks/**`.
- Do not commit `.amk-task.md`, `.amk-scope` or `.amk-brief.md`.

## Reporting

Report the command you ran and its actual output: `cargo test --workspace`, `./scripts/check.sh`,
and a **two-directional** mutation table — every guard deleted (must kill a test) *and* widened
(must also kill a test). Mutate on a **private scratch copy** under the session scratchpad, never
in the worktree, and **delete the copy when the pass ends and say that you did** — seven abandoned
copies filled `/tmp` and took every agent's shell down with them.

Then **run `./scripts/p1-gate.sh` and report its real output.** `0 skipped, 0 with structural
diffs` is the gate. If it still diffs, report the diff — a failing gate reported honestly is the
deliverable; a passing one claimed without its output is not.
