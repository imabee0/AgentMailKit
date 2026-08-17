# AgentMailKit

Self-hosted, 1:1 API-compatible clone of AgentMail (agentmail.to), in Rust, deployed on the OVH
k3s cluster to replace Stalwart. Official SDKs, CLI and MCP work against this server by changing
only the base URL. **No billing surface.**

Read `docs/RESUME.md` first. The plan, registers and phase gates are `docs/PLAN.md`. Operating
rules are `docs/OPERATING-RULES.md`. Contract facts, crate write order and hooks are `CLAUDE.md`.
Do not duplicate those files here.

## Commands

```bash
./scripts/check.sh               # THE verify command: fmt + clippy + tests + provenance + ledger
./scripts/check.sh --fast        # same minus clippy (what the Stop hook runs)
cargo test --workspace           # unit + fixture-regression tests alone
./scripts/shape-provenance.sh    # dependency direction + naming + boundary-type gate
./scripts/plan-ledger.sh         # the plan's obligations, mechanically
./scripts/hooks/guard.test.sh    # the PreToolUse guard's own tests (both directions)
./scripts/dev-db.sh up           # Postgres for amk-store on 127.0.0.1:55432 (down|dsn|psql)

# conformance (structural diff vs the live reference API; keys come from sdxd, never inline)
AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c \
  'REF_KEY="$AGENTMAIL_API_KEY" python3 conformance/dual_target.py conformance/manifest.json --self-test'
```

`check.sh` still exits PASS with no Postgres, having skipped every DB-backed test — read the
warning line. A gate that cannot run here is **not run**, never passed.

## File map

| What | Where |
|---|---|
| Plan, registers, phase gates | `docs/PLAN.md` — orchestrator-only |
| Where the last session stopped | `docs/RESUME.md` |
| Operating rules (long form) | `docs/OPERATING-RULES.md` |
| Contract facts, crate order, hooks | `CLAUDE.md` |
| Live captures that define the contract | `reference/fixtures/` |
| Grok config, agents, hooks | `.grok/` |

## Conventions

- **No invented shapes.** If a type, field or status is not in `amk-types` or a fixture, stop and report.
- **Shape provenance.** Wire types, storage models and identifiers come from AgentMail artifacts only — never Stalwart or JMAP.
- **The plan is orchestrator-only.** Do not edit `docs/PLAN.md`. If it looks wrong, report it.
- **Evidence, not assertion.** Report the command and its output.
- **Frozen types during fan-out.** Implementers do not edit `amk-types` while parallel work is in flight.

Phase status lives in `docs/RESUME.md`. `scripts/plan-ledger.sh` is the mechanical `CURRENT_PHASE`.

## Invariants

- Dev Postgres is `127.0.0.1:55432` via `./scripts/dev-db.sh`.
- Secrets via `sdxd run`; never print a key. Minted keys must never begin `am_eu_`.
- `amk-types`, `docs/PLAN.md` and `scripts/hooks/**` are hook-protected.
- Forge is GitHub `https://github.com/Appsynergy-io/AgentMailKit` (private). PRs via `gh pr create`. Never `gh auth token`.
