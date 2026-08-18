# Contract — amk-ingest: SMTP state machine + persist

Scope-derivation: amended 2026-08-18 after the pre-dispatch three-lens review (contract 6
open, tests 8 open, provenance CLEAN). Re-run against this tree (tip `ed59e73`). The
scope is this command's output, not a recalled list. Persist construction sites are the
`NewMessage` / `NewThread` **fields**, not the function names.

```
$ python3 -c '
import json
from pathlib import Path
p=json.loads(Path("reference/openapi.json").read_text())
hits=0
for path, ops in p["paths"].items():
    pl=path.lower()
    if any(k in pl for k in ("ingest","smtp","receive","inbound","/raw")):
        for m,op in ops.items():
            if str(m).startswith("x-"): continue
            print(f"{m.upper()} {path}  op={op.get(\"operationId\",\"\")}")
            hits += 1
print(f"(hits={hits}; /raw is GET raw-message download, not an ingest POST)")
'
GET /v0/inboxes/{inbox_id}/messages/{message_id}/raw  op=get-raw
(hits=1; /raw is GET raw-message download, not an ingest POST)

$ sed -n '3,10p' Cargo.toml
members = [
    "crates/amk-types",
    "crates/amk-core",
    "crates/amk-store",
    "crates/amk-http",
    "crates/amk-cli",
    "crates/amk-outbound",
]

$ rg -n 'mail-send|mail-builder|mail-auth|mail-parser|smtp-proto' Cargo.toml
32:# corrections that spike found apply to axum/mail-auth/hickory/rmcp, not to sqlx.
48:# The exact versions the spike RESOLVED, not the loose ones its manifest requested. `mail-send`
49:# 0.6.0 depends on `mail-auth ^0.8`, which pins `hickory-resolver =0.26.0-alpha.1` and cannot
50:# co-resolve with `mail-auth 0.12`; 0.6.1 is the release that moved to the 0.12 line. The spike's
53:mail-send = "=0.6.1"
54:mail-builder = "=0.4.4"
55:mail-auth = "=0.12.0"

$ rg -n "not implemented yet|amk-ingest" crates/amk-cli/src/server.rs crates/amk-cli/tests/process.rs
crates/amk-cli/tests/process.rs:306:        ("smtpd", "amk-ingest"),
crates/amk-cli/src/server.rs:19:            "amkd --role smtpd is not implemented yet -- mail ingest/outbound is amk-ingest and \
crates/amk-cli/src/server.rs:23:            "amkd --role worker is not implemented yet -- background job processing is amk-jobs.",
crates/amk-cli/src/server.rs:26:            Some("amkd --role all is not implemented yet -- it requires every role above.")
crates/amk-cli/src/server.rs:60:        assert!(smtpd.contains("amk-ingest") || smtpd.contains("amk-outbound"));

$ sed -n '27,53p' crates/amk-store/src/messages.rs
pub struct NewMessage {
    /// Not yet normalized — folded inside [`insert`], matching [`crate::inboxes`].
    pub inbox_id: InboxId,
    pub message_id: MessageId,
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub thread_id: ThreadId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub attachments: Option<Vec<Attachment>>,
    pub in_reply_to: Option<MessageId>,
    pub references: Option<Vec<MessageId>>,
    pub headers: Option<BTreeMap<String, String>>,
    pub smtp_id: Option<String>,
    pub size: u64,
    pub reply_to: Option<Vec<String>>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub extracted_text: Option<String>,
    pub extracted_html: Option<String>,
}

$ sed -n '33,50p' crates/amk-store/src/threads.rs
pub struct NewThread {
    pub thread_id: ThreadId,
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    /// Not yet normalized — folded inside [`insert`].
    pub inbox_id: InboxId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    pub received_timestamp: Option<Timestamp>,
    pub sent_timestamp: Option<Timestamp>,
    pub senders: Vec<String>,
    pub recipients: Vec<String>,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub last_message_id: MessageId,
    pub message_count: u64,
    pub size: u64,
}

$ ls crates/amk-ingest
ls: cannot access 'crates/amk-ingest': No such file or directory
```

