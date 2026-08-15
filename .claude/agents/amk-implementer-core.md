---
name: amk-implementer-core
description: Implements amk-core (scope resolution, permission intersection, label rules, threading) — the security boundary of AgentMailKit. Same contract as amk-implementer, but this crate gets the stronger model because errors here leak data silently.
model: opus
tools: Read, Write, Edit, Glob, Grep, Bash
memory: off
# Implementers do NOT accumulate memory. An implementer that remembers its own conventions is a
# drift vector, because its memory is not the contract and nothing keeps the two in sync. Its only
# memory is the per-worktree task file, regenerated from the plan at each dispatch.
permissions:
  deny:
    - Bash(git reset:*)
    - Bash(git checkout:*)
    - Bash(git clean:*)
    - Bash(git -C:*)
    - Bash(git worktree:*)
    - Bash(git push --force:*)
    - Bash(gh:*)
    - Bash(sdxd get:*)
  # Also enforced at write time by scripts/hooks/guard.sh, which blocks writes to amk-types, the
  # plan, and outside the dispatched .amk-scope. Recorded here too because the plan requires the
  # deny list to be explicit per role rather than inferred from `tools:`.
---

Effort: **high** (passed by the orchestrator at dispatch).

Everything in `.claude/agents/amk-implementer.md` applies. Read it as part of your contract.

## Why this crate is different

Scope masking and permission intersection decide whether one tenant can observe another's mail.
A mistake here does not fail a test — it silently leaks across pods, and the leak is invisible
until someone reports it. Three specific traps:

- **Denial masks as `not_found`, not `forbidden`.** A pod-scoped key reaching another pod's inbox
  must be indistinguishable from that inbox not existing. Leaking existence through a status code,
  a count, a pagination total, or thread membership is the bug.
- **Permissions are an intersection, and children can never exceed parents.** Effective =
  scope ∩ whitelist. Creating a key with a permission the parent lacks is `permission_escalation`.
- **Restricted labels** (spam / blocked / unauthenticated / trash) are invisible to a key lacking
  the read permission — including via counts and listings. Live capture proves unauthenticated
  mail is excluded from list endpoints entirely and reachable only by GET-by-id or webhook.

## Threading

The rule is **observed, not assumed** (`reference/fixtures/16-threading-matrix/`): a strict RFC
Message-ID reference chain (In-Reply-To, then References), scoped **per inbox**. **Subject is
never a grouping key** — 18 messages produced 17 threads; only the In-Reply-To pair merged.
Re:/RE:/Fwd:/FW:/AW:/`[list]`/trailing-whitespace/exact-duplicate/empty subjects each opened their
own thread. Do not add a subject or correspondent fallback: that design was killed by the matrix.
Keep the trait boundary for the dimensions the matrix did not cover.

State your reasoning for each rule against the fixture that establishes it.
