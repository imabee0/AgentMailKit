# amk-cli — the `amk` and `amkd` binaries — dispatch contract

Scope-derivation: `scripts/derive-bins.sh`, which prints (1) everything `amk-http` exposes for a
server to mount, including `AppState`'s fields and the config surface, (2) `amk-store`'s **whole
public surface** for the three modules these binaries touch, plus `pool.rs` and the crate's
re-exports, (3) the `New*` structs the `create` functions require, field by field, (4) the binaries
that exist today, and (5) what the plan's own P0 line requires of them. Its raw output is pasted
below and is the scope. **A reviewer re-runs the script; it does not read the list.**

Section 2 was narrower on the first pass — it grepped `connect`, `connect_unmigrated` and
`migrate!` by name, so it printed two comment lines out of the middle of `migration_status` and
never its signature, while the sections below instruct the implementer to *call* that function.
Widened to enumerate, because a derivation that cannot show a function the contract names is a
hand-written scope wearing a script's clothes.

Written by the orchestrator before dispatch. The design decisions here are settled; the implementer
resolves ordinary coding detail inside them and escalates anything else.

**This makes P0's gate runnable, and running it is the deliverable.** There is no way to point the
official Python SDK at localhost today because nothing serves HTTP: `amk-http` ships a `router()`
and no binary binds it. These two binaries close that, and then the gate — *official Python SDK
`auth.me()` against localhost returns an Identity* — is actually executed, against the dev database,
with its **verbatim transcript captured to `reference/fixtures/24-p0-gate-sdk-authme.txt`**.

That fixture is what `./scripts/check.sh`'s `p0-gate-sdk-authme` reads; the ledger line asserts the
evidence, not the code, because the gate needs a live server and is too heavy to run inside every
`check.sh`. **Writing code alone cannot flip it, and this contract said otherwise until the
pre-dispatch review caught it** — the line was a bare `pend`, statically wired to PENDING and
unreachable by any implementation. Capturing the transcript is therefore a required output of this
dispatch, not a report of it.

## Derivation output (verbatim)

