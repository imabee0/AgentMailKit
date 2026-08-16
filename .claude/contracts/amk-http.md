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

`crates/amk-http/**`, the workspace `Cargo.lock` **only** as the automatic consequence of
adding a dependency this contract sanctions, and the root `Cargo.toml` **only** to add
`"crates/amk-http"` to `[workspace.members]` and the dependencies this contract sanctions to
`[workspace.dependencies]`. Nothing else. Same rule and same hook as every other dispatch: if the
work requires a path outside that tree, **STOP and report** rather than widening scope.

The root `Cargo.toml` is named because **this crate does not exist yet** — `crates/` holds
`amk-types`, `amk-core`, `amk-store` and nothing else, and `[workspace.members]` lists exactly
those three. Every prior dispatch edited a crate that was already a member, so no earlier contract
needed this and its omission here would have blocked the dispatch at its first command. Add the
member line and the dependency pins; change nothing else in that file.

`Cargo.lock` is named explicitly because the api-keys dispatch proved the omission matters. Its
contract said `crates/amk-store/**` and nothing else, adding a dependency necessarily rewrote the
root lockfile, and **the scope hook never saw it** — the guard is a `PreToolUse` hook on
`Write`/`Edit`/`Bash`, so it observes what an agent writes and is structurally blind to what cargo
writes. That left the implementer choosing between violating its stated scope and being unable to
do what the contract asked. Committing lockfiles is a project rule; so the contract names it.

## Dependencies — pinned here, not chosen by the implementer

Add exactly these to `[workspace.dependencies]` and depend on them from `crates/amk-http/Cargo.toml`.
Every version is `[TESTED]` — `reference/fixtures/15-compile-spike.txt` built against them, exit 0.

```toml
axum = { version = "=0.8.9", features = ["ws"] }
tower = "=0.5.3"
```

plus the workspace members and pins that already exist: `amk-types`, `amk-core`, `amk-store`,
`tokio`, `serde`, `serde_json`, `chrono`, `uuid`, `thiserror`, `base64`, `percent-encoding`.

- **`features = ["ws"]` is required**, even though the WebSocket upgrade is P4 and not in this
  dispatch — `axum::extract::ws::WebSocketUpgrade` does not exist without it, and the spike found
  that the hard way (F1).
- **`uuid` needs its `serde` feature for `Path<Uuid>`** or the handler fails with an opaque
  Handler-trait error that names neither `uuid` nor `serde` (spike F2). It is already enabled in
  the workspace pin; do not remove it.
- **`sqlx` too** — `sqlx.workspace = true`, reusing the pin `amk-store` already carries. `AppState`
  structurally cannot name a `PgPool` field without it. Added to this list after the dispatch
  flagged it rather than silently adding it, which was the right call: "no dependency beyond the
  two pinned" meant **no new third-party crate**, and a workspace-internal pin the store already
  depends on is not that. This crate still writes no SQL.