`inboxes::get` requires `organization_id`. There is **no** store lookup by address alone.
This dispatch does **not** add one. RCPT is a crate-local trait plus a **separate**
local-domain allow-list (PLAN.md:252 open-relay is not “lookup returned None”).
Wiring `amkd --role smtpd` is a later dispatch; the existing rejection test stays green.

## The evidence

`[SPEC:docs/PLAN.md]` P2 inbound: own SMTP daemon via `smtp-proto` + `mail-auth` +
`mail-parser`; RCPT only for local verified domains; greet-pause; size limits pre-DATA;
auth-failure → `unauthenticated`. HTTP ingest fallback is a **library entry** — OpenAPI
has no ingest POST, so do not mount a route.
`[SPEC:reference/fixtures/15-compile-spike.txt]` F5/F6/F9 pins:
`smtp-proto = "=0.2.3"`, `mail-parser = "=0.11.6"`, `mail-auth = "=0.12.0"`. Do not bump.
`[SPEC:reference/fixtures/09b-unauthenticated-variant.txt]` SPF=none + no DKIM →
`labels=["received","unread","unauthenticated"]`; list hides it (existing store
predicate); GET-by-id returns the row. SPF hardfail disposition is **unobserved**.
`[SPEC:reference/fixtures/16-threading-matrix/summary.txt]` R1–R5.
`[SPEC:reference/fixtures/21-unbracketed-in-reply-to.txt]` structured `in_reply_to` is
re-bracketed; `headers.In-Reply-To` stays the wire value; matching is `amk-core` C3.
`[SPEC:docs/execute-plan-v1.md]` PR 6/7 file list and the three mutants.

## Writable paths (implementer)

- `crates/amk-ingest/**`
- Workspace `Cargo.toml` — **only** add `"crates/amk-ingest"` to `members` and pin
  `mail-parser = "=0.11.6"` and `smtp-proto = "=0.2.3"` next to the existing `mail-*`
  pins. Do not bump any other pin.

**Not writable:** `crates/amk-types/**`, `crates/amk-core/**`, `crates/amk-store/**`,
`crates/amk-http/**`, `crates/amk-cli/**`, `crates/amk-outbound/**`, `docs/PLAN.md`,
`scripts/**`. Do not flip `amkd --role smtpd`. Do not add `get_by_address`.

## What to build

1. **SMTP state machine** — ingest owns it. `smtp-proto` parses only
   (`Request::<Cow<str>>::parse`). Bind a test-chosen high port (never :25 in tests).
   Sequence: greet-pause → banner → EHLO/HELO → MAIL → RCPT → DATA → persist → 250.
   No AUTH, no STARTTLS, no real mail. Public signatures: `amk-types` + this crate's
   error enum. No `mail_parser::` / `mail_auth::` / `smtp_proto::` in a public signature.

2. **RCPT is two checks, in this order**
   - **Local domain** — constructor arg `local_domains: &[str]` (ASCII-lowercased).
     RCPT whose domain is not in that set → **550**, even if the inbox lookup would
     return `Some`. This is the open-relay test. Deleting this check must 250 a
     `user@gmail.com` whose lookup is stubbed `Some`.
   - **Local inbox** — crate-local async trait (do not put it in `amk-types`) mapping
     `InboxId` → `Option<(OrganizationId, PodId, InboxId)>` via `InboxId::eq_normalized`.
     `None` → **550**. Do not invent a second case-fold; do not resolve PLAN B4 beyond
     using the existing `InboxId` normalisation.

