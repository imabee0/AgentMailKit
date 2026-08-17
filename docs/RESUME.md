# Resume here

Where the last session stopped, so a fresh one — on this workstation or in Claude's cloud sandbox —
can continue without re-deriving it. Update this file in the commit that invalidates it.

**Last updated:** 2026-08-17, after a sandbox assessment of the open P1 branch.

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

**Re-confirmed in the cloud sandbox, 2026-08-17: `cargo test --workspace` = 570 passed, 0 failed**,
against a hand-started local Postgres (below). The 570 figure therefore holds on both PG17 and PG16.

### DB-backed tests ARE runnable in the sandbox — `dev-db.sh` is not the only way

`./scripts/dev-db.sh up` needs a Docker daemon and correctly exits 1 without one (the sandbox has
the `docker` client but no daemon). It is not the only route: the sandbox image carries PostgreSQL
16 server binaries, and a throwaway instance on the port and DSN the tests already expect covers
every DB-backed `amk-store`/`amk-http` test. Reproduced the full 570 exactly, so PG16 is a faithful
stand-in for the containerised PG17 for this workload.

```bash
# sandbox only; initdb refuses to run as root, hence the unprivileged user
export PATH=/usr/lib/postgresql/16/bin:$PATH
useradd -m amkpg; install -d -o amkpg -m 700 /home/amkpg/pgdata
su amkpg -c "PATH=$PATH initdb -D /home/amkpg/pgdata -U amk --auth=trust"
su amkpg -c "PATH=$PATH pg_ctl -D /home/amkpg/pgdata \
  -o '-p 55432 -k /tmp -c listen_addresses=127.0.0.1' -l /home/amkpg/pgdata/log start"
psql postgres://amk@127.0.0.1:55432/postgres -c "create database amk owner amk"
psql postgres://amk@127.0.0.1:55432/postgres -c "alter user amk password 'amk-dev-local'"
export AMK_DATABASE_URL='postgres://amk:amk-dev-local@127.0.0.1:55432/amk' AMK_REQUIRE_DB=1
```

This narrows the sandbox's "cannot verify" list to what genuinely needs the LAN: `sdxd`/`secd`, the
live AgentMail account, the OVH box. It does **not** make the P1 gate runnable — see below.

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
  probes." It has passed no review lens and no mutation pass.

### Measured state of that branch (sandbox, 2026-08-17) — further along than assumed

The branch is **pushed**: `origin/amk/p1/http-extractors` = `631ddf2`, 2 commits behind `main`.
Nothing is stranded on the workstation. Measured by checking it out in a worktree with the local
Postgres above:

```
cargo test -p amk-http --test extractor_rejections   16 passed; 0 failed
cargo test --workspace  (with the one-line fix below) 586 passed; 0 failed   # +16 vs main, no regressions
cargo clippy --workspace --all-targets -- -D warnings  clean
cargo fmt --all -- --check                             clean
./scripts/shape-provenance.sh                          PASS
```

So the earlier "incomplete by its own account" reading understates it — the body half is written
and tested. Two things are actually outstanding, and **one of them is a defect in the contract, not
in the work**:

1. **The contract is unsatisfiable as written.** It mandates a `max_body_bytes` field on
   `AppConfig` but its writable paths omit `crates/amk-cli/**`. `crates/amk-cli/src/config.rs:51`
   builds `AppConfig` with an exhaustive struct literal, so the workspace does not compile:
   `error[E0063]: missing field 'max_body_bytes' in initializer of 'AppConfig'`. The added
   `impl Default` does not help an exhaustive literal. The fix is one line
   (`..AppConfig::default()`), and all figures above were taken with it applied as a scratch edit.
   This is the contract's own warned-about failure mode again — the derivation enumerated
   *extractor* sites, never *`AppConfig` construction* sites. **Site enumeration is not variant
   enumeration, and it is not construction-site enumeration either.**