- **No other dependency.** Not `tower-http`, not `governor`, not `hyper` directly. Rate limiting is
  a later dispatch and `governor` arrives with it. If you believe you need a crate that is not
  listed, **STOP and report** — adding one is a decision, and it is not yours.

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
  **This type is yours and lives in this crate.** It is not in `amk-types` and must not be added
  there: it is an internal resolution step, never serialised, never on the wire, so it fails the
  test `amk-types` exists to apply. Rule 3 ("if a needed type is not in `amk-types` or a fixture,
  STOP and report") does not bite here, and this line exists so you do not stop to ask. Handlers
  receive the resolved principal and scope — **never the `Credential` itself**, and never the
  presented secret.
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
  a NUL check on axum's already-decoded value — **not** `amk_types`' `from_path_segment`. Corrected
  after the dispatch returned: `from_path_segment` percent-decodes (`ids.rs:93`) and axum 0.8's
  `Path<String>` percent-decodes too, so calling both **double-decodes** — `%2520` becomes a space
  and a literal `%2F` inside an inbox id becomes a path separator, which is the exact round-trip
  this contract's own edge-case list requires to survive. Reuse `amk_types::ids::has_forbidden_byte`
  for the NUL half rather than writing a second copy of that check. `inbox_id` compares
  **ASCII-case-folded**
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
- **The six list operations in *this* dispatch take `limit` and `page_token` only, plus `ascending`
  on four of them.** Generated, not recalled — `/v0/pods`, `/v0/inboxes`, `/v0/pods/{pod_id}/inboxes`
  and `/v0/api-keys` carry `ascending`; `/v0/pods/{pod_id}/api-keys` and
  `/v0/inboxes/{inbox_id}/api-keys` **do not**, so do not offer it there. None of the six carries
  `labels`, `before`/`after`, `include_*`, or any substring filter. Those belong to the
  messages/threads/drafts lists, which are a later dispatch; the paragraph below describes the
  machinery so it is built once, not parameters you should wire onto these six.
- **`limit`: default 100, maximum 100, `[ASSUMED]`.** No fixture settles it — every captured
  listing passed an explicit `limit`, so the server's behaviour on an omitted one was never
  observed, and `type_:Limit` is an unbounded `integer` with no `maximum`. 100 is chosen because it
  is the one documented cap anywhere in this API (`[SPEC:repo agentmail-cli]`, for filtered lists)
  and because an unbounded default is an unbounded scan. A `limit` above the maximum is **clamped,
  not rejected** — there is no `validation_error` for it in any fixture, and inventing one would be
  a wire shape. Reproducing this exactly cannot be probed on the reference account: fixture 22
  measured its inbox limit at 3, so a listing large enough to reveal a default page size cannot be
  built there.
- **Echo `limit` in the envelope only when the caller supplied it.** Every observation
  (`03-id-formats.http`, `04-pagination.http`) passed `limit` and got it back; what the server
  emits for an omitted one is unobserved, and the crate-wide rule is that an absent optional is
  omitted. Emitting our internal default would be claiming an observation we do not have.
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
  `organization_id`.** `organizations::list` **no longer exists** — it took no credential and
  returned every organization in the deployment, so reaching for it here would have been a
  cross-tenant disclosure rather than a shape bug. `.claude/contracts/amk-store-http-prereqs.md`
  decision 5 deleted it: a function that does not exist cannot be reached for, which is a stronger
  guarantee than the prose prohibition that used to stand here.
- **`POST /v0/inboxes` with an empty body must still produce an inbox** (fixture 23). Three
  defaults fire, and only one of them is ours to copy:
  - **The generated local part is adjective + noun + 3 digits, lowercase, no separator**
    (`cleananimal661`). One sample, so the *shape* is evidence and the word lists are not —
    reproduce the shape, the vocabulary is ours.
  - **The domain is configuration, `[ASSUMED]`, and must fail closed when unset.** They default to
    `agentmail.to`; AgentMailKit serves `appsynergy.io`, `imabee.com`, `imabee.ca`, `imabee.cloud`.
    A deployment with no configured primary domain **STOPs** — it does not guess.
  - **`display_name` defaults to a configured product name**, likewise `[ASSUMED]`. Theirs is
    `"AgentMail"`; ours is the operator's, not theirs.
- **The org mount's default pod must be *constructible*, not looked up by a field that does not
  exist.** `POST /v0/inboxes` at the org mount resolves the pod whose `pod_id` equals the
  `organization_id`. `amk init` mints the organization id as a UUID and creates the default pod
  carrying that same UUID, so this crate parses `organization_id` as a `Uuid`, builds the `PodId`
  from it, and **confirms it with `pods::get`**. A parse failure or a missing pod is an *internal
  error* — never an invented `default_pod_id` field on `Organization`, and never a "pick the oldest
  pod" fallback. Rule 3: if the shape is not in `amk-types` or a fixture, it does not get added.
- **`DELETE /v0/api-keys/{api_key_id}` → `204`** (fixture 23). With pods at `204` and inboxes at
  `202`, `openapi.json` is now **0 for 3** on DELETE statuses, all three documented `200`. Treat
  every remaining documented DELETE status in the spec as unverified.
- **There is no `GET /v0/api-keys/{api_key_id}`** — the live 404 body is *"Route not found"*, the
  fallback, not a resource miss. The spec has list and delete and no get-by-id.
  `amk_store::api_keys::get` therefore has **no wire route** in this dispatch; it exists for
  `authenticate` and internal use. Do not add one.
- **`suggestions[]` on an inbox collision: exactly 3, base + 4 decimal digits, no separator,
  `[ASSUMED]` in the same way the generated username is.** `reference/fixtures/05-error-catalog.http:25`
  is the only observation — `amk-probe` colliding produced
  `["amk-probe4991","amk-probe6813","amk-probe9732"]`. What that evidences is the *shape*: three
  entries, the requested username unchanged as a base, a 4-digit numeric suffix, no separator. What
  it does not evidence is whether 3 is fixed, whether the digits avoid leading zeros, or whether
  the server checks the suggestions are themselves free. **Ours must check** — a suggestion that
  collides on use is worse than no suggestion, and checking is one query. If fewer than 3 free
  candidates are found within a bounded number of draws, return the ones found rather than looping;
  `suggestions` is `Vec<String>` with `skip_serializing_if = "Vec::is_empty"`, so an empty one is
  omitted and the envelope stays legal.
- **`billing_id`, `billing_type` and `billing_subscription_id` are omitted deliberately.**
  `type_organizations:Organization` carries all three. This is the no-billing-surface rule, the same
  decision already applied to `upgrade_url`, and the conformance diff must be told rather than
  "fixed". `authentication_id`/`authentication_type` are likewise absent from `amk_types`; both are
  optional and their omission is wire-legal, recorded here for whoever revisits `amk-types`.

**Every `amk-store` dependency of these 25 operations is now on `main`.** Two prerequisite
dispatches landed after this contract was first written, and what they changed is listed here so
this contract describes the crate as it actually is:

- `.claude/contracts/amk-store-inbox-update.md` (merged `3d3e1c9`) added `inboxes::update` and
  pinned `inboxes::get`/`delete` to a pod, which they previously did not do — a cross-pod read and
  a cross-pod delete. All three take `pod_id: Option<PodId>`; `None` is the org mount.
- `.claude/contracts/amk-store-http-prereqs.md` (merged `8a14e63`) made `pods::list`,
  `inboxes::list` and `api_keys::list` return `Page<T>` with a query struct, so the six paginated
  GETs among these 25 have a persistence path; added `StoreError::PodNotEmpty`; cascaded inbox
  deletion; corrected the minted-key constants; and deleted `organizations::list`.

**Turning `Page<T>` into the wire envelope is this crate's job.** `amk-store` returns
`Page { items, next }` and deliberately builds no envelope; `amk_types::page`'s macro produces
`{count, limit?, next_page_token?, <resource>: []}`, and `next_page_token` is **omitted** on the
last page, never `null` and never `""`. Resolving `ascending: Option<bool>` into a
`SortDirection`, and `limit: Option<u64>` into a concrete limit, is likewise yours — the store
takes resolved values.

**`StoreError::PodNotEmpty` maps to `ErrorCode::CannotDelete`.** Do not re-derive its status;
`ErrorCode::status()` returns `409` (commit `2318e9c`).

The wire types already exist and are frozen —
`amk_types::inbox::UpdateInboxRequest` with `MetadataUpdate`'s three states (absent / `null` /
merge). **This crate owns the two validation rules the store deliberately does not**: an empty
`metadata` object is rejected, and each update must carry at least one of `display_name` or
`metadata`. Both produce `validation_error`; the store treats an empty merge as a no-op.

