# Contract — amk-outbound remainder: Transport + four send HTTP operations

Scope-derivation: re-run 2026-08-18 against this tree. The first draft grepped only
`SendMessageRequest` and listed `lib.rs` as “four `.route()` calls only”. That left
`ReplyToMessageRequest` / `ReplyAllMessageRequest` (two of four bodies) out of `amk-types`,
and left `AppState` / `config.rs` / thread-aggregate update unwritable — the same
construction-site hole extractor-rejection rev 3 hit. The scope is this command's output,
not a recalled list.

```
$ python3 -c '
import json
from pathlib import Path
p=json.loads(Path("reference/openapi.json").read_text())
for path, ops in p["paths"].items():
    if any(s in path for s in ("/messages/send","/reply","/forward")):
        for m,op in ops.items():
            if m.startswith("x-"): continue
            ref=((op.get("requestBody") or {}).get("content") or {}).get("application/json",{}).get("schema",{})
            print(f"{m.upper()} {path}  body={ref}")
print("--- request schemas ---")
for name in ("type_messages:ReplyToMessageRequest","type_messages:ReplyAllMessageRequest","type_messages:SendMessageRequest"):
    print(name, list((p["components"]["schemas"][name].get("properties") or {}).keys()))
'
POST /v0/inboxes/{inbox_id}/messages/send  body={'$ref': '#/components/schemas/type_messages:SendMessageRequest'}
POST /v0/inboxes/{inbox_id}/messages/{message_id}/reply  body={'$ref': '#/components/schemas/type_messages:ReplyToMessageRequest'}
POST /v0/inboxes/{inbox_id}/messages/{message_id}/reply-all  body={'$ref': '#/components/schemas/type_messages:ReplyAllMessageRequest'}
POST /v0/inboxes/{inbox_id}/messages/{message_id}/forward  body={'$ref': '#/components/schemas/type_messages:SendMessageRequest'}
--- request schemas ---
type_messages:ReplyToMessageRequest ['labels', 'reply_to', 'to', 'cc', 'bcc', 'reply_all', 'text', 'html', 'attachments', 'headers']
type_messages:ReplyAllMessageRequest ['labels', 'reply_to', 'text', 'html', 'attachments', 'headers']
type_messages:SendMessageRequest ['labels', 'reply_to', 'to', 'cc', 'bcc', 'subject', 'text', 'html', 'attachments', 'headers']

$ rg -n "pub struct Reply" crates/amk-types/src/message.rs
247:pub struct ReplyToMessageRequest {
280:pub struct ReplyAllMessageRequest {

$ rg -n "AppState::new|AppConfig \{" crates --glob '*.rs'
crates/amk-http/src/lib.rs:39:pub struct AppState {
crates/amk-http/src/lib.rs:45:    pub fn new(pool: PgPool, config: AppConfig) -> Self {
crates/amk-http/src/config.rs:19:pub struct AppConfig {
crates/amk-http/tests/support/mod.rs:55:    let config = AppConfig {
crates/amk-http/tests/support/mod.rs:60:    router(AppState::new(pool, config))
crates/amk-http/tests/support/mod.rs:66:    router(AppState::new(pool, AppConfig::default()))
crates/amk-cli/src/config.rs:51:    AppConfig {
crates/amk-cli/src/server.rs:36:    let app = amk_http::router(AppState::new(pool, config));
crates/amk-cli/tests/init_and_serve.rs:148:    let app = router(AppState::new(db.pool.clone(), AppConfig::default()));
crates/amk-http/src/lib.rs:191:        let state = AppState::new(pool, AppConfig::default());

$ rg -n "pub async fn" crates/amk-store/src/threads.rs crates/amk-store/src/messages.rs
messages.rs:55:pub async fn insert
messages.rs:201:pub async fn get
messages.rs:427:pub async fn update
threads.rs:52:pub async fn insert
threads.rs:148:pub async fn get_with_messages
threads.rs:316:pub async fn update   # labels-only
```

Forward uses `SendMessageRequest` (has `subject`). Reply has no `subject` (PLAN.md: parent + `Re:`).
Reply-all body has no recipients. `reply_all: true` on `ReplyToMessageRequest` is mutually exclusive
with `to`/`cc`/`bcc` (PLAN.md / SDK).

## The evidence

`[SPEC:reference/openapi.json]` — the four POSTs and the three request schemas above.
`[SPEC:reference/types_dump.txt]` — same property sets on the Python SDK types.
`[SPEC:reference/fixtures/15-compile-spike.txt]` — `mail-send = "0.6"`, `mail-builder = "0.4"`,
`mail-auth = "0.12"`. Do not bump.
`[SPEC:reference/fixtures/10-dkim-keys.txt]` / `10b-dkim-extraction.txt` — DER, not PEM.
`[SPEC:reference/fixtures/21-unbracketed-in-reply-to.txt]` — C3: re-bracket before matching.
`[SPEC:reference/fixtures/03-id-formats.http]` — sent `message_id` IS the RFC 5322 Message-ID.
`[SPEC:docs/execute-plan-v1.md]` PR 4 / PR 5 assigned cases (HTTP vs MIME).

