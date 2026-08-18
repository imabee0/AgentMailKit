# AgentMailKit — remaining V1 as an execute-plan DAG

Derived from `docs/PLAN.md` and `docs/RESUME.md`. Not a second plan: `docs/PLAN.md` stays the
contract. This file is the parseable PR DAG for `/execute-plan`. If the two disagree, PLAN.md wins
and this file is amended by the orchestrator.

## Goal

Finish V1 in PLAN.md write order without skipping a gate or a listed process. execute-plan launches
worktrees and keeps state. Dispatch, review, branch names, and merge follow PLAN.md.

## Current position

P0 gated (fixture 24). P1 Lane L green; Lane R (dual-target conformance) **not run**;
`CURRENT_PHASE=P0`. P2 message/thread surface landed; `amk-outbound` has signing/assemble/build;
SMTP `Transport` and the four send HTTP endpoints are not wired; `amk-ingest` does not exist.
P3–P6 not started.

## Key Decisions

1. **Gates are nodes.** A gate PR produces a fixture (or an explicit **not run**). It does not
   write product code. “Not run” is never recorded as passed. `CURRENT_PHASE` moves only when the
   current phase is gated.
2. **Lane R is not a parent of the next phase’s code.** PLAN.md allows the next phase’s *code* at
   Lane L code-complete. R-key / R-phys nodes stay in the DAG and stay blocked until their input
   exists (read-only AgentMail key via `sdxd`; OVH + Gmail for P2 R-phys).
3. **Crate write order is the dependency graph.** types → core → store → http → ingest + outbound
   (fan-out only if all four predicates hold) → events + jobs → dns + mcp + reply-extract → import
   last, P6 only.
4. **Fan-out predicates (all four):** disjoint files; neither crate depends on the other; both
   depend only on merged crates; `amk-types` frozen. Ceiling 2. `.claude/fanout.lock` on for the
   duration.
5. **Dispatch order is load-bearing.** Contract into the worktree first, then `.amk-scope`, then
   the lock. Contract reviewed read-only before implement. Scope is derived (command + output on
   `Scope-derivation:`).
6. **Merge branch is `amk/<phase>/<crate>`.** execute-plan’s `execute-plan/<id>-pr-N-…` label is
   the worktree only. Orchestrator publishes `amk/<phase>/<crate>` from `commit_sha` before review.
   Merge that branch after three clean lenses. Never merge-commit; rebase onto `main` first.
7. **Three lenses on every returned diff** — `amk-review-contract`, `amk-review-provenance`,
   `amk-review-tests` — plus the pre-dispatch contract lens. execute-plan’s single generic reviewer
   is extra, not a substitute.
8. **Orchestrator writes no implementation except `amk-types`.** No invented shapes. Mutation both
   directions on new guards. Report the command and its output. `./scripts/check.sh` — read the
   DB-skip line before believing PASS.
9. **No permission bypass.** No `--always-approve` / `--yolo`. Restore
   `~/.grok/requirements.toml` `disable_bypass_permissions_mode = true` if a restart deleted it.
10. **Harness on `main` before any worktree is cut from `origin/main`.** GitHub PRs #5–#9 are the
    harness stack. Until they land, new worktrees branch from the stack tip
    `execute-plan/e957ddfb-pr-5-…`, not from `origin/main`.

## Binding instructions (injected into every child)

- Follow `.claude/contracts/<crate>.md` and the `[SPEC:*]` citations. If a needed type is not in
  `amk-types` or a fixture, STOP and report.
- Writable paths are the contract’s list only.
- Never edit `docs/PLAN.md`, `scripts/hooks/**`, or `crates/amk-types/**`.
- Mutation both directions on a scratch copy **outside** the worktree. The report names the
  scratch path, each mutant as one source line, the test that died, and the `rm -rf` output
  (`PLAN.md` 319–321). Widen (`is_some_and(pred)` → `is_some()`) as well as delete.
