# AgentMailKit

Self-hosted, 1:1 API-compatible clone of AgentMail (agentmail.to), in Rust, deployed on the OVH
k3s cluster to replace Stalwart. The official AgentMail SDKs, CLI and MCP bridge must work
against this server by changing only the base URL. **No billing surface.**

| What | Where |
|---|---|
| Full plan, registers, phase gates | `docs/PLAN.md` — orchestrator-only, hook-enforced |
| Operating rules, lessons in long form | `docs/OPERATING-RULES.md` |
| Where the last session stopped | `docs/RESUME.md` — read this first on a fresh session |
| Evidence that defines the contract | `reference/fixtures/` — live captures |

## Commands

```bash
./scripts/check.sh               # THE verify command: fmt + clippy + tests + provenance + ledger
./scripts/check.sh --fast        # same minus clippy (what the Stop hook runs)
cargo test --workspace           # unit + fixture-regression tests alone
./scripts/shape-provenance.sh    # dependency direction + naming + boundary-type gate
./scripts/plan-ledger.sh         # the plan's obligations, mechanically
./scripts/hooks/guard.test.sh    # the PreToolUse guard's own tests (both directions)
./scripts/dev-db.sh up           # Postgres 17 for amk-store on 127.0.0.1:55432 (down|dsn|psql)

# conformance (structural diff vs the live reference API; keys come from sdxd, never inline)
AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c \
  'REF_KEY="$AGENTMAIL_API_KEY" python3 conformance/dual_target.py conformance/manifest.json --self-test'
```

## Sandbox vs workstation — know which one you are

Work runs either on the OVH-adjacent workstation or in Claude's cloud sandbox. The sandbox has the
repo and nothing else, and several gates degrade **silently** there rather than failing:

- **`./scripts/check.sh` still reports PASS with no Postgres**, having skipped every DB-backed
  `amk-store` and `amk-http` integration test. It sets `AMK_REQUIRE_DB=1` only when 127.0.0.1:55432
  answers, and prints a one-line warning when it does not. Read that line before believing a PASS.
  `./scripts/dev-db.sh up` needs Docker.
- **`sdxd` and `secd` are LAN-only.** No AgentMail key, so the conformance harness, `p1-gate.sh`
  and every live probe are unavailable. Do not fabricate their output.
- **No OVH box, no k3s cluster, no live AgentMail account.** P6 and every fixture-capturing probe
  are workstation-only.
- What does work anywhere: `cargo build/check/clippy/fmt`, `amk-types` and `amk-core` unit tests,
  the fixture-regression suite, `shape-provenance.sh`, `plan-ledger.sh`, `guard.test.sh`.

A gate that cannot run in the sandbox is reported as **not run**, never as passed.

## The five non-negotiables

1. **Shape provenance.** Every wire type, storage model and identifier derives from AgentMail's
   artifacts (`reference/openapi.json`, the SDKs, `reference/fixtures/`) — never from Stalwart or
   JMAP, not even as an optional field. Stalwart is sanctioned only as a migration *source*
   (amk-import, P6) and as a vendor of standalone crates used inside amk-ingest/amk-outbound.
2. **Frozen types during fan-out.** No implementer edits `amk-types` while parallel work is in
   flight. A type change stops all parallel work; the orchestrator makes it; workers restart.
3. **No invented shapes.** If a needed type/field/status is not in `amk-types` or a fixture,
   STOP and report. Never add a field "that obviously belongs".
4. **Evidence, not assertion.** Report the command run and its actual output. "Tests pass"
   without the output is not a report.
5. **The plan is orchestrator-only.** Subagents never edit `docs/PLAN.md` or the registers. If the
   plan looks wrong, report it; the orchestrator amends it and re-dispatches.

## Crate write order (strictly sequential; nothing downstream starts before its upstream is green)

`amk-types` → `amk-core` → `amk-store` → `amk-http` → *(P0/P1 gates)* →
`amk-ingest` + `amk-outbound` (may fan out) → `amk-events` + `amk-jobs` →
`amk-dns` + `amk-mcp` + `reply-extract` → `amk-import` (LAST, P6 only).

**P0 is closed.** `amk-types`, `amk-core`, `amk-store`, `amk-http` and `amk-cli` are merged and
mutation-verified; C3 is applied to `amk-core::threading`; the SDK gate is MET (fixture 24 — the
unmodified official `agentmail==0.5.9` calling `auth.me()` against `amkd --role api`). `amk init`
mints one UUID that is **both** the `organization_id` and the default pod's `pod_id` — the equality
`amk-http` resolves `POST /v0/inboxes` by (fixture 22).

