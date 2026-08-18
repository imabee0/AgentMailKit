# Contract — `amkd --role smtpd` wires `amk-ingest`

Scope-derivation: 2026-08-18 against `main` @ `c1f3a17`. Ingest exists; the binary still
rejects `smtpd` before it connects. G2 (PR 8) needs one injected inbound with a matching
`thread_id`. The scope is this command's output.

```
$ rg -n "not_yet_implemented|serve_api|AmkdRole::Smtpd" crates/amk-cli/src
crates/amk-cli/src/server.rs:15:pub fn not_yet_implemented(role: AmkdRole) -> Option<&'static str> {
crates/amk-cli/src/server.rs:18:        AmkdRole::Smtpd => Some(
crates/amk-cli/src/server.rs:32:pub async fn serve_api(...)
crates/amk-cli/src/bin/amkd.rs:27:    if let Some(message) = server::not_yet_implemented(role) {
crates/amk-cli/src/bin/amkd.rs:42:    if let Err(e) = server::serve_api(&url, &bind, app_config).await {

$ rg -n "^pub " crates/amk-ingest/src/lib.rs crates/amk-ingest/src/smtp.rs crates/amk-ingest/src/lookup.rs
lib.rs: FixedInboxLookup, InboxLookup, serve_session, IngestConfig, accept, StorePersist, Authenticator
smtp.rs: IngestConfig, serve_session   # per-connection; no listen loop
lookup.rs: InboxLookup, FixedInboxLookup   # no store-backed impl

$ rg -n "pub async fn get" crates/amk-store/src/inboxes.rs
inboxes.rs:157:pub async fn get(pool, organization_id, pod_id, inbox_id)
# RCPT has only the address. inbox_id is PRIMARY KEY (0003_inboxes.sql).

$ rg -n "pub const AMK_" crates/amk-cli/src/config.rs
AMK_DATABASE_URL AMK_BIND AMK_PRIMARY_DOMAIN AMK_PRODUCT_NAME
# no AMK_SMTP_* — do not invent one.

$ rg -n "smtpd" crates/amk-cli/tests/process.rs
306: ("smtpd", "amk-ingest")   # currently requires rejection before AMK_DATABASE_URL
```

## The evidence

`[SPEC:.claude/contracts/amk-ingest.md]` — session, RCPT two-check, persist, 09b, C3.
`[SPEC:docs/PLAN.md]` P2 ingest daemon; `amkd --role api|smtpd|worker|all`; local-domain
RCPT; greet-pause; `AMK_PRIMARY_DOMAIN` is the domain `POST /v0/inboxes` uses.
`[SPEC:docs/execute-plan-v1.md]` PR 8: injected inbound + matching `thread_id`.
`[SPEC:crates/amk-store/migrations/0003_inboxes.sql]` `inbox_id TEXT PRIMARY KEY`.

## Writable paths

- `crates/amk-cli/src/server.rs`, `src/bin/amkd.rs`, `src/args.rs` (help text only),
  `src/config.rs` (var_presence only if you reuse existing names — **no new env var**),
  `Cargo.toml` (`amk-ingest` dep only).
- `crates/amk-cli/tests/**` — flip the smtpd-rejected process test; add a bind+inject test.
- `crates/amk-store/src/inboxes.rs` — **one** function, lookup by `InboxId` alone
  (normalized PK). Same row as `get`. No new column.
- `crates/amk-store/tests/**` — tests for that lookup only.
- `crates/amk-ingest/src/lookup.rs` — `StoreInboxLookup` over the new store function.
- `crates/amk-ingest/src/accept.rs` — if `threads::record_member` exists (it does on this
  tree), call it on join. Do not invent an updater.

**Not writable:** `amk-types`, `amk-core`, `amk-http`, `amk-outbound`, `docs/PLAN.md`,
`scripts/**`. `--role worker` and `--role all` stay rejected (`all` still needs jobs).

## What to build

1. **`not_yet_implemented(Smtpd) → None`.** `amkd` matches the role: `serve_api` vs
   `serve_smtpd`.
2. **`serve_smtpd`** — connect (`AMK_DATABASE_URL`, fail as `serve_api` does), bind
   **`AMK_BIND`** (existing var; this role is SMTP, not HTTP). Accept loop:
   `TcpListener` → `serve_session`.
3. **`IngestConfig`**
   - `local_domains` = `[AMK_PRIMARY_DOMAIN]`. **Missing `AMK_PRIMARY_DOMAIN` → refuse
     to start** (fail closed; do not invent a domain).
   - `hostname` = that domain.
   - `max_message_bytes` = `amk_http::config::DEFAULT_MAX_BODY_BYTES` (existing
     `[INFERRED]` 8 MiB — reuse, do not mint a second number).
   - `greet_pause` = `Duration::from_millis(250)` — the non-zero pause
     `crates/amk-ingest/tests/smtp_session.rs` already uses to pin 421. Do **not**
     mint a second number. Fixture/PLAN observe the 421, not a duration.
4. **Lookup** — store `get` by normalized `inbox_id` only; `StoreInboxLookup` implements
   `InboxLookup`. RCPT 550 if `None` or domain not local (existing ingest checks).
5. **Auth** — `Authenticator::live()` for the binary. Tests keep stubs.
6. **Persist** — `StorePersist`. On join, `threads::record_member`.

## Assigned edge cases

SMTP or process observables. Parser Ok/Err is not enough.

1. `amkd --role smtpd` **without** `AMK_DATABASE_URL` exits 1 and names
   `AMK_DATABASE_URL` (connect, not the old “not implemented” string).
   `[SPEC:process.rs` pattern`]`
2. `amkd --role smtpd` with DB but **no** `AMK_PRIMARY_DOMAIN` exits 1 and names
   that variable. Store empty.
3. Spawn **`amkd --role smtpd`** (not `serve_session` in-process). Bind
   `127.0.0.1:<ephemeral>`. Seed a row whose `inbox_id` is `relay@gmail.com`
   (lookup would be `Some`). RCPT `relay@gmail.com` → **550** (domain not in
   `AMK_PRIMARY_DOMAIN`). Deleting only the local-domain check becomes 250.
   Restoring `not implemented` never reaches 220. `[SPEC:PLAN.md:252]`
4. Same **`amkd --role smtpd` process**. Create inbox `user@{AMK_PRIMARY_DOMAIN}`.
   SMTP DATA a message (MAIL FROM a domain with no SPF — live `Authenticator`
   yields none). GET-by-id the Message-ID: row exists, labels contain `received`.
   Do not use `accept()`/`FixedInboxLookup` for this case.
5. **Same listening smtpd**, second DATA with `In-Reply-To` of case 4 (bare or
   bracketed). GET-by-id `thread_id` equals case 4. This is G2’s injected inbound.
   `[SPEC:16 R1]` `[SPEC:21]`
6. `--role worker` and `--role all` still print “not implemented” and never
   connect.

**Mutation** (scratch outside the tree; `rm -rf`):
- Restore `Smtpd => Some("not implemented…")` — kills cases 3, 4, and 5
  (no 220/250 from `amkd --role smtpd`).
- Delete the local-domain RCPT check — kills case 3 (`relay@gmail.com` → 250).

## Prohibitions

- No new env var. No new `amk-types` field. No SMTP AUTH/TLS. No `:25` in tests.
- No real outbound mail. Do not resolve C2. Do not flip worker/all.
- If ambiguous, **STOP and report**.

## Reporting

- `cargo test -p amk-cli -p amk-ingest -p amk-store`
- `./scripts/check.sh` — fail the report if the DB-skip warning prints.
- `./scripts/shape-provenance.sh`
- Mutation report.