3. **`accept` (library entry = future HTTP ingest fallback, not a route)** and DATA
   persist, **after** parse+auth, via existing `NewMessage` / `NewThread` fields only:
   - Parse with `mail-parser`. Auth with `mail-auth`. SPF=none / no DKIM pass →
     `labels::{RECEIVED, UNREAD, UNAUTHENTICATED}`. A DKIM or SPF **pass** omits
     `unauthenticated`. Do not invent `spam`/`blocked`. SPF hardfail → **STOP**.
   - Thread with `ReferenceChainThreading` + a `ThreadIndex` over this inbox
     (`messages::get`). New → `threads::insert`. Join → same `thread_id` on
     `messages::insert`. Call `threads::record_member` **if the symbol exists**;
     if it does not, do not invent an updater. Join tests assert GET-by-id
     `thread_id`, not aggregates.
   - `message_id` is the RFC 5322 Message-ID. Missing → DATA **554**, store nothing
     (do not mint; G6). Duplicate in the **same** inbox → 554, original unchanged.
     Same id in **another** inbox is that inbox's own thread (16 R4).
   - **C3 persist (fixture 21, not a matching re-implementation):** structured
     `in_reply_to` is the **re-bracketed** Message-ID (`<…@…>`); `headers.In-Reply-To`
     is the **wire** value (bare stays bare). Matching stays in `amk-core`.
   - Empty subject → `None` (16 R5). Trailing subject whitespace stripped (16 R5).
     RFC 2047 encoded-word: 250 and `subject` is `Some` (do **not** mandate
     decode vs raw — unobserved). `to` is the
     header To list; missing To → `to` is `[]`, **not** back-filled from RCPT
     (unobserved). `from` is the header From. Envelope MAIL FROM is used only for
     SPF. Multiple `From:` → 554, store nothing (unobserved winner).
   - `headers` is the received header map (omit `None` when empty). Do not invent
     `Authentication-Results`. `smtp_id` may be `None`; do not invent an SES queue
     id. `preview` / `text` / `html` / `extracted_*` come from the parsed body;
     omit when empty. `timestamp` is `amk-types::Timestamp`. `size` is the DATA
     byte length.
   - CR/LF in any `accept`/parsed **header value** that becomes `from`, a `to`
     element, or `subject` → 554, store nothing (PLAN.md:245, one test per field).

4. **Config the tests inject** — `local_domains`, `max_message_bytes`, `greet_pause`.
   Not product constants, not `amk-http::DEFAULT_MAX_BODY_BYTES`.
   Size: `cap-1` and **`cap` accepted** and stored (`size` matches); **`cap+1`
   rejected** (5xx), store empty. PLAN.md:250 is just-under / just-over;
   execute-plan PR 7 names reject at cap+1.

## Assigned edge cases

Every case names an SMTP reply **or** a store row. Parser `Ok`/`Err` does not
discharge it. HTTP routes are out of this crate.

**SMTP session (loopback, no real mail):**
1. Open relay — `local_domains = ["local.test"]`, lookup stubbed `Some` for
   `alice@gmail.com` → RCPT **550**, store empty. `[SPEC:PLAN.md:252]`
   **This is the test mutant 1 must kill.**
2. Unknown local user — domain is in `local_domains`, lookup `None` → RCPT **550**,
   store empty. `[SPEC:PLAN.md:206]` (unknown-recipient; not the open-relay
   bullet).
3. Pipelined EHLO **before** `greet_pause` → **421**; session never reaches MAIL.
   After the pause, EHLO is 250. `[SPEC:PLAN.md:253]` `[SPEC:PLAN.md:31,190]`
   **Mutant 2 (drop greet-pause) must kill this.**
4. Size `cap-1` and `cap` → 250, stored `size` equals the DATA length. `cap+1` →
   5xx, store empty. `[SPEC:PLAN.md:250]`
5. SPF=none + no DKIM (09b branch 1) → 250; GET-by-id label **membership** is
   `{received, unread, unauthenticated}`. `messages::list` without restricted
   include flags → `count: 0`. `[SPEC:09b]`
   **Mutant 3 (never write `unauthenticated`) must kill this.**
6. Unbracketed parent `In-Reply-To` → GET-by-id `thread_id` equals parent;
   structured `in_reply_to` is `"<parent@…>"`; `headers["In-Reply-To"]` is the
   bare wire value. `[SPEC:21]` `[SPEC:16 R1]`
