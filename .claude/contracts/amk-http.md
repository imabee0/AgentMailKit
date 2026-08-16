# amk-http — dispatch contract

Scope-derivation: `python3` over `reference/openapi.json` — 130 operations partitioned across
dispatches, sum asserted; command and output in "Scope — derived by enumeration, not recall".

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

**Do not start until `amk-store` is merged and `./scripts/check.sh` is green on `main`.**

## What this crate is

The axum HTTP surface: the tower auth layer, scope resolution into handlers, the error envelope,
pagination parameter parsing, and the P0/P1 handlers. It depends on `amk-types`, `amk-core` and
`amk-store`. It is the crate the official SDKs actually talk to, so **its job is byte-level
fidelity to the reference API, not elegance**.

## Writable paths (exact)

`crates/amk-http/**`, plus the workspace `Cargo.lock` **only** as the automatic consequence of
adding a dependency this contract sanctions. Nothing else. Same rule and same hook as every other
dispatch: if the work requires a path outside that tree, **STOP and report** rather than widening
scope.

`Cargo.lock` is named explicitly because the api-keys dispatch proved the omission matters. Its
contract said `crates/amk-store/**` and nothing else, adding a dependency necessarily rewrote the
root lockfile, and **the scope hook never saw it** — the guard is a `PreToolUse` hook on
`Write`/`Edit`/`Bash`, so it observes what an agent writes and is structurally blind to what cargo
writes. That left the implementer choosing between violating its stated scope and being unable to
do what the contract asked. Committing lockfiles is a project rule; so the contract names it.

## `[SPEC:*]` citations governing every shape here

- `[TESTED]` `reference/fixtures/05-error-catalog.http` — the auth/app error asymmetry (bare
  `{"message":…}` at 401/403 **even for a well-formed but unknown key**; full envelope for app
  errors), and inbox collision as `already_exists` at **HTTP 403** with `suggestions[]`.
- `[TESTED]` `reference/fixtures/01-auth-me.http` — the Identity shape returned by `auth/me`, which
  is the P0 gate's subject.
- `[TESTED]` `reference/fixtures/04-pagination.http` — envelope `{count, limit?, next_page_token?,
  <resource>: []}`; token absent on the last page.
- `[TESTED]` `reference/fixtures/18-inbox-case-normalization.txt` — case-folded `inbox_id` in a
  path segment; `limit_exceeded` extras (`resource`, `limit`; `upgrade_url` deliberately omitted —
  no billing surface).
- `[TESTED]` `reference/fixtures/20-search-and-label-precedence.txt` — the `LabelAccess` mode is
  chosen by route, not by a global default. The `include_*` flags exist on **4 of 33** paginated
  GETs.
- `[TESTED]` `reference/fixtures/03-id-formats.http` — percent-encoded angle-bracket `message_id`
  in a path segment; live responses carry `organization_id`/`pod_id`/`smtp_id`.
- `[SPEC:openapi]` — 82 paths / 242 schemas, and the three mounts (org, `pods/{pod_id}`,
  `inboxes/{inbox_id}`) sharing one handler set.
- `[SPEC:sdk]` — 128 endpoints; the SDKs are the acceptance test, so their expectations are the
  contract where the spec is silent.

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
- The `include_*` flags exist on **4 of the 33** paginated GETs. Build the `LabelAccess` **mode**
  from the route, not from a global default: `Mode::List(flags)` on those four, `Mode::Search` on
  the search endpoints, `Mode::ById` on get-by-id. Routing a search or a drafts list through the
  list rule makes restricted mail permanently unreachable for every credential that will ever
  exist (`reference/fixtures/20-search-and-label-precedence.txt`).

  This is the most security-relevant count in the contract, so it is **generated, not recalled** —
  re-run this rather than trusting the number:

  ```bash
  python3 - <<'PY'
  import json
  s = json.load(open('reference/openapi.json'))
  paged, incl = [], []
  for p, d in s['paths'].items():
      op = d.get('get')
      if not op: continue
      names = {q.get('name') for q in op.get('parameters', [])}
      if 'page_token' in names or 'limit' in names: paged.append(p)
      if any(n.startswith('include_') for n in names): incl.append(p)
  print(len(paged), 'paginated GETs;', len(incl), 'carry include_*:', sorted(incl))
  PY
  ```

  Verified output: **33 paginated GETs; 4 carry `include_*`** — `/v0/threads`,
  `/v0/pods/{pod_id}/threads`, `/v0/inboxes/{inbox_id}/threads`,
  `/v0/inboxes/{inbox_id}/messages`. The flag set on each is exactly
  `include_blocked`, `include_spam`, `include_trash`, `include_unauthenticated` — four flags, one
  per restricted label, matching `amk_types::labels::RESTRICTED`. None of these four is in this
  dispatch's 25 operations; they land in the second dispatch, and this is pinned here so the mode
  decision is made from evidence when it gets there.