**P1: Lane L green; Lane R not run.** Extractor-rejection merged (`main` @ `0d0631c`).
`scripts/plan-ledger.sh` still reads `CURRENT_PHASE=P0`. Deferred: blobs, FTS, signed URLs,
jobs, idempotency. **P2: message/thread landed.** `amk-outbound` is partial.

## Branching, dispatch and merge

- **One branch per crate per phase, named `amk/<phase>/<crate>`** — `amk/p2/ingest`,
  `amk/p1/http-extractors`. One worktree per branch under `.claude/worktrees/<name>/`. No branch
  outlives its phase: one open across a phase boundary is a drift signal, so close it or restart it
  from the new base.
- **Commits conventional and atomic** — one logical change, tests in the same commit as the code
  they cover, no `wip` commits on a branch that will be reviewed.
- **Rebase onto `main` before review; never merge-commit into the branch.** The reviewed diff must
  be the diff that lands. Merge order follows the crate write order — never merge a downstream
  crate before its upstream is on `main`.
- **After merge, delete the branch and remove the worktree.** Non-interactive runs never hit the
  keep/remove prompt and leave worktrees behind (`git worktree remove --force` if dirty); the
  ledger's `hygiene-worktrees-swept` fails when one is left with no dispatch in flight.
- **Dispatch order is load-bearing**: write the contract into the worktree **first**, then
  `.amk-scope`, then `touch .claude/fanout.lock`. `.amk-scope` existing is what arms the guard's
  scope rule, so a contract written after it is blocked — and an exemption there is precisely what
  an agent would use to rewrite its own contract. Remove the lock at merge or abandonment.
- **The orchestrator writes no implementation code except `amk-types`.** It holds the plan,
  dispatches, reviews returned diffs, runs gates, merges. Fan out only when the crates share no
  files, neither depends on the other, both depend only on merged gate-passed crates, and
  `amk-types` is frozen; ceiling 2–3 concurrent.
- **Three read-only lenses on every returned diff** — contract-conformance, provenance,
  test-adequacy — plus one on the *contract* before dispatch. Merge only when all three are clean.
- PRs via `gh pr create`. Never `gh auth token`.

## Process rules that are load-bearing

Long form and the failures that bought each: `docs/OPERATING-RULES.md`.

- **A test that has never failed is not evidence, and mutation runs in both directions** — delete
  the guard *and* widen it (`is_some_and(pred)` → `is_some()`), each must kill a test. A guard with
  no clean-path test is unpinned in the direction that breaks real traffic. Seed data that is
  random makes failure random. Falsify every new test before trusting it.
- **A contract's scope is derived, never recalled** — carry the enumeration command and its output
  on a `Scope-derivation:` line (`contract-scope-derived` enforces this), and have a read-only lens
  review the contract *before* dispatch. Site enumeration is not variant enumeration.
- **Delete your mutation scratch copy when the pass ends**, and never mutate a tree another lens is
  reading. `df -h /tmp` when tooling fails absurdly — a full tmpfs presents as a broken harness.
- **The live capture beats the spec text** — five instances so far. Check the fixture before
  trusting `openapi.json`, the SDKs, or existing code.
- **An approval prompt is a defect signal, not friction** — it locates a gap in
  `.claude/settings.json`'s allow-list; fix the list rather than approving past it. The exception is
  a prompt guarding privilege escalation (an agent editing its own permissions), which is the layer
  working as designed.
- **Agent role definitions load only at session start.** `.claude/agents/*.md` is not hot-reloaded,
  so dispatching before a restart runs under default model/effort/tools and nothing inside the
  dispatch can see that. `memory:` is deliberately absent — an unsupported key deregisters silently.

No CI: gating is `./scripts/check.sh` plus the hooks, on the machine running them — a user decision
with its cost recorded in the plan. `scripts/plan-ledger.sh` asserts no workflow directory exists;
adding GitHub Actions is a deliberate plan change, not a migration side effect.

Rules 2 and 3 are enforced by a hook, not honour: `scripts/hooks/guard.sh` blocks an implementer
writing to `amk-types`, to `docs/PLAN.md`, outside its dispatched `.amk-scope`, or introducing a
stalwart-labs type into the protected crates. Subagency is decided by path — inside
`.claude/worktrees/` or not. `.claude/fanout.lock` freezes `amk-types`, the plan and
`scripts/hooks/**` for **everyone including the orchestrator** while a dispatch is in flight.