- Reviewers are the three named lenses, read-only. All three must be clean. One lens is not a panel.
- Report the command and its actual output. “Tests pass” is not a report.

## Gate catalog

| ID | Gate | Lane | Fixture + MET line | Advances |
|---|---|---|---|---|
| G1 | P1 dual-target | R-key | `reference/fixtures/25-p1-gate-conformance.txt` must contain `0 skipped, 0 with structural diffs`, `THIRD RUN — CLEAN`, `dual_target.py exit: 0` | `CURRENT_PHASE` off P0 only when every P1 conjunct is MET |
| G2 | P2 Lane L | L | `reference/fixtures/28-p2-lane-l.txt`: `check.sh` with **no** DB-skip warning; `schemathesis exit: 0` via `scripts/p1-gate.sh` Lane L invocations (custom checks, not bare `st run`); both `sdk_smoke.* exit: 0` with at least one send and one inbound `thread_id` match; `derive-implemented-paths` +4 send ops; mutation pass named in the fixture | P2 code-complete; P3 code may start |
| G3 | P2 conformance | R-key | `reference/fixtures/29-p2-conformance.txt`. Manifest **must gain** the four send POSTs (or the PR states why POSTs against the live account are forbidden **and** names the substitute that still diffs those shapes). Re-running the P1 GET-only manifest is **not** MET. `dual_target.py exit: 0` | P2 gated (with G4) |
| G4 | P2 inject + Gmail | R-phys | `reference/fixtures/30-p2-r-phys.txt`: 3-message `/root/amksend.py` thread + Gmail DKIM/SPF pass | P2 gated (with G3) |
| G5 | P3 Lane L | L | `reference/fixtures/31-p3-lane-l.txt` + mutation; cases in PR 15 | P3 code-complete |
| G6 | P3 R-key | R-key | `reference/fixtures/32-p3-conformance.txt`; manifest gains draft/idempotency ops; not the P1 GET list | P3 gated (with G5) |
| G7 | P4 Lane L | L | `reference/fixtures/33-p4-lane-l.txt`: official svix lib accept **and** forged/truncated reject; WS `message.received`; spam variant XOR `received`; mutation | P4 code-complete |
| G8 | P4 R-key | R-key | `reference/fixtures/34-p4-conformance.txt`; manifest gains webhook CRUD | P4 gated (with G7) |
| G9 | P5 Lane L | L | `reference/fixtures/C1-domain-shape.txt` + `35-p5-lane-l.txt`; extra or omitted field fails | P5 code-complete |
| G10 | P5 R-phys | R-phys | `reference/fixtures/36-p5-r-phys.txt` | P5 gated (with G12) |
| G11 | P6 | R-phys | `reference/fixtures/38-p6-cutover.txt` | V1 acceptance |
| G12 | P5 dual-target | R-key | `reference/fixtures/37-p5-conformance.txt`; domain reads, D1-constrained; not the P1 GET list | P5 gated (with G10) |

## PR Plan

### PR 1: Land the harness stack on `main`

- **Description:** Merge GitHub PRs #5–#9 in stack order. Human merges. Orchestrator does not push
  `main`. After merge: delete `execute-plan/e957ddfb-*` branches and leftover worktrees. Confirm
  `./scripts/hooks/guard.test.sh` is 49/49 on `main` and `.grok/hooks/amk-harness.json` exists.
- **Files/components affected:** merge only
- **Dependencies:** None

### PR 2: This file (`docs/execute-plan-v1.md`)

- **Description:** The DAG. Already this document. Do not edit `docs/PLAN.md`.
- **Files/components affected:** `docs/execute-plan-v1.md`
- **Dependencies:** None (authored on the harness tip so later worktrees see it)

### PR 3: Re-derive the outbound remainder contract

- **Description:** Re-run the derivation in `.claude/contracts/amk-outbound.md` against the
  post-harness `main` (or this tip until PR 1 lands). Record command + output on
  `Scope-derivation:`. Remaining work is SMTP `Transport` (direct-to-MX + smarthost) and the four
  HTTP operations, persist via `amk-store::messages::insert`, thread via `amk-core::threading`. No
  new types. Read-only contract lens before any implementer. Orchestrator amends the contract only
  if derivation drifted.
