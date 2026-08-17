# Operating rules — AgentMailKit

Project-scoped copy of the operating rules this build has run under since P-1. They lived in the
machine-local `~/.claude/CLAUDE.md` until the 2026-08-17 GitHub migration, where that stopped
working: a session in Claude's cloud sandbox sees the repo and nothing else, so a rule outside the
repo is a rule that silently stops applying. Everything below is in force for this repository
regardless of which machine the session runs on.

`CLAUDE.md` is the loaded contract and stays under its 200-line cap; this file is where the
long-form reasoning lives. Where the two disagree, `CLAUDE.md` wins.

## Authority and judgment

Build what a competent engineer on this stack would ship for the stated goal. Interpret the request
narrowly and implement it completely: judgment is broad, permission is not.

- **Authorisation comes from a request to act, never from criticism.** Questions, evaluations and
  complaints ask for information or judgment — answer, analyse, propose; do not act. A correction
  landing while requested work is in flight redirects that work; it does not widen it.
- **Permission is not inferred.** Completing the request is authorised; changing state it did not
  cover is not. When an action would materially extend the request's scope, or create an
  irreversible consequence not implied by it, ask first — even when it is the obvious next step.
- **Assume conventional engineering details.** Ask only when the choice materially affects
  correctness, product behaviour, architecture, security or the stated goal — then one question
  carrying a recommended answer, proceeding with everything not blocked. Reversible choices are
  yours: take the default and note it in one line at the end.
- **Codebase over preference.** Existing patterns, naming, libraries and error handling win even
  when you would choose otherwise. A new dependency or new pattern only when the request cannot be
  met without it — then say so in one line.
- **Vertical, not horizontal.** Finish every layer the request already touches — entry point →
  handler → storage → error path → test → doc. Wiring the asked-for capability through is not scope
  creep; a second capability, screen, endpoint or abstraction is.
- **Unknowns are questions, not inventions.** Never fabricate a schema, endpoint, credential, copy
  string or product rule to keep moving, and never present a stub or mock as working. This is the
  general form of non-negotiable #3.
- **Finish, then report.** Done = the capability runs end-to-end through its real entry point,
  error paths included, with `./scripts/check.sh` exited clean and its output read. Complete every
  part of a multi-part request, naming any part left blocked and why. Unverified work is reported
  as unverified, never as done.

## Writing — docs and replies

Docs are read by models far more than by people: each fact once, one precise sentence, where a
reader looks for it, and only what is true, current and actionable. Every sentence must change the
reader's behaviour; delete the rest. Frame affirmatively; an anti-pattern only when it gates a real
failure. Exclude history, rationale, TODOs and anything readable from the code in a minute.

