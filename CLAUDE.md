# AgentMailKit

Self-hosted, 1:1 API-compatible clone of AgentMail (agentmail.to), in Rust, deployed on the OVH
k3s cluster to replace Stalwart. The official AgentMail SDKs, CLI and MCP bridge must work
against this server by changing only the base URL. **No billing surface.**

Full plan: `~/.claude/plans/download-agents-mail-sdk-drifting-frog.md`.
Evidence: `reference/fixtures/` — live captures that define the contract.

## Commands

```bash
./scripts/check.sh               # THE verify command: fmt + clippy + tests + provenance
./scripts/check.sh --fast        # same minus clippy (what the Stop hook runs)
cargo test --workspace           # unit + fixture-regression tests alone
./scripts/shape-provenance.sh    # dependency direction + naming + boundary-type gate
./scripts/hooks/guard.test.sh    # the PreToolUse guard's own tests (24 cases, both directions)
./scripts/dev-db.sh up           # Postgres 17 for amk-store on 127.0.0.1:55432 (down|dsn|psql)

# conformance (structural diff vs the live reference API; keys come from sdxd, never inline)
AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c \
  'REF_KEY="$AGENTMAIL_API_KEY" python3 conformance/dual_target.py conformance/manifest.json --self-test'
```

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
5. **The plan is orchestrator-only.** Subagents never edit the plan or the registers. If the
   plan looks wrong, report it; the orchestrator amends it and re-dispatches.

## Crate write order (strictly sequential; nothing downstream starts before its upstream is green)

`amk-types` → `amk-core` → `amk-store` → `amk-http` → *(P0/P1 gates)* →
`amk-ingest` + `amk-outbound` (may fan out) → `amk-events` + `amk-jobs` →
`amk-dns` + `amk-mcp` + `reply-extract` → `amk-import` (LAST, P6 only).

Current phase: **P0** — `amk-types` (62) and `amk-core` (117) green, both through two review rounds
and mutation-verified. `amk-store` (31) implemented for the P1 slice on branch `amk/p0/store`:
migrations, pool, control-plane repositories, keyset pagination, message/thread reads. Deferred by
decision, not omission: blobs, FTS, signed URLs, jobs, idempotency, and `api_keys` (blocked —
`amk-types` has no `ApiKey` wire resource yet). Contracts: `.claude/contracts/amk-{store,http}.md`.

**A test that has never failed is not evidence.** Mutation testing found six defects in a green,
twice-reviewed crate that two rounds of reading had missed — including a fail-open reachable
through a sibling of the function its regression test guarded. Mutate before claiming a gate.

Rules 2 and 3 are enforced by a hook, not honour: `scripts/hooks/guard.sh` blocks an implementer
writing to `amk-types`, to the plan, outside its dispatched `.amk-scope`, or introducing a
stalwart-labs type into the three protected crates. Subagency is decided by path — inside
`.claude/worktrees/` or not.

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
  endpoints** — reachable only by id or webhook.
- Page tokens are `base64(JSON keyset cursor)`; the token is **absent** on the last page.
- Timestamps: RFC 3339, exactly three fractional digits, `Z`. `Timestamp` is wire-exact.
- Optionals are **omitted** when absent — never `null`, never `""`.
- Live responses carry `organization_id`/`pod_id` (and `smtp_id` on messages) that the SDK types
  omit; emit them or the conformance diff fails.
- `inbox_id` **folds case**: `{"username":"AmkCase"}` stores `amkcase@…`, and lookups resolve any
  casing. Compare with `InboxId::eq_normalized`, never `==` on raw ids.
- Permissions: **36** flags (not 34), owned by `amk-types::api_key`. An **absent** permissions
  object grants everything; a present-but-empty one grants nothing. Never define a second
  representation of these flags — two modules doing that is what caused the collision above.
- Restricted-label admission must be a **storage-layer predicate**. Post-filtering a fetched page
  leaves a gap: `?limit=1` walked across the cursor returns `count:0` with a `next_page_token` on
  exactly the hidden rows, which discloses how many there are.
- `smtp-proto` is parser-only — amk-ingest owns the SMTP state machine. `mail-auth` DKIM wants
  **DER** keys.

## Open at the boundary (do not silently resolve)

- Thread labels vs member labels is **unobserved** — no fixture has a mixed-label thread. The
  fail-closed choice (filter membership, recompute aggregates) is implemented and marked
  `[INFERRED]` in one function. Register C2. **This is the only one still open.**

Closed by probe, and both reversed an implemented choice — check the fixture before trusting code:

- An unbracketed `In-Reply-To` **does** join the bracketed message's thread (fixture 21). AgentMail
  re-brackets the parsed value before matching: the same message returns `in_reply_to` bracketed
  while `headers.In-Reply-To` stays bare. `amk-core::threading` currently asserts the opposite and
  must be inverted.
- The Svix retry schedule is **not** truncated at 5 attempts — a 6th fired on two chains (fixture
  07). Keep all 8, with `message.attempt.exhausted` and the 5-day auto-disable as planned.

## Secrets

Never read or print a credential. Inject via `sdxd run` (see the `sdxd` skill); the AgentMail key
is `kv/agentmail`, granted for this directory. Never write a key into a file, fixture, or commit.

## Forge

Gitea only: `https://git.appsynergy.io/imabee/AgentMailKit`. Never GitHub. Push/PR via the
credential helper and the API under `sdxd run` from `~/projects`.