- **Files/components affected:** `.claude/contracts/amk-outbound.md` (only if derivation requires)
- **Dependencies:** PR 2

### PR 4: `amk-outbound` SMTP Transport

- **Description:** Implement `mail-send` behind the existing `Transport` trait. Direct-to-MX and
  smarthost, both configurable. Fail closed with no signing key (`OutboundError::NoSigningKey`,
  no `SignedMessage` on the fake). Public signatures `amk-types` only. Tests use a recording fake
  — no real mail. Branch `amk/p2/outbound`. Three lenses. `./scripts/check.sh` +
  `shape-provenance.sh` in the report. Contract: `.claude/contracts/amk-outbound.md`. Specs:
  fixtures 15, 10/10b. **Assigned:** contract case 1 *sign-side only* (no key → no signed
  message). Store-side “stores nothing” and case 7 (no local-inbox short-circuit) belong to PR 5.
  Mutation: remove DKIM signing; passthrough `check_headers` — each must kill a named test.
- **Files/components affected:** `crates/amk-outbound/**`
- **Dependencies:** PR 3

### PR 5: `amk-outbound` HTTP send / reply / reply-all / forward

- **Description:** Mount the four operations. `AppState` carries `Keyring` + `Transport`. Persist
  through existing `messages::insert`; thread through existing `amk-core::threading`. Same branch
  `amk/p2/outbound`. Three lenses. No `amk-types` edits. `derive-implemented-paths` grows by
  exactly four. MIME-only unit tests do **not** discharge HTTP cases.
  **Assigned (HTTP integration, store/thread observables):**
  1. No-key send: fail-closed error **and** `messages::get`/list empty (case 1 store half).
  2. `reply` GET the thread: parent membership, same `thread_id` (not header-only).
  3. Unbracketed parent `In-Reply-To` still joins (fixture 21 / C3), via GET thread.
  4. `reply-all` excludes sending inbox, de-duplicates.
  5. `forward` returned `thread_id` ≠ parent.
  6. Hostile `headers` map (From, Bcc, CR/LF) plus CR/LF in `to` and `subject` (PLAN.md:246).
  7. Send to a local inbox still goes through `Transport` (recording fake has one `SignedMessage`)
     and stored raw carries `DKIM-Signature`.
  8. Attachment size cap−1 accepted; cap and cap+1 rejected or URL-threshold per toolkit.
  Mutation: persist-on-error (insert then ignore `NoSigningKey`); mint a new `thread_id` on reply;
  copy `headers` onto MIME after `build_signed`. Each must kill a named HTTP test.
- **Files/components affected:** `crates/amk-http/src/handlers/messages.rs`,
  `crates/amk-http/src/lib.rs`, `crates/amk-http/Cargo.toml`, `crates/amk-http/tests/**`,
  `crates/amk-outbound/**` as needed to expose Transport
- **Dependencies:** PR 4

### PR 6: Derive and review the `amk-ingest` contract

- **Description:** New contract. First line is `Scope-derivation:` plus the enumeration command and
  its raw output. Sites from PLAN.md P2 ingest and fixtures 09b, 16, 21, 15. `smtp-proto` is
  parser-only; ingest owns the state machine. **Required section `Assigned edge cases`** quotes
  PLAN.md 243–253 plus fixture 09b (`unauthenticated` label, list exclusion is a storage
  predicate). Cases must name SMTP/store observables (RCPT 550, labels on the stored row, size
  reject at cap+1), not parser-internal Ok/Err. Three-lens review of the **contract** before
  dispatch. No product code. Orchestrator writes the contract.
- **Files/components affected:** `.claude/contracts/amk-ingest.md`
- **Dependencies:** PR 2

### PR 7: `amk-ingest` crate