- Budgets: `CLAUDE.md` ≤ 200 lines (this project's own cap, set in the plan), `README` ≤ 100,
  module doc ≤ 40. Over budget, compress before appending; update the doc in the commit that
  invalidates it.
- Replies lead with the answer — yes/no/number/name. No preamble, hedging or recap.

## Context and grounding

Think first on multi-step logic, architecture, unknown-root-cause debugging and multi-file systems;
answer directly on lookups and single-file edits. One approach, revisited only when facts
contradict it. Read the relevant files before answering codebase questions and ground claims in
code opened this session. Smallest correct change; tests assert observable behaviour.

- Locate with Grep/Glob, then read the exact range; whole-file reads only for small files. Facts
  established this session stay established.

## Structure and forge

**GitHub, as of 2026-08-17** — `https://github.com/Appsynergy-io/AgentMailKit`, private. This
reverses the standing "Gitea only, never GitHub" rule by explicit user instruction, for this
project only, so the repo can be driven from Claude's cloud sandbox. The Gitea remote was dropped;
a copy of the pre-migration history remains on `git.appsynergy.io` and is not maintained.

Default flow: branch → implement → verify → push → PR → merge. `main` is protected by convention,
not by a server rule: nothing lands except a merge that passed a phase gate.

- **Isolation:** mutating work runs as a subagent in an isolated git worktree under
  `.claude/worktrees/`; the orchestrator does not write feature code on the primary checkout.
  Waves of 2–3 concurrent subagents, disjoint files. Pure reads may stay on the parent.
- **Delegation:** subagents execute decisions, they do not make them. The parent fixes intended
  behaviour, scope and design before handing work over; within that boundary the subagent resolves
  ordinary implementation detail and never invents or broadens product behaviour, architecture or
  scope. An unresolved product question goes back to the parent.
- **Verify:** `./scripts/check.sh` in one foreground shell; process exit is the completion event.
  Report done only after it exits, and read its output — see the sandbox caveat below.
- **No stall:** subagents wake on a harness completion notification only — no sleep-polling, no
  busy loops.

## Security and secrets

OWASP Top 10:2025 and NIST SSDF by default. The choices that differ from a naive implementation:

- Authorization deny-by-default, server-side on every route, including SSRF egress. Parameterised
  queries for all external input. Every error path explicit; failures close.
- Pin exact dependency versions, commit lockfiles. argon2id for key secrets, SHA-256+ elsewhere,
  TLS everywhere, short-lived tokens. Secrets injected at deploy, never in source.
- **A secret in the transcript is compromised — say so plainly and rotate it.** This has happened
  once in this project: a `pgrep -af` printed a throwaway gate key's `Authorization` header,
  because argv is world-readable via `/proc/<pid>/cmdline`. The fix was a `0600` curl config file
  and an env-sourced hook, not a promise to be careful.
- Keep keys, tokens, passwords and DSNs out of tool inputs, read-back output, tracked files,
  commits and PRs; `<redacted>` in examples. Pass by reference, never `echo`/`cat`.
- Any probe touching `CreateApiKeyResponse` redacts **at capture**, not after.
- Never expose secrets in hook output, transcripts or logs — DKIM key material passes through this
  project.

## Code

Rust: `thiserror` in libraries, `anyhow` in binaries, `?`, and `.expect("invariant: reason")` over
`unwrap`. Domain first; illegal states unrepresentable; pin exact deps; audit before adding.

Review AI-written code slower per line than human code: delete restating comments, verify packages
exist, kill gratuitous abstractions, check deprecations, cover every error path, assert behaviour
in tests.

## Lessons that cost us

Each was bought with a real failure. `CLAUDE.md` carries the one-line form; this is the detail.

- **A test that has never failed is not evidence, and mutation runs in both directions.** Mutating
  a green, twice-reviewed crate found six defects two rounds of reading missed. Twenty *deletion*
  mutations then reported no survivors while a live one sat in `messages::insert`: widening
  `in_reply_to`'s guard to `is_some()` rejects every threaded reply and the suite stayed green,
  because only hostile-value tests touched that field. Delete **and** widen. A guard with no
  clean-path test is unpinned in the direction that breaks real traffic.
- **A test whose seed data is random is a test whose failure is random.** Three keyset-tiebreak
  tests seeded rows with random ids, so a dropped `ORDER BY` tiebreak surfaced in ~3 runs of 10.
  Seed the order the assertion depends on.
- **A test that passes for the wrong reason is the same defect one level up.** Self-comparisons
  (expected value built from the constant under test), tripwires that iterate their own copy, and
  fixtures quoted in comments rather than read at test time have each shipped here. Falsify a new
  test by breaking the thing it guards and confirming it fails.
- **A contract's scope is derived, never recalled.** The id-safety dispatch cost four correction
  rounds because its contract listed call paths remembered from a review report instead of
  enumerated from the code; five sites were missing and every one was found by enumerating. A
  contract scoping existing code carries the command that produced its scope and that command's
  output on a `Scope-derivation:` line. **Site enumeration is not variant enumeration** — a later
  413 defect lived in a rejection enum's variant list, which no site scan could see.
- **A mutating reviewer works on a private copy, never the dispatch worktree, and never
  concurrently with a reading one** — and deletes the copy when the pass ends. A test lens mutating
  in place while two lenses read the same tree produced a review that reasonably read as a
  prompt-injection attempt, void test evidence, and a flaky-test diagnosis that blamed Postgres.
  Separately, seven abandoned scratch copies filled the 32G `/tmp` tmpfs; because the Bash tool
  appends `pwd > /tmp/claude-<pid>-cwd`, one ENOSPC made *every* command fail with no output and
  look like a broken harness. `df -h /tmp` when tooling fails absurdly.
- **The live capture beats the spec text.** Five instances so far: fixture 19's system labels,
  DELETE statuses (`openapi.json` 0-for-3), `Organization`'s 17-vs-12 fields, `ApiKeyPermissions`'
  38-vs-36 flags, and fixture 27's malformed-request handling. When a document and a capture
  disagree, the capture is the contract.
- **A reviewer agreeing with the code is not the same as the code being right.** Two independent
  readers called a permission-after-lookup ordering correct, including a lens that verified it
  clean — and it was a guessable existence oracle on a public multi-tenant API. Ask what each
  option discloses, not whether the stated rationale is coherent.
- **An approval prompt is a defect signal, not friction.** With the permissions layer built as the
  plan specifies, routine plan-following work never reaches the user. Being asked to approve
  `cargo test` means `.claude/settings.json`'s allow-list is missing a command the plan sanctions —
  fix the list, never approve past it. The exception is a prompt guarding privilege escalation
  (an agent editing its own permissions), which is the layer working.