- **Never post-filter a page.** Build the `LabelAccess` and hand it to `amk-store`, which pushes the
  exclusion into the query. A `count` computed after filtering leaks the hidden rows.

## Scope — derived by enumeration, not recall

This section was rewritten after the `amk-store` id-safety dispatch cost four correction rounds,
every one of them because that contract's list of affected code was written from a review report
instead of from the codebase. A scope written from memory is a scope with holes in it, and the
holes are invisible until an implementer walks into one. So: this scope is **generated**, the
command that generates it is below, and a reviewer's job is to re-run it rather than read it.

```bash
python3 - <<'PY'
import json, collections
s = json.load(open('reference/openapi.json'))
ops = [(p, m.upper()) for p, d in s['paths'].items()
       for m in d if m in ('get','post','put','patch','delete')]
print(len(ops), 'operations across', len(s['paths']), 'paths')
PY
# → 130 operations across 82 paths
```

**Every one of those 130 is assigned below. The partition is total and sums to 130 — that
totality is the property to check, not the individual rows.** An operation appearing in no bucket
is the defect this table exists to make impossible.

| Count | Bucket |
|---|---|
| **25** | **this dispatch** — identity + org (2), pods (4), inboxes (10), api-keys (9) |
| 12 | second dispatch — allow/block lists |
| 13 | second dispatch — drafts |
| 13 | second dispatch — messages |
| 18 | second dispatch — threads |
| 6 | second dispatch — metrics |
| 22 | P4 — webhooks + events |
| 14 | P5 — domains |
| 5 | **parked post-V1** — AgentID P-256 public keys (`/v0/api-keys/public-keys*`) |
| 2 | **parked** — `POST /v0/agent/sign-up`, `POST /v0/agent/verify` (agent signup + OTP; the plan parks these config-gated and off by default) |
| **130** | **total** |

### The 25 operations of this dispatch, exactly

| Method | Path |
|---|---|
| `GET` | `/v0/auth/me` |
| `GET` | `/v0/organizations` |
| `GET` | `/v0/pods` |
| `POST` | `/v0/pods` |
| `GET` | `/v0/pods/{pod_id}` |
| `DELETE` | `/v0/pods/{pod_id}` |
| `GET` | `/v0/inboxes` |
| `POST` | `/v0/inboxes` |
| `GET` | `/v0/inboxes/{inbox_id}` |
| `PATCH` | `/v0/inboxes/{inbox_id}` |
| `DELETE` | `/v0/inboxes/{inbox_id}` |
| `GET` | `/v0/pods/{pod_id}/inboxes` |
| `POST` | `/v0/pods/{pod_id}/inboxes` |
| `GET` | `/v0/pods/{pod_id}/inboxes/{inbox_id}` |
| `PATCH` | `/v0/pods/{pod_id}/inboxes/{inbox_id}` |
| `DELETE` | `/v0/pods/{pod_id}/inboxes/{inbox_id}` |
| `GET` | `/v0/api-keys` |
| `POST` | `/v0/api-keys` |
| `DELETE` | `/v0/api-keys/{api_key_id}` |
| `GET` | `/v0/pods/{pod_id}/api-keys` |
| `POST` | `/v0/pods/{pod_id}/api-keys` |
| `DELETE` | `/v0/pods/{pod_id}/api-keys/{api_key_id}` |
| `GET` | `/v0/inboxes/{inbox_id}/api-keys` |
| `POST` | `/v0/inboxes/{inbox_id}/api-keys` |
| `DELETE` | `/v0/inboxes/{inbox_id}/api-keys/{api_key_id}` |

Plus the cross-cutting machinery those 25 need: the tower auth layer and `Credential`, scope
extraction for all three mounts, the error envelope with per-code extras and the auth/app
asymmetry, the 404 fallback, pagination parameter parsing, `client_id` idempotent creates, and the
inbox-collision path.

