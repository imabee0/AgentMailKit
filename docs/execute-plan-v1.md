# AgentMailKit — remaining V1 as an execute-plan DAG

Derived from `docs/PLAN.md` and `docs/RESUME.md`. Not a second plan: `docs/PLAN.md` stays the
contract. This file is the parseable PR DAG for `/execute-plan`. If the two disagree, PLAN.md wins
and this file is amended by the orchestrator.

## Goal

Finish V1 in PLAN.md write order without skipping a gate or a listed process. execute-plan launches
worktrees and keeps state. Dispatch, review, branch names, and merge follow PLAN.md.

## Current position

**Derived, not transcribed.** Run `./scripts/plan-ledger.sh | head -1` for the phase and
`./scripts/derive-implemented-paths.sh` for the mounted surface.

The paragraph that used to sit here was wrong in two directions at once by 2026-08-19: it said
`amk-ingest` does not exist (it merged at `28e6afa`) and that P1 Lane R was not run (fixture 25
records `THIRD RUN — CLEAN` with `0 skipped, 0 with structural diffs`). Both were stale rather than
mistaken when written, which is the failure mode a hand-maintained status paragraph has by
construction. It is not replaced with a fresher one.

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

**REGENERATED 2026-08-19.** PRs 1–8 and the harness stack are done and merged; the list below is
what is actually left. Nothing here transcribes a count — run the two derivation scripts.

```bash
./scripts/derive-remaining-surface.sh            # per-resource: mounted / total / remaining
./scripts/derive-remaining-surface.sh missing    # every operation not yet mounted
./scripts/plan-ledger.sh | head -1               # the phase, from the gate transcripts
```

### DONE since the last revision of this file

W1 is complete. `binary-smoke.sh` is the gate that made most of it findable — it starts the release
binaries from a production-shaped environment and asserts on what leaves the process, which is the
one thing no other gate did.

| | Outcome |
|---|---|
| **D1** | `amkd` could not send mail at all: `AppState::new` built an empty `Keyring` and no env var could inject one. Fixed; `AMK_DKIM_KEYS` / `AMK_SMTP_SMARTHOST`. |
| **rustls panic** | Every outbound send panicked — two crypto providers in the graph, none default. Fixed at boot and behind a `Once`. |
| **Observability** | `tracing` (JSON on a pipe, human on a TTY), request ids, `/health`, `/ready`, `/metrics`. Was zero call sites workspace-wide. |
| **Limits** | Pagination clamp, SMTP connection cap + session deadline, per-key/per-IP rate buckets with a 21× auth-failure surcharge. |
| **STARTTLS** | Inbound was plaintext unconditionally. Opportunistic TLS, RFC 3207 §4.2 verified against the running binary. |
| **CI** | Four workflows, affected-crate closure, build-once/promote-by-digest, `ci-ok` as the single required check. The no-CI decision is reversed and recorded. |
| **C2** | Closed by decision. Register A and C are now empty of open items. |

### The work that remains, in dependency order

Each item: one branch `amk/<phase>/<crate>`, a contract with a derived `Scope-derivation:`, three
read-only lenses, mutation both directions, **a line in `binary-smoke.sh`**, and a stated
`derive-remaining-surface.sh` delta. The last two are not optional — a capability not observed
through the shipped binary is not a capability, which is the lesson D1 cost.

**PR A — blobs, attachments, raw, signed URLs.**
Deferred by decision at P1 and now blocking: `messages/{id}/raw`, `messages/{id}/attachments/{id}`
and the draft-attachment reads all need content-addressed storage behind `amk-store`'s blob trait.
`reference/fixtures/06-download-url-expiry.txt` is the spec — a CloudFront-style signed URL with a
~1h TTL and a 403 after expiry. Nothing else in the queue needs blobs, but every remaining message
endpoint does.

**PR B — `amk-jobs`.**
`amkd --role worker` refuses to start and names this crate. Postgres jobs table + tokio workers.
Prerequisite for C and D; nothing else unblocks scheduled sends or webhook retries.

**PR C — `amk-events`** (7 webhook operations + inbox events).
Svix-wire compatible. `reference/fixtures/07-webhook-retry-curve.txt` proved the retry schedule is
**not** truncated at 5 attempts — a 6th fired on two chains — so all 8 land, with
`message.attempt.exhausted` and the 5-day auto-disable. `09-event-payloads.txt` and
`17-message-complained.txt` are the payload shapes. Needs PR B.

**PR D — drafts, scheduling, `Idempotency-Key`** (3 draft operations + the inbox-mounted set).
`send_at` needs PR B. Idempotency is a tower layer, not a handler concern.

**PR E — `amk-dns` + domains** (7 operations).
`reference/fixtures/C1-domain-shape.txt` is the authority and the gate is exact: any field we emit
that is not in that fixture, or any fixture field we omit, is a conformance failure rather than a
judgement call. hickory for verification, zone-file export, DKIM keygen.

**PR F — search + FTS.**
`messages/search` at every mount. Parked as "post-V1" in PLAN.md's full-parity list, which is wrong
for a 1:1 clone: a client that calls search against us gets a 404 where the reference returns
results. Note the contract fact — search does **not** hide restricted-label mail, unlike the list
endpoints.

**PR G — lists** (4 operations, allow/block).
Small, self-contained. `send`-direction enforcement is 403 `message_rejected`.

**PR H — `amk-mcp` + `reply-extract`.**
MCP is named in this project's own headline claim — "SDKs, CLI **and MCP bridge** work by changing
only the base URL" — while sitting in the parked list. Either it ships in V1 or the claim changes;
it should ship.

**Deliberately NOT queued:** `agent/sign-up` and `agent/verify` (config-gated, off by default, and
they imply an OTP surface this deployment does not want), and `amk-import` (migration, not product,
and P6 only).

### Gate catalog

Unchanged in shape from the previous revision; `plan-ledger.sh` reports each as MET, STALE or
PENDING and is the authority. Note that every P0–P2 transcript currently reads **STALE**, because
the crates they cover have changed since they were captured — re-running them is the first Lane R
work, not new development.