2. **Three divergences from the contract's edge cases**, all rooted in `crates/amk-http/src/
   pagination.rs`, which is also outside the writable paths. The implementer marked each one
   `[INFERRED]`/"documented divergence" in code *and* pinned it in a test, so nothing is hidden —
   but the branch does not satisfy its contract, and #3 is a live conformance divergence against
   fixture 27:
   - `?limit=-1`, `?limit=` — contract edge case 6 wants `too_small`/`minimum:0`/`inclusive:false`;
     emitted is `invalid_type`/`received:"NaN"`, because `ListQuery::limit` is `Option<u64>` and
     cannot represent a negative at all (`"-1"` and `"abc"` fail identically).
   - `?limit=0` — contract wants `too_small`; emitted is 200 with an empty page, because `u64`
     parses `"0"` and `pagination::resolve` already treats a supplied 0 as "return nothing".
   - `?limit=101` — contract edge case 7 says 200 **echoing `limit:101`**, and "do not add a cap".
     `pagination` clamps to `MAX_LIMIT = 100` (itself marked `[ASSUMED]` on `main`) and echoes the
     *applied* value, so the response says `100`.

### The decision this needs before re-dispatch

Amending the writable paths to include `crates/amk-cli/src/config.rs` is forced — the field change
is mandated, so its call sites must be in scope. The open judgement call is the pagination three:

- **(A) Widen the contract to include `crates/amk-http/src/pagination.rs`** and make `limit` a
  signed/coercing type so `too_small` and the un-clamped echo match fixture 27. Conformance-correct;
  closes a real divergence; also retires the `[ASSUMED]` on `MAX_LIMIT`.
- **(B) Accept the divergences**, record them in `amk-p1-divergences.md` with a register entry, ship.

**Recommend (A)**, at minimum for `?limit=101`: that one is *directly observed* in fixture 27, and
schemathesis — the conformance half that found this whole work item — is exactly what would catch
it. Declaring P1 met over a known conformance divergence repeats the mistake this file already
warns about one section up.

### Then

1. Amend the contract (orchestrator-only), re-run a read-only lens over it before dispatch.
2. Rebase `amk/p1/http-extractors` onto `main`; re-dispatch against the amended contract, treating
   `631ddf2` as a reference rather than a base.
3. Missing report artifacts the dispatch never produced and which are required before merge: the
   probe-table re-run, the `./scripts/derive-request-extractors.sh` re-run, and the **mutation pass
   in both directions**. Plus the three review lenses.
4. Re-run `./scripts/p1-gate.sh` in full and capture the schemathesis half as fixture evidence with
   its own ledger check, then advance `CURRENT_PHASE` to P1.

**Where each step can run.** Steps 1–3 are sandbox-capable now that DB-backed tests are. Of the
gate's four conjuncts, the dual-target conformance diff and both SDK smokes shell out to `sdxd` for
the live AgentMail key and are **workstation-only**; the schemathesis conjunct only needs a local
`amkd`, `reference/openapi.json` and a pip venv, so it is *probably* sandbox-runnable — untested,
so treat that as a lead, not a fact. Step 4 as a whole is workstation-only.

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
- `harness-no-github` was **not** in fact retired from the ledger, contrary to what this file said
  until 2026-08-17: `scripts/plan-ledger.sh:94` still asserts it, still described as "no .github/
  (Gitea only)". It currently reads MET only because no `.github/` directory exists. It will fire
  the moment anyone adds a PR template or an issue template — neither of which is CI, and both of
  which are ordinary on GitHub. Either retire it for real or re-key it on workflow directories
  alone; `ci-layer-local-only` already holds the no-CI decision that way.
- **Pending, needs the user:** `.claude/settings.json` still denies `Bash(gh:*)`. The auto-mode
  classifier correctly blocks an agent from editing its own permissions, so that patch is applied
  by hand — see the migration commit message.