- **Description:** Implement the reviewed ingest contract only. Fan-out with PR 4/5 only if all
  four fan-out predicates hold and `fanout.lock` is on. Branch `amk/p2/ingest`. Three lenses. Stop
  if a type is missing. Do not resolve C2. **Assigned:** every case in
  `.claude/contracts/amk-ingest.md` Assigned edge cases (PLAN.md 243–253 + fixture 09b). Tests
  assert RCPT 550 for non-local, greet-pause before pipelined EHLO, `unauthenticated` on the
  stored row for SPF=none, size reject at cap+1. Parser Ok/Err is not enough.
  Mutation: delete local-domain RCPT check; drop greet-pause; never write `unauthenticated`. Each
  must kill a named test.
- **Files/components affected:** `crates/amk-ingest/**`, workspace `Cargo.toml` member line only
- **Dependencies:** PR 6

### PR 8: P2 Lane L gate (G2)

- **Description:** Produce `reference/fixtures/28-p2-lane-l.txt`. Fail the PR if `check.sh` prints
  the DB-skip warning. Run the Lane L invocations from `scripts/p1-gate.sh` (custom schemathesis
  checks, not bare `st run`; `sdk_smoke.py` / `sdk_smoke.mjs` at `AMK_BASE=http://127.0.0.1:8111`).
  Fixture must contain `schemathesis exit: 0`, both `sdk_smoke.* exit: 0`,
  `derive-implemented-paths` showing +4 send operations, one SDK send and one injected inbound
  with matching `thread_id`, and the mutation report (path, mutants, killed tests, `rm -rf`).
  Do **not** advance `CURRENT_PHASE`. If a conjunct fails, this PR fails; P3 does not start.
- **Files/components affected:** `reference/fixtures/` (P2 Lane L transcript), `docs/RESUME.md`
  (one status paragraph)
- **Dependencies:** PR 5, PR 7

### PR 9: P1 Lane R gate (G1)

- **Description:** Independent of PR 4–8. Command (keys by reference, never inline):
  `AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c 'REF_KEY="$AGENTMAIL_API_KEY" python3 conformance/dual_target.py conformance/manifest.json'`.
  MET only if `reference/fixtures/25-p1-gate-conformance.txt` contains `0 skipped, 0 with
  structural diffs`, `THIRD RUN — CLEAN`, and `dual_target.py exit: 0`. Else **not run**. Leave
  `CURRENT_PHASE=P0` unless every P1 conjunct is MET.
- **Files/components affected:** `reference/fixtures/25-p1-gate-conformance.txt`,
  `scripts/plan-ledger.sh` (`CURRENT_PHASE` only when actually gated)
- **Dependencies:** PR 1

### PR 10: P2 R-key gate (G3)

- **Description:** `reference/fixtures/29-p2-conformance.txt`. Manifest **must list** the four
  send POSTs (or this PR states live POSTs are forbidden and names the shape-diff substitute).
  Re-running the P1 18-GET + `DELETE /v0/auth/me` manifest is **not** MET. **Not run** without the
  key. Does not unblock P3 code (G2 does). With G4, and only then, P2 is gated and
  `CURRENT_PHASE` may move. `scripts/plan-ledger.sh` only on that move.
- **Files/components affected:** `reference/fixtures/29-p2-conformance.txt`,
  `conformance/manifest.json`
- **Dependencies:** PR 8

### PR 11: P2 R-phys gate (G4)

- **Description:** Mail injected from the OVH box via `/root/amksend.py` appears with correct
  threading over a 3-message exchange; SDK send to a Gmail account shows DKIM+SPF pass. **Not run**
  without the box and Gmail. Does not unblock P3 code. With G3, and only then, P2 is gated and
  `CURRENT_PHASE` may move.
- **Files/components affected:** `reference/fixtures/` (P2 R-phys transcript)
- **Dependencies:** PR 8

### PR 12: `amk-events`