**Three things the enumeration caught that the previous, recalled version of this section had
wrong** — recorded because they are the evidence that generating beats remembering:

- **`/v0/*/lists/{direction}/{type}` (12 operations) appeared in neither the In nor the Out list.**
  Allow/block lists were simply absent from this contract. They are second dispatch.
- **`/v0/*/metrics/usage` (6 operations) likewise appeared in neither.** The old Out list said
  "`/metrics`", which is the Prometheus scrape endpoint — a different thing entirely from these
  per-scope usage resources. Second dispatch.
- **`PATCH` on inboxes was never named**, at either mount, though "inboxes" was listed as In. It
  carries the metadata merge semantics (`key → null` deletes) and is in this dispatch.

`GET /v0/organizations` is the only organizations operation in the whole spec — there is no `POST`
and no `PATCH`. Do not build one.

## Settled by probe, because nothing else settled them

The pre-dispatch review of this contract found operations among the 25 that neither a fixture nor a
schema decided, which an implementer would have had to invent. They were probed live rather than
guessed — `reference/fixtures/22-org-mount-and-delete-semantics.txt`, throwaway resources, torn
down, end state re-verified. Two of the answers contradict `openapi.json`.

- **`POST /v0/inboxes` at the org mount resolves the pod whose `pod_id` equals the
  `organization_id`.** `type_inboxes:CreateInboxRequest` carries no `pod_id` and `Inbox.pod_id` is
  required, so the server picks one; three pods existed at the moment of the probe and it picked
  that one, not the newest. The account's "Default Pod" is minted carrying the organization's own
  UUID. The fixture names the one rival reading it cannot exclude — "the oldest pod", since Default
  Pod is also the oldest — and this contract implements id-equality: it is O(1), exact, and `amk
  init` mints the default pod that way, so both readings agree in our deployment forever. It also
  explains fixture 01's org-scoped `scope_id == organization_id`.
- **`DELETE /v0/pods/{pod_id}` on a pod that still owns an inbox → `409` `cannot_delete`**, full
  envelope, and the refusal is total: neither the pod nor the inbox is touched. `cannot_delete`
  appears **zero times** in `openapi.json`, and `amk-types` mapped it to 403 until the probe; that
  is corrected on `main` (commit `2318e9c`). Do not re-derive the status — use `ErrorCode::status()`.
- **`DELETE /v0/pods/{pod_id}` → `204`. `DELETE /v0/inboxes/{inbox_id}` → `202`.** `openapi.json`
  documents `200` for both and is wrong twice. The `202` is the consequential one: inbox deletion is
  accepted-then-processed, so a test that deletes an inbox and immediately asserts `404` on `GET` is
  racing the server — write it to tolerate the row still being visible.
- **`GET /v0/organizations` returns a bare `Organization` object, not a list envelope**, despite the
  plural path — it is "the organization for the authenticated API key". Every other plural path in
  these 25 (`/v0/pods`, `/v0/inboxes`, `/v0/api-keys`) *is* a list envelope, so the pattern-match is
  wrong here specifically. **Call `amk_store::organizations::get` with the resolved identity's
  `organization_id`. Never `organizations::list`** — that function takes no credential and returns
  every organization in the deployment; reaching for it here is a cross-tenant disclosure, not a
  shape bug.
- **`billing_id`, `billing_type` and `billing_subscription_id` are omitted deliberately.**
  `type_organizations:Organization` carries all three. This is the no-billing-surface rule, the same
  decision already applied to `upgrade_url`, and the conformance diff must be told rather than
  "fixed". `authentication_id`/`authentication_type` are likewise absent from `amk_types`; both are
  optional and their omission is wire-legal, recorded here for whoever revisits `amk-types`.

**`PATCH` on inboxes depends on work that is not merged yet.** `amk_store::inboxes` has no
`update`; `.claude/contracts/amk-store-inbox-update.md` adds it, and this dispatch does not start
until that is on `main`. The wire types already exist and are frozen —
`amk_types::inbox::UpdateInboxRequest` with `MetadataUpdate`'s three states (absent / `null` /
merge). **This crate owns the two validation rules the store deliberately does not**: an empty
`metadata` object is rejected, and each update must carry at least one of `display_name` or
`metadata`. Both produce `validation_error`; the store treats an empty merge as a no-op.

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