7. Identical subject, no linkage → different `thread_id` (16 R2). Empty subject
   → `subject` is `None`, not `""`. Subject `Re:` only → stored `Some("Re:")`,
   own thread. 10 KB subject → stored in full. RFC 2047 encoded-word → 250 and
   `subject` is `Some` (decode vs raw unobserved). Homoglyph / “normalizes
   identically”, no linkage → two `thread_id`s.
   `[SPEC:PLAN.md:248]` `[SPEC:16 R2,R5]`
8. In-Reply-To naming nothing this inbox holds → 250, **new** `thread_id`
   (16 R1: join only when the referenced id is in this inbox).
   `[SPEC:PLAN.md:247]` `[SPEC:16 R1]`
9. References that only loop to self → 250, new `thread_id` (core ignores
   self-ref). 500-entry References → 250 in bounded time, a `thread_id` is
   stored (no hang). `[SPEC:PLAN.md:247]`
10. Same Message-ID to a **second** inbox → stored there with that inbox's
    `thread_id`. `[SPEC:PLAN.md:246]` `[SPEC:16 R4]`
11. Missing Message-ID → DATA 554, store empty. Duplicate Message-ID in the
    same inbox → 554, original row unchanged. `[SPEC:PLAN.md:246]`
12. Envelope MAIL FROM ≠ header From: stored `from` is the **header** From;
    labels still follow envelope SPF (09b-style none → `unauthenticated`).
    Missing header To → stored `to` is `[]` (not RCPT). Multiple `From:` →
    554, store empty. `[SPEC:PLAN.md:249]` `[SPEC:09b]`
13. CR/LF in parsed From, To, or Subject **value** (one test each) → 554,
    store empty. `[SPEC:PLAN.md:245]`

**Parse / hostile MIME (SMTP DATA or `accept`, then store):**
14. Unterminated boundary, 8-bit in a header, missing Content-Type, conflicting
    CTE, nested-multipart bomb: **554 + nothing stored**. No “whatever the
    parser produced” arm. `[SPEC:PLAN.md:244]`
15. Attachment filename `../../etc/passwd` or containing NUL → 554, store
    empty (do not sanitize). 200-char name → 250 and `attachments[0].filename`
    is exactly those 200 characters (`Attachment` only; no blob column).
    `[SPEC:PLAN.md:251]`

**Not assigned (STOP if you start them):**
- SPF hardfail disposition (09b branch 2).
- `message.received.unauthenticated` webhook (amk-events).
- Virus/clamd/rspamd, `spam`/`blocked`.
- `amkd --role smtpd`, TLS, AUTH, DSN/ARF (P5).
- Signed download URLs, FTS, C2.
- PLAN.md:250 ≈5.95 MB attachment inline/URL threshold — **blocked**, not
  skipped: that observable is `download_url` / blob store, which this crate
  must not invent. Do not add a case that asserts a URL you cannot mint.

## Mutation (scratch outside the tree; report path, mutant, killed test, `rm -rf`)

- Delete the local-domain RCPT check (accept any domain the lookup maps).
- Drop greet-pause (first-byte EHLO is 250).
- Never write `unauthenticated` (SPF=none row lacks that label).

Must kill named tests for cases **1**, **3**, **5** respectively.

## Prohibitions

- No `mail_parser::` / `mail_auth::` / `smtp_proto::` / `mail_send::` type in a
  public signature.
- No Stalwart or JMAP concept. No new `amk-types` field. No new threading rule.
  No second permissions object. Do not resolve C2. Do not mint Message-IDs.
  Do not back-fill `to` from RCPT. Do not invent `Authentication-Results` or
  an SES `smtp_id`.
- Do not send real mail. Do not listen on :25 in a test.
- If the contract is ambiguous or appears wrong, **STOP and report**.

## Reporting

- `cargo test -p amk-ingest` and `./scripts/check.sh` (fail if the DB-skip
  warning is printed). Persist tests need Postgres on `127.0.0.1:55432`.
- `./scripts/shape-provenance.sh` — `amk-ingest` clean on the boundary-type check.
- Mutation report as specified above.
