# amk-http — dispatch contract

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

**Do not start until `amk-store` is merged and `./scripts/check.sh` is green on `main`.**

## What this crate is

The axum HTTP surface: the tower auth layer, scope resolution into handlers, the error envelope,
pagination parameter parsing, and the P0/P1 handlers. It depends on `amk-types`, `amk-core` and
`amk-store`. It is the crate the official SDKs actually talk to, so **its job is byte-level
fidelity to the reference API, not elegance**.

## The rule that governs every other decision

A response that is structurally different from `api.agentmail.to`'s is a defect, even when it is
better. The conformance harness (`conformance/dual_target.py`) diffs our responses against the live
API and gates the phase. Where this contract and a fixture disagree, **the fixture wins and you
report the contradiction**.

## Decisions (settled — implement, do not relitigate)

### Errors — the asymmetry is real and observed

Two shapes, and the branch is on **who rejected the request**, not on status code
(`reference/fixtures/05-error-catalog.http`):

- **Auth-layer failures return a bare body**: `{"message":"Unauthorized"}` at 401,
  `{"message":"Forbidden"}` at 403 — no `name`, no `code`, no `fix`, no `docs`. This holds even for
  a **well-formed but unknown** `am_` key, which is the case that looked like it should return an
  envelope and does not.
- **Application failures return the full envelope**: `{name, code, message, fix?, docs?}`.
- **Per-code extras are real and are not a fixed set**: `validation_error` carries
  `errors[]` of `{code, path[], message}` (path is a JSON-pointer array with **mixed string and
  integer members**, e.g. `["add_labels", 0]`); `already_exists` carries `suggestions[]`;
  `limit_exceeded` carries `resource`, `limit`, `upgrade_url`. Model them per code.
- **`upgrade_url` is deliberately omitted** and no plan or price appears in any `fix` string. This
  is the no-billing-surface rule applied on purpose — record it, do not "fix" it.
- **Unknown path or wrong method → 404 with the full envelope, `code: "not_found"`. There is no
  405.** Configure axum's fallback accordingly; the default method-not-allowed response is wrong.
- Clients branch on `code`; `name` and `message` deliberately keep legacy values (a permission
  denial still reads `Forbidden`). Do not tidy them into consistency.

### Auth

- `Authorization: Bearer <key>`, deny-by-default: a route is unreachable without a resolved
  credential unless it is explicitly public.
- The layer resolves a **`Credential` enum**, not a raw key — one variant today (`ApiKey`).
  Handlers receive the resolved principal and scope and **never see the credential itself**. This
  is a type-shape decision only: no session tokens, no JWT handling, no console surface in V1.
- Key verification is O(1) lookup by key id then a **constant-time** verify of an argon2id hash.
  Never scan the key table.
- Scope resolution runs **before** the handler, via `amk_core::scope`. A handler that re-derives
  scope is a defect.
- **Scope and label denial mask as `not_found`, never `forbidden`** — a pod-scoped key reaching
  another pod's inbox must not learn that it exists.

### Routing

- axum 0.8: route parameter syntax is `{id}`, **not** `:id`. `features = ["ws"]` is required for
  the WebSocket upgrade (P4, not this dispatch).
- **Three mounts share one handler set**: org-level, `pods/{pod_id}/…`, `inboxes/{inbox_id}/…`.
  Write the handler once and mount it three times with a different scope extractor; do not fork the
  logic per mount.
- **Path ids are percent-encoded and must round-trip.** `inbox_id` is an email address;
  `message_id` is an RFC 5322 angle-bracket value containing `<`, `>`, `@`. Decode with
  `amk_types`' `from_path_segment`; never hand-roll it. `inbox_id` compares **ASCII-case-folded**
  (`reference/fixtures/18-inbox-case-normalization.txt`) — `AMKCASE@…` resolves the same inbox as
  `amkcase@…`.

### Serialization

- **Optionals are omitted when absent — never `null`, never `""`.** `skip_serializing_if` on every
  optional field. This is the single most likely source of a conformance diff.
- Timestamps are RFC 3339 with **exactly three** fractional digits and `Z`. `amk_types::Timestamp`
  already guarantees this; do not format dates by hand anywhere in this crate.
- Emit `organization_id` and `pod_id` (and `smtp_id` on messages) even though the SDK types omit
  them — the live API sends them and the conformance diff fails without them.

### Pagination and list parameters

- Envelope is `{count, limit?, next_page_token?, <resource>: []}`. **`next_page_token` is absent on
  the last page** — never an empty string.
- Parameters: `before`/`after`/`ascending`, `labels[]`, the `include_*` visibility flags
  (default false), and the substring filters, which AND together. Filtered-list `limit` caps at 100.
- The `include_*` flags exist on **4 of the 33** paginated GETs (`/threads`, `/pods/{id}/threads`,
  `/inboxes/{id}/threads`, `/inboxes/{id}/messages`). Build the `LabelAccess` **mode** from the
  route, not from a global default: `Mode::List(flags)` on those four, `Mode::Search` on the search
  endpoints, `Mode::ById` on get-by-id. Routing a search or a drafts list through the list rule
  makes restricted mail permanently unreachable for every credential that will ever exist
  (`reference/fixtures/20-search-and-label-precedence.txt`).
- **Never post-filter a page.** Build the `LabelAccess` and hand it to `amk-store`, which pushes the
  exclusion into the query. A `count` computed after filtering leaks the hidden rows.

## Scope of the FIRST dispatch (a second one follows)

**In:** the tower auth layer and `Credential`, scope extraction for all three mounts, the error
envelope with per-code extras and the auth/app asymmetry, the 404 fallback, pagination parameter
parsing, and handlers for `auth/me`, organizations, pods, inboxes, api-keys, including
`client_id` idempotent creates and the inbox-collision path.

**Out (second dispatch, named so the omission is a decision):** messages/threads/drafts handlers,
attachment and raw downloads, the `Idempotency-Key` layer, WebSockets, rate limiting, `/metrics`.

## Assigned edge cases (write the test before the code it targets)

- `message_id` round-trip through encode → route → decode with `+`, `%`, `/`, `?`, `#`, space and
  non-ASCII in the local part; a literal encoded `%2F`; double-encoding; an over-long segment.
- `inbox_id` in a path with plus-addressing and mixed case.
- A page token that is tampered, truncated, invalid base64, **from a different scope**, or from a
  deleted resource.
- Pod-scoped key reaching another pod's inbox → `not_found`, not `forbidden`. Cross-org attempts at
  all three mounts.
- Creating a key with a permission the parent lacks → `permission_escalation`; child ⊄ parent at
  every level.
- Inbox username collision → `already_exists` at **HTTP 403** with `suggestions[]` — not 409, not
  422 (`reference/fixtures/05-error-catalog.http`).
- A bare-body 401/403 asserted as **bare**: a test that fails if `code` or `name` appears.
- Unknown path and wrong method both → 404 envelope.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export.
- No JMAP, Sieve, RocksDB, or mailbox-role concept. (A hook blocks this at write time.)
- No SQL in this crate. All persistence goes through `amk-store`'s public interface.
- No billing surface: no plan, price, quota-upsell string, or `upgrade_url`.
- Do not edit `amk-types`, `amk-core`, `amk-store`, or the plan. If a type or field you need does
  not exist, **STOP and report** — do not add a field that obviously belongs.

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-http`, `./scripts/check.sh`,
and the mutation table required at every phase gate. "Tests pass" without the output is not a
report. Name anything you did not do and why.