```
== 1. what amk-http exposes for a server to mount ==
  lib.rs:37:pub struct AppState {
  lib.rs:52:pub fn router(state: AppState) -> Router {
  --- AppState fields ---
  pub struct AppState {
      pub pool: PgPool,
      pub config: AppConfig,
  }
  --- config surface ---
  pub struct AppConfig {
      /// The domain a `POST /v0/inboxes` with no `domain` field is created under. `None` means "not
      /// configured" — creation without an explicit `domain` then fails closed.
      pub primary_domain: Option<String>,
      /// The `display_name` a `POST /v0/inboxes` with no `display_name` field gets. `None` means
      /// "not configured" — same fail-closed rule as `primary_domain`.
      pub product_name: Option<String>,
  }

== 2. what amk-store exposes for init, migrate and doctor ==
  --- crate re-exports (lib.rs) ---
  lib.rs:41:pub mod api_keys;
  lib.rs:42:pub mod error;
  lib.rs:43:pub mod inboxes;
  lib.rs:44:pub mod messages;
  lib.rs:45:pub mod organizations;
  lib.rs:46:pub mod pagination;
  lib.rs:47:pub mod pods;
  lib.rs:48:pub mod pool;
  lib.rs:49:pub mod threads;
  lib.rs:51:pub use error::{PageTokenError, StoreError};
  lib.rs:52:pub use pagination::{
  lib.rs:55:pub use pool::{connect, connect_unmigrated, migration_status, MigrationStatus};
  --- pool.rs public surface ---
  pool.rs:12:pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
  pool.rs:25:pub async fn connect_unmigrated(database_url: &str) -> Result<PgPool, sqlx::Error> {
  pool.rs:38:pub struct MigrationStatus {
  pool.rs:48:    pub fn is_current(&self) -> bool {
  pool.rs:67:pub async fn migration_status(pool: &PgPool) -> Result<MigrationStatus, sqlx::Error> {
  --- every public fn in the modules init/doctor touch ---
  organizations.rs:64:pub async fn create(pool: &PgPool, new: NewOrganization) -> Result<Organization, StoreError> {
  organizations.rs:80:pub async fn get(
  organizations.rs:110:pub async fn exists(pool: &PgPool) -> Result<bool, StoreError> {
  organizations.rs:117:pub async fn delete(pool: &PgPool, organization_id: &OrganizationId) -> Result<bool, StoreError> {
  pods.rs:36:pub async fn create(pool: &PgPool, new: NewPod) -> Result<Pod, StoreError> {
  pods.rs:82:pub async fn get(
  pods.rs:124:pub async fn list(
  pods.rs:197:pub async fn delete(
  api_keys.rs:399:pub async fn create(pool: &PgPool, new: NewApiKey) -> Result<CreateApiKeyResponse, StoreError> {
  api_keys.rs:470:pub async fn get(
  api_keys.rs:510:pub async fn list(
  api_keys.rs:595:pub async fn delete(
  api_keys.rs:623:pub async fn authenticate(
  api_keys.rs:663:pub async fn touch_used_at(pool: &PgPool, api_key_id: &ApiKeyId) -> Result<bool, StoreError> {

== 3. the New* structs those creates require ==
  organizations.rs: pub struct NewOrganization {
  organizations.rs:     pub organization_id: OrganizationId,
  organizations.rs:     pub inbox_limit: Option<u64>,
  organizations.rs:     pub domain_limit: Option<u64>,
  organizations.rs: }
  pods.rs: pub struct NewPod {
  pods.rs:     pub organization_id: OrganizationId,
  pods.rs:     pub pod_id: PodId,
  pods.rs:     pub client_id: Option<String>,
  pods.rs:     pub name: String,
  pods.rs: }
  api_keys.rs: pub struct NewApiKey {
  api_keys.rs:     pub organization_id: OrganizationId,
  api_keys.rs:     /// Exactly one of `pod_id`/`inbox_id` may be `Some`, or both `None` for an organization-scoped
  api_keys.rs:     /// key — enforced by the migration's `CHECK`, not merely here.
  api_keys.rs:     pub pod_id: Option<PodId>,
  api_keys.rs:     pub inbox_id: Option<InboxId>,
  api_keys.rs:     pub name: String,
  api_keys.rs:     /// `None` grants everything; `Some(ApiKeyPermissions::default())` grants nothing — the
  api_keys.rs:     /// NULL-vs-`{}` distinction `amk_types::api_key::KeyGrants::from_wire` owns. Passed straight
  api_keys.rs:     /// through; this module never restates that semantics.
  api_keys.rs:     pub permissions: Option<ApiKeyPermissions>,
  api_keys.rs: }

== 4. binaries that exist today ==
  (none — no main.rs anywhere)
  crates/amk-core/Cargo.toml:0
  crates/amk-http/Cargo.toml:0
  crates/amk-store/Cargo.toml:0
  crates/amk-types/Cargo.toml:0

== 5. what the plan's P0 line requires of them ==
  61:| TLS | cert-manager (Cloudflare DNS-01 — DNS already Cloudflare-as-code) → Secrets; amkd terminates TLS via rustls with hot-reload | kills the in-pod ACME sidecar pattern |
  66:Workspace: crates `amk-types` (wire types + error catalog), `amk-core` (scope/permissions/threading/labels/ids), `amk-store` (sqlx, migrations, blobs, FTS, signed downloads), `amk-ingest`, `amk-outbound`, `amk-events`, `amk-jobs`, `amk-http`, `amk-mcp`, `amk-dns`, `reply-extract`; bins `amkd` (--role api|smtpd|worker|all), `amk` (init/migrate/doctor/import); `conformance/`; `deploy/k3s/`; `reference/` (vendored openapi.json + SDK extracts + mcp-manifest).
  108:- **P0 Skeleton** — workspace, config, migrations, `amk init` (default org+pod, root key shown once), Bearer auth deny-by-default, error shapes **per A8 — RESOLVED, build the asymmetry**: auth-layer failures (missing/invalid credential) return the bare gateway body `{"message":"Unauthorized"}` 401 / `{"message":"Forbidden"}` 403 (no name/code/fix/docs); app-layer failures return the full envelope. cursor pagination = base64(JSON keyset {sort-key,id}) per fixture 04. Gate: official Python SDK `auth.me()` against localhost returns Identity, AND the shape-provenance CI check passes from the first commit: (i) dependency direction — a `cargo metadata`-based script asserting no dependency path from amk-types/amk-core/amk-store to amk-import (chosen over cargo-deny: zero extra tooling, exact graph); (ii) naming — a grep-based deny-lint over those three crates' sources rejecting Stalwart/JMAP-derived concepts (JMAP, Sieve, blob-id-as-Stalwart, RocksDB key shapes, mailbox-role enums absent from AgentMail's spec), run as a CI step alongside the tests; (iii) boundary types — the stalwart-labs crates (mail-parser, mail-auth, mail-send, mail-builder, smtp-proto) are an unguarded leak path because their types are ergonomic and right there: assert no `mail_parser::`/`mail_auth::`/`mail_send::`/`smtp_proto::` type appears in any public signature or re-export of amk-types/amk-core/amk-store — those types live only inside amk-ingest/amk-outbound, converted at the boundary.
  557:argon2id hash", and there is no `api_keys` table, no repository and no hash in the crate. `amk init`
```