- **Description:** Webhooks CRUD (3 scopes, write-only headers), Svix-wire delivery + retries
  (full 8-attempt schedule, fixture 07), inbox events. Contract first. Branch `amk/p4/events`
  after G2. **Assigned (PLAN.md 260–264):** 3xx → failure; 15s hang → fail; `svix-id` stable;
  exhaustion → `message.attempt.exhausted`; 5-day disable + `EndpointDisabledEvent`; opt-in spam
  **replaces** `message.received` (assert `received` is NOT also delivered); webhook SSRF matrix;
  WS subscribe to an unseen inbox / unauthorized `event_type`.
- **Files/components affected:** `crates/amk-events/**` (after a reviewed contract)
- **Dependencies:** PR 8

### PR 13: `amk-jobs`

- **Description:** Postgres `jobs` table + tokio workers. Fan-out with PR 12 if predicates hold.
  Branch `amk/p3/jobs` when P3 is open (drafts/`send_at` need it). Contract first.
  **Assigned (PLAN.md:274):** worker crash mid-send → no double-send on restart (SKIP LOCKED
  tested, not assumed).
- **Files/components affected:** `crates/amk-jobs/**`
- **Dependencies:** PR 8

### PR 14: P3 drafts, scheduling, idempotency

- **Description:** Drafts CRUD/modes/references, `send_at` jobs, `Idempotency-Key` layer.
  **Assigned (PLAN.md 240–241, 255–258, 267–268):** same key + same body → original; same key +
  different body → 409; empty key → 400; key after 24h TTL; **concurrent** identical keys must not
  double-send; first-attempt-failed retryable; `client_id` replay; `send_at` past / DST / naive;
  url-attachment SSRF matrix (127.0.0.1, link-local, private, DNS-to-private, redirect-to-private,
  unbounded stream, hang, DNS rebinding / pin holds). Gate G5 after. No types invented.
- **Files/components affected:** per a reviewed P3 contract
- **Dependencies:** PR 8, PR 13

### PR 15: P3 Lane L gate (G5)

- **Description:** `reference/fixtures/31-p3-lane-l.txt`. Must include PR 14’s concurrent
  identical-key case (two POSTs, one send), empty key → 400, and mutation. Sequential duplicate →
  same body and mismatch → 409 are not enough. Do **not** advance `CURRENT_PHASE`. This is the
  only P3 node that unblocks P4 *code*.
- **Files/components affected:** `reference/fixtures/` (P3 Lane L transcript), `docs/RESUME.md`
- **Dependencies:** PR 14

### PR 16: P3 R-key gate (G6)

- **Description:** `reference/fixtures/32-p3-conformance.txt` must contain `dual_target.py exit: 0`.
  Manifest gains draft/idempotency ops; P1 GET list is not MET. **Not run** without the key.
  Sibling of PR 15. Does not parent P4 code. Required (with G5) before P3 gated / `CURRENT_PHASE`.
- **Files/components affected:** `reference/fixtures/32-p3-conformance.txt`,
  `conformance/manifest.json`
- **Dependencies:** PR 14

### PR 17: `amk-dns` + `amk-mcp` + `reply-extract`

- **Description:** Leaf crates. Fan-out (ceiling 2, then the third). Each has its own derived
  contract. MCP connector Gate is later. Reply extract may stay degraded (`extracted_*` = full
  body) until the Talon port exists.
- **Files/components affected:** `crates/amk-dns/**`, `crates/amk-mcp/**`, `crates/reply-extract/**`
- **Dependencies:** PR 12, PR 13

### PR 18: P4 events HTTP/WS surface

- **Description:** WS hub, metrics, opt-in spam events replace `received`. Product only. Depends
  on the events crate and on P3 **Lane L** (G5), not on G6.
- **Files/components affected:** `crates/amk-http/**` event mounts, `crates/amk-events/**`
- **Dependencies:** PR 12, PR 15

### PR 19: P4 Lane L gate (G7)

