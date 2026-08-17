# Resume here

Where the last session stopped, so a fresh one — on this workstation or in Claude's cloud sandbox —
can continue without re-deriving it. Update this file in the commit that invalidates it.

**Last updated:** 2026-08-17, at the GitHub migration.

## Verified state

```
$ ./scripts/check.sh
check: PASS
plan-ledger: PASS
$ ./scripts/check.sh --fast   # summed test results
total passed: 570
```

`main` is green at 570 workspace tests with a live Postgres on 127.0.0.1:55432. Without that
database the same command still exits PASS having skipped every DB-backed test — see the sandbox
section of `CLAUDE.md`.

## Phase position

**P0: closed.** `amk-types`, `amk-core`, `amk-store`, `amk-http`, `amk-cli` merged, review-panelled
and mutation-verified. Gate met (fixture 24).

**P1: gate conjuncts recorded MET, with one known open divergence.** The ledger reads
`p1-gate-conformance` MET (fixture 25) and `p1-gate-sdk-smoke` MET (fixture 26 — a clean run *and*
a falsification proving failure propagates). `./scripts/p1-gate.sh` is the four-conjunct runner:
dual-target conformance diff, Python SDK smoke, Node SDK smoke, schemathesis over the 25 mounted
operations.

`scripts/plan-ledger.sh` still reads `CURRENT_PHASE=P0`. **Do not advance it to P1 until the open
item below is closed** — the schemathesis half is what found that divergence, so declaring P1 met
while it stands would record a gate that its own evidence contradicts.

## The one open work item

**axum extractor rejections escape our JSON error contract.** schemathesis found that malformed
requests bypass `AppError` entirely and surface axum's own rejections: `text/plain` bodies, and
statuses the error catalog has no code for — **415**, **422**, and **413** (the last from an
unconditional 2 MB `DEFAULT_LIMIT` inside `Bytes::from_request`, which applies whether or not a
`DefaultBodyLimit` layer is installed; amk-http installs none).

The reference's actual behaviour is captured in `reference/fixtures/27-malformed-request-handling.txt`:
every malformed request is **400 + `application/json` + the full envelope with exactly one
`errors[]` entry**. No 415, no 422, no plain text anywhere in that surface. It reverses two
decisions an earlier inferred contract had made, and adds the 413 case a review lens found after
the fact — site enumeration listed *where* extractors are used and could not see a rejection
enum's *variant* list.

- Contract: `.claude/contracts/amk-http-extractor-rejections.md` (revised twice; reviewed by a
  read-only lens before dispatch).
- **Unreviewed work in progress on branch `amk/p1/http-extractors`, commit `631ddf2`** — 1052
  insertions including `crates/amk-http/src/body.rs` (374 lines) and
  `crates/amk-http/tests/extractor_rejections.rs` (519 lines). The dispatched implementer was
  **terminated by the user mid-work**; its last reported line was "All matching. Now the body
  probes.", so the body-rejection half is incomplete by its own account. It has passed no review
  lens and no gate, and `./scripts/check.sh` has never run against it.
- Next step: rebase that branch onto `main` and re-dispatch against the contract, treating the
  commit as a reference rather than a base. Then re-run `./scripts/p1-gate.sh` in full and capture
  the schemathesis half as fixture evidence with its own ledger check.

## Outstanding, needs the user

**Two pods are still on the live AgentMail account** and should be deleted:
`083523ee-276c-417a-85a3-8703d230c543` and `1c5a543a-5219-41fd-b4a1-289355162f2f`, both named
"My Pod", created 2026-08-17T00:59:41Z. They were created by a probe that wrongly reasoned a
body-less POST could not create anything — `CreatePodRequest`'s fields are all optional, so an
absent body **is** a valid create. Recorded in fixture 27 §4. The local permission layer correctly
blocked the agent from issuing the deletes.

```bash
# run this yourself; the classifier blocks an agent from deleting live resources
sdxd run --with agentmail=kv/agentmail -- bash -c '
  for p in 083523ee-276c-417a-85a3-8703d230c543 1c5a543a-5219-41fd-b4a1-289355162f2f; do
    curl -sS -o /dev/null -w "$p -> %{http_code}\n" -X DELETE \
      -H "Authorization: Bearer $AGENTMAIL_API_KEY" "https://api.agentmail.to/v0/pods/$p"
  done'
```

**The rule this bought:** read the request *schema* before calling a POST non-creating. "Malformed"
is a property of the parse, not of the outcome.

## Registers

- **Register A: empty** except A10(g), a non-blocking confirmation tail on its own T+30d clock.
- **Register C2 is the only open boundary question** — whether a thread's labels are a strict union
  of its members'. No fixture has a mixed-label thread. The fail-closed choice (filter membership,
  recompute aggregates) is implemented and marked `[INFERRED]` in one function in
  `amk-core::labels`.
- Deferred by decision, not by oversight: blobs, FTS, signed download URLs, the jobs table,
  idempotency.

## Migration notes (2026-08-17)

- Forge is now `https://github.com/Appsynergy-io/AgentMailKit` (private). The Gitea remote was
  dropped; that copy is unmaintained.
- The plan moved from `~/.claude/plans/download-agents-mail-sdk-drifting-frog.md` to `docs/PLAN.md`
  so a sandbox session reads the same contract. `scripts/hooks/guard.sh` protects both paths, and
  `guard.test.sh` covers the new one in both directions (falsified: removing the glob makes the
  blocking test fail).
- Machine-local operating rules were copied into `docs/OPERATING-RULES.md` for the same reason.
- `harness-no-github` was retired from the ledger — its premise was "we are not on GitHub".
  `ci-layer-local-only` still holds the no-CI decision, keyed on workflow directories.
- **Pending, needs the user:** `.claude/settings.json` still denies `Bash(gh:*)`. The auto-mode
  classifier correctly blocks an agent from editing its own permissions, so that patch is applied
  by hand — see the migration commit message.