Everything the binaries need already exists. Section 4 is the finding that created this dispatch:
**no binary exists anywhere in the workspace**, and none was assigned to any prior one. The plan
names `amkd` and `amk` only in prose under "P0 Skeleton". This is the third capability discovered
with no owner — after `api-keys` and `inboxes::update` — and it was found the same way all three
were: checking a contract against the code before dispatching.

## `[SPEC:*]` and `[TESTED]` citations

- `[SPEC:plan]` P0 Skeleton: *"workspace, config, migrations, `amk init` (default org+pod, root key
  shown once)"*, and the bins line: `amkd` (`--role api|smtpd|worker|all`), `amk`
  (`init|migrate|doctor|import`).
- `[TESTED]` `reference/fixtures/22-org-mount-and-delete-semantics.txt` — the account's default pod
  carries the organization's **own UUID** (`pod_id == organization_id`). `amk-http`'s org-mount
  inbox creation resolves the default pod by that equality and fails closed otherwise, so **`amk
  init` is what makes that resolvable.** Get this wrong and `POST /v0/inboxes` at the org mount is
  an internal error in every deployment.
- `[TESTED]` `reference/fixtures/01-auth-me.http` — an org-scoped identity's `scope_id` equals the
  `organization_id`, which is the same fact seen from the auth side.

`amk-types`, `amk-core`, `amk-store` and `amk-http` are all **frozen** for this dispatch. If
something you need does not exist in them, **STOP and report**.

## Writable paths (exact)

`crates/amk-cli/**`, the workspace `Cargo.lock`, the root `Cargo.toml` **only** to add
`"crates/amk-cli"` to `[workspace.members]`, and `reference/fixtures/24-p0-gate-sdk-authme.txt`
(the gate transcript — the one file outside the crate this dispatch must produce). Nothing else. If
the work requires a path outside those, **STOP and report**.

**The fixture is a verbatim capture, under the same rules as every other file in that directory:**
the commands run and their unmodified output. No `Authorization` header value is ever written into
it, and the root key minted by `amk init` is `<redacted>` at capture — not redacted afterwards. A
probe that printed a secret and cleaned up later already happened once here (fixture 23) and is
recorded as a defect in the probe, not the API.

## Decisions (settled — implement, do not relitigate)

### Crate layout

One crate, `crates/amk-cli`, with two `[[bin]]` targets (`amk`, `amkd`) over a shared `lib.rs`.
Config loading, DSN handling and the argument parser live in the library half and are unit-tested
there; each `main.rs` stays thin. Two separate crates would duplicate all three.

### No new dependency — the parser is hand-written

**Do not add `clap` or any other argument parser.** The surface is four subcommands and one flag.
A hand-written parser for that is a few dozen lines and fully testable; `clap` is a dependency tree
for it. This is a decision, not an oversight — if the CLI ever grows options that make hand-parsing
genuinely awkward, that is the moment to revisit, and it is not this dispatch.

**Test the parser directly**: an unknown subcommand, a missing required argument, no arguments at
all, and `--help` each produce a clear message and a non-zero exit (except `--help`, which is 0).
A parser that silently accepts garbage is how a deployment ends up running the wrong role.

### Configuration — environment only, and it fails closed