## This crate ships a `Router`, not a binary — and that is a decision, not an omission

**`amk-http` exposes a function returning an `axum::Router` plus the state it needs. It has no
`main`, no `[[bin]]`, and does not bind a port.** Tests drive the router in-process; nothing in this
dispatch listens on a socket.

Naming this explicitly because **the binaries are currently assigned to no dispatch at all** —
`find crates -name main.rs` returns nothing, no `[[bin]]` exists anywhere, and the plan names
`amkd` (`--role api|smtpd|worker|all`) and `amk` (`init|migrate|doctor|import`) only in prose under
"P0 Skeleton". This is the third time a capability has been discovered with no owner: `api-keys`
was in neither the first `amk-store` dispatch nor its deferral list, `inboxes::update` likewise,
and now the binaries. All three were found the same way — checking a contract against the code
before dispatching rather than after an implementer stopped.

So it is recorded here as the one place that owns it: **`amk` and `amkd` are the next dispatch
after this one.** `amk init` (default org + pod + root key shown once) needs only `amk-store`;
`amkd --role api` serves this crate's router. Splitting them keeps one returned diff reviewable,
the same reasoning that split `amk-store`'s first dispatch.

**The consequence for this contract, stated rather than left implicit:** the org-mount rule above
says "`amk init` mints the organization id as a UUID and creates the default pod carrying that same
UUID". That binary does not exist yet. It changes nothing you build — the handler still parses
`organization_id` as a `Uuid`, builds the `PodId`, confirms with `pods::get`, and fails closed as
an internal error — and your tests seed that arrangement directly through `amk-store`'s own
functions. What it does mean is that **P0's gate (the official Python SDK's `auth.me()` against
localhost) cannot run until the binaries land**, so do not treat that gate as yours and do not
build a binary to reach it.

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
