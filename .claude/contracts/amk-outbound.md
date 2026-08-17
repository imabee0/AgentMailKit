# Contract — amk-outbound: DKIM-signed send, reply, reply-all and forward

Scope-derivation: the four operations are what `openapi.json` describes on the send paths, and the
types they need are what `amk-types` already carries. Both enumerated, not recalled.

```
$ python3 -c "…openapi paths matching /messages/send|/reply|/forward…"
  POST /v0/inboxes/{inbox_id}/messages/send                        -> send
  POST /v0/inboxes/{inbox_id}/messages/{message_id}/reply          -> reply
  POST /v0/inboxes/{inbox_id}/messages/{message_id}/reply-all      -> reply-all
  POST /v0/inboxes/{inbox_id}/messages/{message_id}/forward        -> forward

$ grep -n "pub struct SendMessageRequest" -A 22 crates/amk-types/src/message.rs
  to, cc, bcc, reply_to, subject, text, html, labels, attachments, headers   # all present
$ grep -n "pub struct SendMessageResponse" -A 4 crates/amk-types/src/message.rs
  { message_id: MessageId, thread_id: ThreadId }                             # present

$ grep -rn "mail-send\|mail-builder\|mail-auth" crates/*/Cargo.toml
  (only the boundary comments forbidding them in amk-types/core/store/http)
```

**No type is added by this dispatch.** If a field seems missing, STOP and report.

## The evidence

`[SPEC:reference/fixtures/15-compile-spike.txt]` — the pinned versions the workspace resolved and
compiled against: `mail-send = "0.6"`, `mail-builder = "0.4"`, `mail-auth = "0.12"`. Use those, and
do not bump them here; a version change is a workspace decision with its own evidence.

`[SPEC:reference/fixtures/10-dkim-keys.txt]` and `10b-dkim-extraction.txt` — the DKIM key material
and how it is obtained. **`mail-auth` wants DER**, not PEM: this is written in `CLAUDE.md`'s
contract-facts list because it has already cost time once.

`[SPEC:reference/fixtures/21-unbracketed-in-reply-to.txt]` — an unbracketed `In-Reply-To` DOES join
the bracketed message's thread, because the reference re-brackets the parsed value before matching
while leaving `headers.In-Reply-To` bare. `amk-core::threading` implements this (register C3); the
send path must produce linkage headers that path can match, and must not re-derive the rule.

`[SPEC:reference/fixtures/03-id-formats.http]` — `message_id` **is** the RFC 5322 Message-ID. A
sent message's id is the one that went on the wire, not a synthesised surrogate.

`[SPEC:reference/openapi.json]` — `SendMessageResponse` is `{message_id, thread_id}` only.

## Writable paths

- `crates/amk-outbound/**` — NEW crate.
- `Cargo.toml` — the workspace member entry and the three pinned dependencies only.
- `crates/amk-http/src/handlers/messages.rs` — the four handlers.
- `crates/amk-http/src/lib.rs` — the four `.route()` calls only.
- `crates/amk-http/Cargo.toml` — the `amk-outbound` dependency only.
- `crates/amk-http/tests/` — the tests.

Nothing else. In particular **not** `crates/amk-types/**` (frozen), **not** `crates/amk-core/**`
(threading and labels are the rules and are already written), not `crates/amk-store/**` beyond
calling its existing `messages::insert`/`threads::insert`, not `scripts/**`, not the plan.

## The boundary rule that governs this crate

`mail-send`, `mail-builder`, `mail-auth` and `smtp-proto` are **stalwart-labs crates consumed as
libraries** — one of the plan's two sanctioned roles for Stalwart. They live **inside**
`amk-outbound` and are converted at its edge: **no `mail_send::`/`mail_builder::`/`mail_auth::`/
`smtp_proto::` type may appear in any public signature or re-export** of this crate, and
`./scripts/shape-provenance.sh` is the check. `amk-outbound`'s public API speaks `amk-types` only.

## What to build

1. **Message construction** — `mail-builder` assembles the MIME from `SendMessageRequest`. The
   `From` is the sending inbox; `Message-ID` is generated once and is the id returned.
2. **DKIM signing** — `mail-auth`, DER keys, selector and domain from configuration. A send with no
   configured key for the domain **fails closed** with an internal error; it never sends unsigned.
   `amk-http`'s `AppConfig` fail-closed defaults are the precedent.
3. **Delivery** — `mail-send`, direct-to-MX with a smarthost option, both configurable.
4. **Persistence** — the sent message is stored through `amk-store::messages::insert` with the
   `sent` label, and threaded through `amk-core::threading` exactly as inbound mail is. Reply and
   reply-all set `In-Reply-To`/`References` from the parent so the thread matches; forward does not.
5. **reply-all recipient derivation** — from the parent's `from`/`to`/`cc`, minus the sending inbox
   itself. `[INFERRED]` unless a fixture is captured: mark it, and say so in the report.

## Assigned edge cases

1. A send with no configured DKIM key for the domain fails closed and stores nothing.
2. `reply` sets `In-Reply-To` to the parent's Message-ID and lands in the parent's thread —
   asserted by reading the thread back, not by inspecting the header alone.
3. `reply` to a parent whose stored `In-Reply-To` is **unbracketed** still joins the right thread
   (fixture 21 / register C3).
4. `reply-all` excludes the sending inbox from the derived recipients, and de-duplicates.
5. `forward` starts a NEW thread; assert the returned `thread_id` differs from the parent's.
6. A hostile `headers` map cannot inject a second `From`, a `Bcc` that leaks, or a header
   containing CR/LF — one test per injection vector, each asserting the assembled MIME.
7. Sending to an address that is itself a local inbox does not short-circuit into a direct store
   write that skips DKIM and the outbound path.
8. Boundary and one unit either side on attachment size, per the plan's testing rules, around the
   ~5.95 MB inline threshold `[SPEC:repo agentmail-toolkit]`.

## Prohibitions

- No stalwart-labs type in any public signature or re-export. `shape-provenance.sh` enforces it.
- No Stalwart or JMAP concept, field or name anywhere.
- No new type in `amk-types`; no new rule in `amk-core::threading` — if threading seems wrong,
  **STOP and report**, because register C3 already reversed it once.
- Do not send real mail from a test. Delivery is behind a trait; tests use a recording fake. The
  live send is P2's **R-phys** gate half, run from the OVH box, not from `cargo test`.
- Do not implement drafts or `send_at` scheduling — P3.
- Do not resolve register **C2**.
- If the contract is ambiguous or appears wrong, **STOP and report**.

## Reporting

Report the command run and its actual output; "tests pass" without the output is not a report.

- `cargo test -p amk-outbound -p amk-http` and `./scripts/check.sh`.
- `./scripts/shape-provenance.sh`, showing `amk-outbound` clean on the boundary-type check.
- `./scripts/derive-implemented-paths.sh`, showing the mounted set grown by exactly four operations
  and still reconciling against `openapi.json`.
- A mutation pass in **both directions** on a scratch copy outside the tree: remove the DKIM
  signing step (must kill a test) and make the header sanitiser a pass-through (must also kill a
  test). Delete the scratch copy and confirm it.
