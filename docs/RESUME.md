# Resume here

Where the last session stopped, so a fresh one — on this workstation or in Claude's cloud sandbox —
can continue without re-deriving it. Update this file in the commit that invalidates it.

**Last updated:** 2026-08-17, after the P2 message/thread surface landed.

## Verified state

```
$ ./scripts/check.sh
check: PASS
plan-ledger: PASS
$ ./scripts/check.sh --fast   # summed test results
total passed: 619
```

The branch is green at 619 workspace tests with a live Postgres on 127.0.0.1:55432 (`main` was
570 before this session's two fixes). Without that
database the same command still exits PASS having skipped every DB-backed test — see the sandbox
section of `CLAUDE.md`.

**Re-confirmed in the cloud sandbox, 2026-08-17: `cargo test --workspace` = 570 passed, 0 failed**,
against a hand-started local Postgres (below). The 570 figure therefore holds on both PG17 and PG16.

### DB-backed tests need no Docker any more

`./scripts/dev-db.sh` used to `docker run postgres:17-alpine`, which made every DB-backed
`amk-store`/`amk-http` integration test unrunnable wherever there is no Docker daemon — including
this sandbox, which has the `docker` client and nothing behind it. `check.sh` then exited PASS
having silently skipped that whole suite. It now drives `initdb`/`pg_ctl` directly, so the plain
documented command works in both places and the suite actually runs:

```bash
./scripts/dev-db.sh up      # same port, DSN, role and database as before
```

It finds server binaries on `PATH`, under `/usr/lib/postgresql/*/bin`, `/usr/pgsql-*/bin` and the
Homebrew prefixes, taking the highest version; as root it owns the cluster as an unprivileged user
(`initdb` refuses to run as root) under `/var/lib/amk-dev-db`, otherwise under `.amk-dev-db/` in the
repo. Verified on PostgreSQL 16 here against a baseline recorded on 17, reproducing the 570 exactly,
so the two are interchangeable for this workload.

`./scripts/p1-gate.sh` had the same dependency in three `docker exec ... psql` calls and now talks
to the cluster over TCP. That was worth fixing for more than the sandbox: a container dependency the
gate never needed presented as "this gate is workstation-only", which is a much more expensive
belief than a missing client.

**Correction, measured 2026-08-17: only ONE of `p1-gate.sh`'s four conjuncts needs the live key.**
An earlier revision of this file said three did. Both SDK smokes point at
`AMK_BASE=http://127.0.0.1:8111` — they drive *our* server, never the reference — and schemathesis
is local too. Verified in the sandbox: `schemathesis==4.24.3` installs and selects 25 of 130
operations; `agentmail==0.5.9` (Python) and `agentmail@0.5.19` (Node) both install pinned. The one
credentialed check is the **dual-target conformance diff**, and its manifest is 18 GETs plus a
single `DELETE /v0/auth/me` probe — **read-only against the reference account**.

`docs/PLAN.md` now carries this as an explicit **Lane L / Lane R** split on every phase gate, with
`code-complete` (Lane L green) separated from `gated` (Lane R green). That is what lets an
unattended session keep going instead of stopping at each phase boundary.

## Phase position

**P0: closed.** `amk-types`, `amk-core`, `amk-store`, `amk-http`, `amk-cli` merged, review-panelled
and mutation-verified. Gate met (fixture 24).

**P1: Lane L is GREEN. Lane R is one check away.** All four conjuncts of `./scripts/p1-gate.sh`
were run individually in the sandbox:

| conjunct | lane | result |
|---|---|---|
| schemathesis over the mounted operations | L | **exit 0** — 2056 cases, Coverage 25/25, Fuzzing 25/25, Stateful 84/84 |
| Python SDK smoke (`agentmail==0.5.9`) | L | **28 checks, 0 failed** |
| Node SDK smoke (`agentmail@0.5.19`) | L | **26 checks, 0 failed** |
| dual-target conformance diff | **R-key** | **not run — needs the read-only AgentMail key** |

Both defects that had schemathesis red are fixed and one is merged: the extractor escapes
(`main` @ `0d0631c`) and the metadata round-trip. `scripts/plan-ledger.sh` still reads
`CURRENT_PHASE=P0` and **stays there until the conformance diff runs** — local green is not the
gate, and advancing on Lane L alone is exactly the "gate its own evidence contradicts" trap.

**P2: started.** The first slice — the message and thread LIST endpoints — is written, tested and
mutation-verified on this branch. See "P2 progress" below.

## The extractor-rejection work item: DONE and MERGED (`main` @ 0d0631c), still ungated

**axum extractor rejections escaped our JSON error contract.** schemathesis found that malformed
requests bypassed `AppError` entirely and surfaced axum's own rejections: `text/plain` bodies, and
statuses the error catalog has no code for — **415**, **422**, and **413** (the last from an
unconditional 2 MB `DEFAULT_LIMIT` inside `Bytes::from_request`, which applies whether or not a
`DefaultBodyLimit` layer is installed; amk-http installed none).

The reference's behaviour is `reference/fixtures/27-malformed-request-handling.txt`: every malformed
request is **400 + `application/json` + the full envelope with exactly one `errors[]` entry**.

Contract: `.claude/contracts/amk-http-extractor-rejections.md`, now at **revision 3**.

### What landed

`JsonBody<T>` and `QueryParams<T>` in `crates/amk-http/src/body.rs` wrap axum's own extractors with
`type Rejection = AppError`, mirroring `ids.rs`'s existing `PathPodId` pattern. Content-type is not
enforced and an absent body is `{}` (fixture 27 §2). An explicit `DefaultBodyLimit` comes from
`AppConfig::max_body_bytes`, default 8 MiB and marked `[INFERRED]`.

**Revision 3 fixed two things the first dispatch could not.** Both were defects in the *contract*,
not the work, and both are the same mistake in a third and fourth form — the contract enumerated
extractor *sites* and mistook that for enumerating everything a mandated change reaches:

1. It mandated `AppConfig::max_body_bytes` without making `AppConfig`'s other construction site
   writable. `crates/amk-cli/src/config.rs` builds it with an exhaustive struct literal, so the
   workspace did not compile — an added `impl Default` does not rescue an exhaustive literal.
2. It mandated `limit` behaviour `crate::pagination`'s own types could not express, while leaving
   `pagination.rs` unwritable. `ListQuery::limit` was `Option<u64>`, and fixture 27 §1 requires
   `?limit=abc` (`invalid_type`/`received:"NaN"`) to split from `?limit=-1`, `?limit=` and
   `?limit=0` (one *identical* `too_small` body). `u64::from_str` fails on `"-1"`, `""` and `"abc"`
   the same way, and `"0"` parses and reaches no validator — so the split was unrepresentable and
   the first dispatch pinned three divergences in tests instead. `MAX_LIMIT = 100` separately
   clamped `?limit=101` to echo `100`, which is a value clients paginate against, not cosmetics.

`limit` is now `Option<String>` classified structurally by `pagination::parse_limit`, which *deletes*
the `limit` half of `body.rs`'s serde-message string matching rather than adding to it. The clamp
and `MAX_LIMIT` are gone: fixture 27 §1 records that no cap is enforced. **No divergences remain.**

### Evidence

```
./scripts/check.sh              check: PASS   shape-provenance: PASS   plan-ledger: PASS
cargo test --workspace          587 passed; 0 failed        (main was 570)
```

Mutation pass, both directions, on a scratch copy outside the tree, since deleted:

| mutation | kills |
|---|---|
| delete the empty-`limit` guard | `empty_negative_and_zero_limits_are_all_the_same_too_small_issue` |
| widen: every rejection -> one `too_small` | `a_non_numeric_limit_is_invalid_type_nan_not_too_small` |
| restore the `MAX_LIMIT` clamp | `a_limit_above_one_hundred_is_accepted_and_echoed_verbatim` **and** `a_limit_above_the_maximum_is_neither_clamped_nor_rejected` |
| narrow `i128` -> `i64` | `a_negative_below_i64_min_is_still_too_small_not_nan` |

`./scripts/derive-request-extractors.sh` re-run: sections 1–2 show **no** bare `Json<`/`Query<` left
in argument position; section 4 lists both new wrappers. Fixture 27's probe table was replayed
against a locally-served `amkd --role api` and every row matches, including `?limit=101` -> 200 with
`"limit":101`, and the two content-type rows now returning 200 rather than 415. No 415, 422, 413 or
`text/plain` anywhere in the surface.

### What is NOT done

- **The three review lenses never ran** on this diff, and it is now merged. Contract-conformance,
  provenance and test-adequacy were all required before merge; none was dispatched, because this
  session is instructed not to use the Agent tool unless asked. Recorded as a deviation rather than
  quietly skipped — see "Outstanding" item 2.
- **`./scripts/p1-gate.sh` has not been re-run in full.** Three of its four conjuncts need the live
  AgentMail key via `sdxd`, so this is workstation-only. Until it passes, `CURRENT_PHASE` stays at
  `P0` — declaring P1 met on the strength of the local suite alone is exactly the "gate its own
  evidence contradicts" this file warned about.
- The schemathesis conjunct still needs its own fixture capture and ledger check.

## The metadata round-trip defect: FIXED (2026-08-17)

Found by the P1 gate's schemathesis conjunct, the first time it ran in the sandbox.
`PATCH /v0/inboxes/{id}` with `{"metadata":{"a":1.7976931348623157e+308}}` returned **500** and left
the inbox **permanently unreadable** — the row was written, and every later `GET`, `PATCH` or list
touching it failed the same way. One accepted request removed an inbox from the API.

Contract: `.claude/contracts/amk-metadata-roundtrip.md`.

### Root cause

The write path and the read path disagreed about one number. serde_json accepts
`1.7976931348623157e308` in exponent form; Postgres `jsonb` normalises it to `numeric` and renders
it back with **no exponent** as a 309-digit integer; serde_json's *long-integer* path — stricter
than its float path — then refuses that literal with `number out of range`, **even though the value
is below `f64::MAX`**.

| literal | parses as f64? |
|---|---|
| `1.7976931348623157e308` (what the client sends) | ok |
| `1` + 308 zeros | ok |
| `17976931348623157` + 292 zeros (**what jsonb emits**) | ERR |

Note `1e308` survives while the *smaller* `1.7976931348623157e308` does not — so a magnitude cap
would have been the wrong fix.

### The fix

`MetadataValue::survives_storage_round_trip()` in `amk-types` reconstructs jsonb's own rendering
(shortest round-trip decimal, exponent expanded to plain notation) and asks serde_json whether it
parses back. **No hard-coded threshold**: the boundary is a property of serde_json's parser, so
deriving it by construction is the point. `amk-http`'s inbox create and `validate_update` refuse a
failing value at the write boundary, so nothing unstorable ever reaches a row.

### Evidence

```
./scripts/check.sh        check: PASS   shape-provenance: PASS   plan-ledger: PASS
cargo test --workspace    599 passed; 0 failed        (was 587)
```

Repro replayed against the fixed build — the corruption half, not just the status:

```
PATCH f64::MAX   -> 400 {"code":"validation_error","errors":[{"code":"custom","path":["metadata","a"],...}]}
GET after        -> 200      <- was 500 forever, before
PATCH 1e308      -> 200      <- still accepted; not a blunt cap
GET after 1e308  -> 200
```

Mutation pass, both directions, scratch copy outside the tree since deleted:

| mutation | kills |
|---|---|
| delete the guard (everything round-trips) | 5 tests, incl. `a_patch_..._leaves_the_inbox_readable` |
| widen it to refuse every number | `one_e_308_is_stored_while_the_smaller_f64_max_is_refused` |

### `[INFERRED]`, and how to close it

No fixture covers what the reference does with an out-of-range metadata number, and
`conformance/manifest.json` is read-only so no existing capture can answer it. Three choices are
marked `[INFERRED]` in `reject_unstorable_metadata`: the **400 `validation_error`** status (not
really a guess — 500 is wrong under any reading), the **`custom` issue kind** (chosen because it
claims nothing about the schema), and the **two-segment `path`** naming the offending key.

**One live request settles all three** and should replace that block:
`PATCH /v0/inboxes/{id}` with `{"metadata":{"a":1.7976931348623157e+308}}` against
api.agentmail.to. That is R-key work — see item 1 below.

## P2 progress

Contracts: `.claude/contracts/amk-http-message-thread-reads.md`,
`.claude/contracts/amk-store-mail-mutations.md`.

**Landed (unmerged): the whole message/thread read+mutate surface.** Router reconciles clean at
**41 operations** (was 25 at P1):

```
GET                     /v0/inboxes/{inbox_id}/messages
GET  PATCH  DELETE      /v0/inboxes/{inbox_id}/messages/{message_id}
GET                     /v0/threads, /v0/pods/{pod_id}/threads, /v0/inboxes/{inbox_id}/threads
GET  PATCH  DELETE      /v0/threads/{thread_id} and the same at both other mounts
```

It landed in two slices on purpose. The LIST slice went first with get-by-id deliberately
**unmounted**, because every get-by-id path in the spec carries GET, PATCH *and* DELETE and
`amk-store` had none of the latter two — a path serving some of its described methods is what
`derive-implemented-paths.sh` reports, and the gate's schemathesis scope is derived by PATH, so a
half-served path makes the gate fuzz operations the server does not implement and report absences
as failures. `amk-store`'s `update`/`delete` came second, then the six mounts together.

**Fixture 19's system-label gate is implemented once and called from both resources.** The gate
refuses `sent`/`received`/`bounced`/`scheduled`; **restricted labels are NOT system**, so a client
may set `spam`/`trash`/`blocked`/`unauthenticated`; `errors[].path` is `["add_labels", 0]` (field
name then array index); and one bad label rejects the **whole** mutation, asserted by re-reading
the row rather than by the status.

**Two invented shapes were caught by tests, both the same mistake.** A draft used `thread_read`,
and the update/delete handlers wanted `thread_update`/`thread_delete`. None of the three exists —
the vocabulary is the 38 names in `amk-types::api_key::WIRE_NAMES`, and `message_read`,
`message_update` and `message_delete` carry threads too, per their own field docs.

### Where P2 goes next

1. **`amk-ingest` + `amk-outbound`** — the write order allows these to fan out. Both are large and
   neither is started.
2. Deferred and still deferred: search (needs FTS), attachments and `raw` (need blobs and signed
   URLs), the batch pair (parked under *Full parity*), drafts (P3).
3. **P2's gate has an R-phys half no key substitutes for**: mail injected from the OVH box via
   `/root/amksend.py` appearing with correct threading over a 3-message exchange, and an SDK send
   to a Gmail account showing DKIM+SPF pass.

## Outstanding, needs the user

Ordered by how much each one unblocks. The first four are what stand between an unattended session
and continuous execution; the rest are physical.

### 1. A read-only AgentMail API key, reachable from the sandbox — the single biggest unblock

Closes the one credentialed conjunct in every P1–P5 gate (the dual-target conformance diff). Without
it, phases can only reach `code-complete`, never `gated`, and `CURRENT_PHASE` can never advance.

- **Read-only is sufficient and is what should be used.** `conformance/manifest.json` is 18 GETs and
  one `DELETE /v0/auth/me` probe; nothing is created on the reference account. The permission model
  has 38 flags, so mint a key with read flags only rather than exposing the org root key.
- Deliver it as an **environment variable on the Claude Code environment** (`AGENTMAIL_API_KEY`),
  not in a file and not in a message. `sdxd`/`secd` are LAN-only and cannot reach here.
- This needs a small amendment to the plan's secrets rule, which currently says "inject via
  `sdxd run`" and has no sandbox clause. Say the word and I will write it.
- **Judgement call that is yours, not mine:** a key in the sandbox environment is readable by
  anything running here, including me. A read-only key on a personal account is a defensible
  exposure; the org root key is not. If you would rather not, say so — the Lane L / Lane R split in
  `docs/PLAN.md` is built to work without it, at the cost of finding conformance divergences later.

### 2. Permission to dispatch subagents

`docs/PLAN.md` mandates three read-only review lenses on every returned diff, and implementer
fan-out from P2 onward. **This session is instructed not to use the Agent tool unless you ask for
it**, so the extractor-rejection diff that just merged was written and reviewed by the same actor —
which the plan explicitly forbids. Either grant it, or accept single-actor review and let me record
that as a standing deviation rather than a per-diff surprise.

### 3. A `.claude/settings.json` patch — I cannot apply this one myself

The classifier blocks an agent editing its own permissions, correctly. Every approval prompt is a
gap in this list (`CLAUDE.md`'s own rule), and each one stops an unattended run. Missing, from
commands actually used this session:

```jsonc
// add to permissions.allow
"Bash(./scripts/p1-gate.sh:*)", "Bash(./scripts/derive-request-extractors.sh:*)",
"Bash(git cherry:*)", "Bash(git cherry-pick:*)", "Bash(git ls-remote:*)",
"Bash(git stash:*)", "Bash(git reset --soft:*)", "Bash(git checkout:*)",
"Bash(git push -u origin claude/:*)", "Bash(git push origin --delete:*)",
"Bash(curl:*)", "Bash(psql:*)", "Bash(rm:*)", "Bash(chmod:*)", "Bash(chown:*)",
"Bash(install:*)", "Bash(useradd:*)", "Bash(su amkpg:*)", "Bash(timeout:*)",
"Bash(tee:*)", "Bash(awk:*)", "Bash(paste:*)", "Bash(bc:*)", "Bash(xargs:*)",
"Bash(sort:*)", "Bash(npm:*)", "Bash(node:*)", "Bash(.venv-gate/bin/:*)",
"Bash(.venv-schemathesis/bin/:*)"

// REMOVE — stale or now dead
"Bash(docker exec amk-dev-postgres:*)",   // dev-db.sh no longer uses Docker
"Bash(docker ps:*)",                       // same
"Bash(git -C /home/imma/projects/AgentMailKit push origin main:*)"  // workstation-only path
```

Note this file also, briefly, claimed `.claude/settings.json` denies `Bash(gh:*)`. It does not —
the deny list holds only `gh auth token` and `gh auth login --with-token`.

### 4. Which branch policy wins

`docs/PLAN.md` says one branch per crate per phase, `amk/<phase>/<crate>`. This session is
instructed to develop and push only to `claude/next-steps-planning-u0nvud`. They conflict, and the
merged PR used the session branch. Tell me which governs, or grant `git push origin amk/*` and I
will follow the plan.

### 5. Physical infrastructure — no key substitutes for these

- **P2 gate:** mail injected from the OVH box via `/root/amksend.py` must appear with correct
  threading over a 3-message exchange, and an SDK send to a Gmail test account must show DKIM+SPF
  passing. Needs the box and a Gmail account.
- **P5 gate:** one real domain verified end-to-end, and an induced bounce.
- **P6:** the k3s cluster, the restore drill, the cutover.

I can write and locally verify all of P2–P5's code without these; I cannot close their gates.

### 6. Two live pods still on the AgentMail account

`083523ee-276c-417a-85a3-8703d230c543` and `1c5a543a-5219-41fd-b4a1-289355162f2f`, both "My Pod",
created 2026-08-17T00:59:41Z, by a probe that wrongly reasoned a body-less POST could not create
anything — `CreatePodRequest`'s fields are all optional, so an absent body **is** a valid create.
Recorded in fixture 27 §4. The local permission layer correctly blocked the agent from deleting
live resources.

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

### 7. Housekeeping I could not finish

`amk/p1/http-extractors` still exists on the remote. Its work is fully merged (`git cherry` reports
it as already-applied), but this environment's git proxy hangs up on delete refspecs across four
retries. Delete it from the workstation or the GitHub UI.

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
- `harness-no-github` was retired from the ledger at the migration commit (`8e9cdb5`) — its premise
  was "we are not on GitHub". `ci-layer-local-only` still holds the no-CI decision, keyed on
  `.gitea/workflows` and `.github/workflows` rather than on the whole `.github/` directory, so
  non-CI GitHub metadata (issue templates, CODEOWNERS) is allowed and a workflow is not.
  **A revision of this file dated 2026-08-17 claimed the retirement never happened. That claim was
  wrong and is withdrawn** — it came from reading a ledger run inside the `amk/p1/http-extractors`
  worktree, which sat two commits behind `main` and therefore predated the retirement. The lesson
  is worth more than the correction: a ledger result is only about the tree it ran in, and this
  session had two trees at different commits at the same moment.
- ~~`.claude/settings.json` still denies `Bash(gh:*)`~~ — **struck, this was wrong.** The deny list
  contains only `gh auth token` and `gh auth login --with-token`; `gh pr`, `gh api`, `gh issue` and
  `gh repo` are all allowed. The real permission gaps are listed under "Outstanding, needs the
  user" above, and the reason they still need a human hand is unchanged: the classifier correctly
  blocks an agent from editing its own permissions.