## Contract facts that are easy to get wrong

Each was observed live; the fixture is the authority.

- `inbox_id` **is** the email address; `message_id` **is** an RFC 5322 angle-bracket Message-ID
  (`<…@…>`) and must be percent-encoded in a path segment (`<`, `>`, `@`).
- Threading groups **only** by the Message-ID reference chain, per inbox. **Subject never
  groups** — not even identical subjects or `Re:`/`Fwd:` variants.
- Two error shapes: auth-layer failures return a **bare** `{"message":…}` (401/403), including
  for a well-formed-but-unknown key; app errors return the full envelope. Branch on `code`.
- Inbox username collision → `already_exists` at **HTTP 403** with `suggestions[]`.
- Restricted-label mail (`unauthenticated`, `spam`, `blocked`, `trash`) is **excluded from list
  endpoints** — reachable only by id or webhook. Search does **not** hide it; get-by-id does not.
- Page tokens are `base64(JSON keyset cursor)`; the token is **absent** on the last page.
- Timestamps: RFC 3339, exactly three fractional digits, `Z`. `Timestamp` is wire-exact.
- Optionals are **omitted** when absent — never `null`, never `""`.
- Live responses carry `organization_id`/`pod_id` (and `smtp_id` on messages) that the SDK types
  omit; emit them or the conformance diff fails.
- `inbox_id` **folds case**: `{"username":"AmkCase"}` stores `amkcase@…`, and lookups resolve any
  casing. Compare with `InboxId::eq_normalized`, never `==` on raw ids.
- Permissions: **38** flags, owned by `amk-types::api_key`. `openapi.json` documents 36; the live
  API emits two more (`owner_email`, `owner_profile`), found by the P1 gate. An **absent**
  permissions object grants everything; a present-but-empty one grants nothing. Never define a
  second representation of these flags — two modules doing that caused a fan-out collision.
- Restricted-label admission must be a **storage-layer predicate**. Post-filtering a fetched page
  leaves a gap: `?limit=1` walked across the cursor returns `count:0` with a `next_page_token` on
  exactly the hidden rows, which discloses how many there are.
- Malformed requests: the reference answers **400 + `application/json` + the full envelope with
  exactly one `errors[]` entry** — no 415, no 422, no plain text. `path` is `["<field>"]` for a
  field failure and `[]` only for a whole-body one; `ValidationIssue` carries kind-specific extras
  (fixture 27). Content-type is **not** enforced, and an absent body means `{}`.
- Our minted keys must **never** begin `am_eu_` — the official node SDK routes that prefix to
  AgentMail's EU host when neither `environment` nor `baseUrl` is set, leaving our base URL.
- `smtp-proto` is parser-only — amk-ingest owns the SMTP state machine. `mail-auth` DKIM wants
  **DER** keys.

## Open at the boundary (do not silently resolve)

**Nothing is open.** C2 — thread labels vs member labels — was the last, and it is closed by
decision (2026-08-19), not by observation: no fixture has a mixed-label thread and inducing one
means provoking a spam classification on someone else's production API. The fail-closed choice
(filter membership, recompute aggregates) ships, is marked `[INFERRED]` on one function, is pinned
by a test named for the assumption, and is declared in `conformance/manifest.json`'s
`expected_divergences`. It reopens only if a mixed-label thread is ever observed.

Closed by probe, and both reversed an implemented choice — check the fixture before trusting code:

- An unbracketed `In-Reply-To` **does** join the bracketed message's thread (fixture 21). AgentMail
  re-brackets the parsed value before matching: the same message returns `in_reply_to` bracketed
  while `headers.In-Reply-To` stays bare. `amk-core::threading` asserted the opposite until C3.
- The Svix retry schedule is **not** truncated at 5 attempts — a 6th fired on two chains (fixture
  07). Keep all 8, with `message.attempt.exhausted` and the 5-day auto-disable as planned.

## Secrets

Never read or print a credential. Inject via `sdxd run` (see the `sdxd` skill); the AgentMail key
is `kv/agentmail`, granted for this directory. Never write a key into a file, fixture or commit.
`sdxd`/`secd` exist only on the workstation. A secret that reaches the transcript is compromised:
say so plainly and rotate it.

## Forge

**GitHub:** `https://github.com/Appsynergy-io/AgentMailKit` (private). Migrated from Gitea
2026-08-17 by user instruction so the repo can be driven from Claude's cloud sandbox — this
supersedes the global "Gitea only, never GitHub" rule for this project. The Gitea remote was
dropped; that copy is unmaintained.