| Variable | Required | Meaning |
|---|---|---|
| `AMK_DATABASE_URL` | **yes** | Postgres DSN. No default — a default here would silently point production at a dev database. |
| `AMK_BIND` | no | `amkd --role api` listen address; default `127.0.0.1:8080`. |
| `AMK_PRIMARY_DOMAIN` | no | `AppConfig::primary_domain`. Absent means inbox creation without an explicit `domain` fails closed — that is `amk-http`'s rule and this crate only passes the value through. |
| `AMK_PRODUCT_NAME` | no | `AppConfig::product_name`. Same fail-closed rule. |

A missing `AMK_DATABASE_URL` is a clear error naming the variable, not a panic and not a default.

### **The DSN and the root key must never reach a log, an error message, or a file**

This is the security requirement of the dispatch and the one most easily lost:

- **`sqlx::Error`'s `Display` can carry the connection URL, and a DSN carries a password.** Never
  print a `sqlx::Error` (or anything wrapping one) verbatim. Map connection failures to a message
  that names the *variable* (`AMK_DATABASE_URL`) and the failure kind, never the value. Write a
  test that sets `AMK_DATABASE_URL` to a DSN with a recognisable password, forces a connection
  failure, and asserts that string does **not** appear in the output.
- **The root key's plaintext is printed exactly once, to stdout, and nowhere else.** Never to
  `tracing`, never to stderr, never to a file, never through `Debug`.
  `amk_types::api_key::CreateApiKeyResponse` has a **hand-written redacting `Debug`** precisely so
  a stray `{:?}` cannot leak it — do not defeat that by formatting the field into a log line.
  Print `response.api_key` explicitly, with a line telling the operator it will not be shown again.

### `amk init` — and it is what makes the org mount work

1. Mint a fresh v4 UUID. That value is **both** the `organization_id` (as its string form) **and**
   the default pod's `pod_id`. This is not a convenience: `amk-http` resolves the org-mount default
   pod by `pod_id == organization_id` (fixture 22), so any other arrangement makes
   `POST /v0/inboxes` an internal error forever.
2. `organizations::create` with that id, `inbox_limit`/`domain_limit` both `None`.
3. `pods::create` with `pod_id` equal to the same UUID and a `name` of your choosing — `[ASSUMED]`,
   no fixture names it; `"Default Pod"` matches what the reference account calls its own and is the
   obvious choice.
4. `api_keys::create`, org-scoped (`pod_id: None`, `inbox_id: None`), `permissions: None` — which
   **grants everything**, the NULL-vs-`{}` distinction `amk-store` already owns. This is the root
   key.
5. Print the organization id, the pod id, and the key exactly once.

**Re-running `init` must not mint a second root key — and the naive reading of that is wrong.**
It is tempting to reason "`organizations::create` is a plain `INSERT`, so a second run fails on the
unique violation". It does not. Step 1 mints a **fresh** UUID, so the second run's row collides with
nothing and the `INSERT` **succeeds** — silently creating a second organization, a second default
pod and a second root key. An untracked credential holding every permission is the worst possible
outcome of a typo'd re-run, and it would happen with no error at all.

So: **call `amk_store::organizations::exists(&pool)` first, and refuse if it returns `true`.** That
function was added for this caller (`d889246`) after the pre-dispatch review found this instruction
unresolvable as originally written. It is deliberately not a resurrection of `organizations::list`,
which was deleted for taking no credential and returning every organization in the deployment: this
discloses one bit — *some* organization exists — and no identifier, no count, no row.

The refusal message names what to do (there is already an organization; use `amk doctor` to inspect
the deployment) and exits non-zero. **Mint nothing before the check** — not the UUID, not the key.

### `amk migrate`

`amk_store::connect(url)` — the migrator is compiled in (`sqlx::migrate!`). Report the state
afterwards using `amk_store::migration_status(&pool)`, which returns
`MigrationStatus { applied, embedded }`: say how many are applied of how many embedded, and whether
that is current. Idempotent by construction.

**Do not read `_sqlx_migrations` yourself, and do not embed a second `sqlx::migrate!` pointed at
`amk-store`'s migrations.** The first routes persistence around the crate that owns it; the second
is two declarations of migration ownership, and the copy is the one that drifts. `migration_status`
exists (`d889246`) so neither is necessary — it was added after the pre-dispatch review found this
section asking for information the public interface did not expose.