- **Description:** `reference/fixtures/33-p4-lane-l.txt`. Official lib **accepts** a real
  signature **and rejects** forged/truncated. WS receives `message.received`. Opt-in spam
  delivered XOR `message.received`. Mutation. Do **not** advance `CURRENT_PHASE`. Unblocks P5
  *code*.
- **Files/components affected:** `reference/fixtures/` (P4 Lane L transcript)
- **Dependencies:** PR 18

### PR 20: P4 R-key gate (G8)

- **Description:** `reference/fixtures/34-p4-conformance.txt` must contain `dual_target.py exit: 0`.
  Manifest gains webhook CRUD. **Not run** without the key. Sibling of PR 19. Does not parent P5
  code. With G7, then P4 gated / `CURRENT_PHASE`. `scripts/plan-ledger.sh` only on that move.
- **Files/components affected:** `reference/fixtures/34-p4-conformance.txt`,
  `conformance/manifest.json`
- **Dependencies:** PR 18

### PR 21: P5 domains product

- **Description:** Domain CRUD, DNS verify, DKIM keygen/import, `feedback_enabled`. Product only.
  Depends on P4 Lane L (G7) and leaf crates, not on G8/G10/G12. D1 still blocks probing production
  domains.
- **Files/components affected:** per a reviewed domain contract
- **Dependencies:** PR 17, PR 19

### PR 22: P5 Lane L gate (G9)

- **Description:** `reference/fixtures/35-p5-lane-l.txt` plus `C1-domain-shape.txt`. Any extra or
  omitted field fails. Mutation both directions on the domain-shape compare (drop a fixture field
  / add an invented one — each must kill the gate). Do **not** advance `CURRENT_PHASE`. Unblocks
  the import mapping table.
- **Files/components affected:** `reference/fixtures/35-p5-lane-l.txt`
- **Dependencies:** PR 21

### PR 23: P5 R-phys gate (G10)

- **Description:** `reference/fixtures/36-p5-r-phys.txt` must contain a 200 on the verified
  domain GET and a `message.bounced` event id after the induced bounce. **Not run** without
  R-phys. Sibling of G9/G12. Does not parent import-table work.
- **Files/components affected:** `reference/fixtures/36-p5-r-phys.txt`
- **Dependencies:** PR 21

### PR 24: P5 R-key gate (G12)

- **Description:** `reference/fixtures/37-p5-conformance.txt` must contain `dual_target.py exit: 0`.
  Domain reads only, D1-constrained. **Not run** without the key. Sibling of G9/G10. With G10,
  then P5 gated / `CURRENT_PHASE`.
- **Files/components affected:** `reference/fixtures/37-p5-conformance.txt`,
  `conformance/manifest.json`
- **Dependencies:** PR 21

### PR 25: Import mapping table (P6, before any import code)

- **Description:** Write and review the Stalwart→AgentMail mapping table from PLAN.md. Any
  “keep Stalwart’s version” row is a defect. No `amk-import` code. Depends on P5 **Lane L** (G9).
- **Files/components affected:** `.claude/contracts/amk-import.md`
- **Dependencies:** PR 22

### PR 26: `amk-import` product

- **Description:** LAST product crate. Translation boundary only. Deletable after cutover. Not
  started before PR 25 is reviewed. No cutover in this PR. **Assigned:** every DROPPED mapping-table
  row is absent from store; our threading wins (Stalwart threads not imported); re-import is
  idempotent.
- **Files/components affected:** `crates/amk-import/**`
- **Dependencies:** PR 25

### PR 27: P6 cutover gate (G11)

- **Description:** `reference/fixtures/38-p6-cutover.txt` must contain restore-drill exit 0,
  outside banner from `.64`, `replicas: 0` on Stalwart, and `dns-health.py` green. Existence of
  the file is not MET. **Not run** without the cluster. Does not write import code.
- **Files/components affected:** `deploy/k3s/**` (cutover manifests only),
  `reference/fixtures/38-p6-cutover.txt`
- **Dependencies:** PR 26