## Writable paths (implementer)

- `crates/amk-outbound/**` — Transport (direct-to-MX + smarthost) and anything needed to expose it.
- `crates/amk-http/src/handlers/messages.rs` — the four handlers.
- `crates/amk-http/src/lib.rs` — `AppState` (pool + config + `Keyring` + a `Transport`) and the
  four `.route()` mounts. Changing `AppState::new`'s arity is in scope.
- `crates/amk-http/src/config.rs` — only fields required to build a `Keyring` / choose
  direct-to-MX vs smarthost. Fail closed if a send domain has no key. Do not invent env var names
  that are not already in `amk-cli` unless you load keys only from test fixtures / `AppState`
  injection.
- `crates/amk-http/Cargo.toml` — `amk-outbound` dependency only (already present if so).
- `crates/amk-http/tests/**` including `tests/support/mod.rs` — `AppState` construction sites.
- `crates/amk-cli/src/server.rs`, `crates/amk-cli/src/config.rs`, `crates/amk-cli/tests/**` —
  every `AppState::new` / `AppConfig {` site. Spread `..Default` where a new field has a safe
  default (the extractor-rejection lesson).
- `crates/amk-store/src/threads.rs` — **one new function**, `record_member`, that updates the
  existing thread columns (`last_message_id`, `message_count`, `size`, `senders`, `recipients`,
  timestamps, preview/subject as already stored) when a new message joins. No new column, no new
  wire field. `threads::update` stays labels-only.
- `crates/amk-store/tests/messages_and_threads.rs` — tests for `record_member` only.

**Not writable:** `crates/amk-types/**` (orchestrator already added the two request types),
`crates/amk-core/**`, `docs/PLAN.md`, `scripts/**`.

## What to build

1. **Delivery** — `mail-send` behind the existing `Transport` trait. Direct-to-MX and smarthost,
   both configurable. Tests use `RecordingTransport` only. No real mail.
2. **HTTP send / reply / reply-all / forward** — inbox-scoped mounts only (the derivation listed
   no org/pod send paths). `AppState` carries `Keyring` + `Transport`. Persist **after** sign+deliver
   succeeds: `messages::insert` with `sent`, then `threads::insert` (new thread: send/forward) or
   `threads::record_member` (reply / reply-all). Reply/reply-all set `In-Reply-To`/`References`
   from the parent (C3). Forward does not join the parent thread.
3. **reply-all recipients** — from the parent's `from`/`to`/`cc`, minus the sending inbox.
   `[INFERRED]`. `ReplyAllMessageRequest` has no recipient fields; `ReplyToMessageRequest` with
   `reply_all: true` is the same derivation and rejects if `to`/`cc`/`bcc` are also set.

## Assigned edge cases

HTTP integration tests unless marked MIME/unit. MIME-only tests do not discharge HTTP cases.

**PR 4 (Transport, `crates/amk-outbound/**`):**
1a. No key → `OutboundError::NoSigningKey`, recording fake has no `SignedMessage`.

**PR 5 (HTTP + persist):**
1b. No-key send: fail-closed error **and** `messages::get`/list empty.
2. `reply` GET the thread: parent membership, same `thread_id`.
3. Unbracketed parent still joins (fixture 21 / C3), via GET thread.
4. `reply-all` excludes sending inbox, de-duplicates.
5. `forward` returned `thread_id` ≠ parent.
6. Hostile `headers` (From, Bcc, CR/LF) plus CR/LF in `to` and `subject` (PLAN.md:246).
7. Send to a local inbox still goes through `Transport` (fake has one `SignedMessage`) and stored
   raw carries `DKIM-Signature`.
8. Attachment size cap−1 accepted; cap and cap+1 rejected or URL-threshold per toolkit.

Mutation (scratch outside the tree; report path, mutant, killed test, `rm -rf`):
- PR 4: remove DKIM signing; passthrough `check_headers`.
- PR 5: persist then ignore `NoSigningKey`; mint a new `thread_id` on reply; copy `headers` onto
  MIME after `build_signed`.

## Prohibitions

- No stalwart-labs type in any public signature. `shape-provenance.sh` enforces it.
- No Stalwart or JMAP concept. No new `amk-types` field. No new `amk-core::threading` rule.
- Do not send real mail from a test. Do not implement drafts / `send_at`. Do not resolve C2.
- Do not invent a second permissions object, a folder entity, or a thread-label union rule.
- If the contract is ambiguous or appears wrong, **STOP and report**.

## Reporting

- `cargo test -p amk-types -p amk-outbound -p amk-http -p amk-store` and `./scripts/check.sh`
  (fail if the DB-skip warning is printed).
- `./scripts/shape-provenance.sh` — `amk-outbound` clean on the boundary-type check.
- `./scripts/derive-implemented-paths.sh` — mounted set grown by exactly four operations.
- Mutation report as specified above.