### `amk doctor`

Read-only. Reports, each on its own line:

- **Is the DSN even parseable?** Parse it before connecting — `AMK_DATABASE_URL.parse::<sqlx::postgres::PgConnectOptions>()`.
  This matters more than it looks: `[TESTED]` against this workspace on 2026-08-16, a **malformed**
  DSN does not report as malformed. `PgPoolOptions::connect` defers the parse, so
  `not-a-url-at-all` and `mysql://…` both surface as `pool timed out while waiting for an open
  connection` after five seconds — identical to an unreachable database. `doctor` exists to tell
  those apart, so it must do the parse itself. No new dependency: `sqlx` is already a dependency of
  this crate's own dependency tree via the workspace pin.
- Whether the database is reachable.
- `migration_status`: applied of embedded, and current or not.
- For each configuration variable, **set** or **unset** — never its value.

`doctor` is what someone runs when something is wrong, which is exactly when they will paste its
output into a chat window. It must be safe to paste.

### `amk import` — does not exist yet

`import` is P6 and `amk-import` is not written. **Do not add the subcommand**, not even as a stub
that prints "not implemented": a stub is indistinguishable from a feature at the call site and the
plan's write order puts `amk-import` last for reasons that outlive this dispatch. An unknown
subcommand error is the correct behaviour today.

### `amkd --role`

`--role api` serves `amk_http::router(AppState { pool, config })` on `AMK_BIND` over plain HTTP.
TLS is P6 (cert-manager terminating via rustls), not here.

`smtpd`, `worker` and `all` are **recognised and rejected** with a message naming the phase that
will implement them — not silently accepted, not treated as unknown. A role that parses and does
nothing is a server that looks like it is running and is not.

## Assigned edge cases (write the test before the code it targets)

- The argument parser: unknown subcommand, unknown `--role`, missing `--role`, no arguments,
  `--help`. Each asserts the exit code **and** that the message names what was wrong.
- `AMK_DATABASE_URL` unset → an error naming the variable; the process does not panic.
- A DSN whose password appears nowhere in the output of a failed connection — asserted on the
  captured output, not by reading the code.
- `amk init` against a fresh database: assert `organization_id == pod_id.to_string()`, that the key
  is org-scoped, and that its permissions are `None` (grants everything) rather than
  `Some(default)` (grants nothing) — those two are one character apart in the source and opposite
  in effect.
- `amk init` twice → the second run fails, mints no key, and says why.
- The `amk_http::router` mounted by `amkd --role api` answers `GET /v0/auth/me` with the root key
  minted by `amk init` — the two halves of this dispatch meeting, which is also the P0 gate in
  miniature.
- `amkd --role smtpd|worker|all` → rejected with a message naming the phase, non-zero exit.
- `amk doctor` output contains no configuration **value** — asserted by setting a recognisable
  sentinel in each variable and grepping the output for it.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type. No JMAP,
  Sieve, RocksDB, or mailbox-role concept.
- **No SQL in this crate.** All persistence goes through `amk-store`'s public interface.
- No new dependency — no `clap`, no `tracing-subscriber` beyond what a workspace pin already
  provides, nothing. If you believe you need one, **STOP and report**.
- Do not edit `amk-types`, `amk-core`, `amk-store`, `amk-http`, the plan, any contract file, or
  `scripts/**`.
- No billing surface. No `import` subcommand.
- Do not commit `.amk-task.md`, `.amk-scope` or `.amk-brief.md`.

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-cli`, `./scripts/check.sh`,
and a **two-directional** mutation table. `cargo-mutants` does not mutate string literals, so
mutate by hand: the env-var names, the `pod_id == organization_id` equality, the role strings, and
the `permissions: None` on the root key. Every guard gets both directions — delete it (must kill a
test) **and** widen it (must also kill a test). Mutate on a **private scratch copy**, never in the
dispatch worktree.

**Then run the P0 gate and report its real output**: install the pinned SDK
(`conformance/requirements-gate.txt`) into `.venv-gate`, start `amkd --role api` against the dev
database, and call `auth.me()` with the root key from `amk init`. That gate has been PENDING since
P0 began. If it fails, report the failure — a failing gate reported honestly is the deliverable; a
passing one claimed without its output is not.
