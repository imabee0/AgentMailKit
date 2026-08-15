#!/usr/bin/env bash
# PreToolUse guard. Turns the plan's anti-drift rules from prompts into guarantees.
#
# A prompt is a request; a hook is a guarantee. Every rule here is one the plan states must hold,
# so it is enforced at write time rather than trusted to a subagent's judgement.
#
# Contract: reads the hook JSON on stdin, exits 2 to BLOCK (reason on stderr, shown to the model),
# exits 0 to allow. Runnable outside Claude Code for testing:
#     echo '{"tool_name":"Write","tool_input":{"file_path":"..."}}' | scripts/hooks/guard.sh
#
# How "is this a subagent?" is decided: by PATH, not by identity. Implementer subagents work inside
# .claude/worktrees/<id>/; the orchestrator works in the primary checkout. That is an observable,
# deterministic discriminator that matches the project's actual isolation model — no guessing at
# session identity.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
PLAN_GLOB='plans/download-agents-mail-sdk-drifting-frog.md'

read -r -d '' PAYLOAD || true

deny() { printf 'BLOCKED by scripts/hooks/guard.sh\n\n%s\n' "$1" >&2; exit 2; }

eval "$(printf '%s' "$PAYLOAD" | python3 -c '
import json, sys, shlex
def emit(k, v):
    sys.stdout.write(k + "=" + shlex.quote(v if v else "") + "\n")
try:
    d = json.load(sys.stdin)
except Exception:
    for k in ("TOOL", "FILE", "CMD", "CONTENT", "CWD"):
        emit(k, "")
    sys.exit(0)
ti = d.get("tool_input") or {}
emit("TOOL", d.get("tool_name"))
emit("FILE", ti.get("file_path"))
emit("CMD", ti.get("command"))
# Write carries content; Edit carries new_string
emit("CONTENT", (ti.get("content") or "") + (ti.get("new_string") or ""))
emit("CWD", d.get("cwd"))
')"

TOOL="${TOOL:-}"; FILE="${FILE:-}"; CMD="${CMD:-}"; CONTENT="${CONTENT:-}"; CWD="${CWD:-}"

# ---------------------------------------------------------------- Bash rules
if [ "$TOOL" = "Bash" ]; then
  # A refactor agent inheriting Bash has, in a reported incident, run `git reset` against a shared
  # checkout and destroyed uncommitted orchestrator work. Worktree-scoped agents get no history
  # rewriting and no redirecting git at another checkout.
  case "$CWD" in
    */.claude/worktrees/*)
      case "$CMD" in
        *"git reset"*|*"git checkout"*|*"git clean"*|*"git -C"*|*"GIT_DIR="*|*"git worktree"*)
          deny "Implementer agents may not run history-rewriting or checkout-redirecting git.
Blocked command: $CMD
Commit on your own branch; the orchestrator merges. If you believe you need this, STOP and report."
          ;;
      esac
      ;;
  esac
  exit 0
fi

# ------------------------------------------------------- Write / Edit rules
case "$TOOL" in
  Write|Edit|NotebookEdit) ;;
  *) exit 0 ;;
esac
[ -n "$FILE" ] || exit 0

# 1. The plan and its registers are orchestrator-only. A subagent that edits the contract it is
#    being judged against has silently redefined "correct".
case "$FILE" in
  *"$PLAN_GLOB"|*/.claude/plans/*)
    case "$CWD" in
      */.claude/worktrees/*)
        deny "The plan is the contract and is ORCHESTRATOR-ONLY.
If the plan is wrong or ambiguous, STOP and report it — do not edit it."
        ;;
    esac
    ;;
esac

# 2. Frozen types during fan-out. A type change mid-fan-out invalidates every parallel worker's
#    assumptions, so implementers never touch amk-types — the orchestrator makes the change and
#    workers restart from the new base.
case "$FILE" in
  */crates/amk-types/*)
    case "$CWD" in
      */.claude/worktrees/*)
        deny "amk-types is FROZEN for implementer agents (plan: five non-negotiables #2).
Every other crate's correctness is downstream of these shapes.
If you need a type that does not exist, STOP and report — do not add it."
        ;;
    esac
    ;;
esac

# 3. Per-worktree scope. The orchestrator writes .amk-scope at dispatch listing the paths this
#    agent may write. Enforced only when present, so an un-dispatched worktree is not bricked.
case "$FILE" in
  */.claude/worktrees/*)
    WT="${FILE%%/.claude/worktrees/*}/.claude/worktrees/$(printf '%s' "${FILE#*/.claude/worktrees/}" | cut -d/ -f1)"
    if [ -f "$WT/.amk-scope" ]; then
      REL="${FILE#"$WT"/}"
      ok=0
      while IFS= read -r pat; do
        [ -z "$pat" ] && continue
        case "$pat" in \#*) continue ;; esac
        # shellcheck disable=SC2254
        case "$REL" in $pat) ok=1; break ;; esac
      done < "$WT/.amk-scope"
      [ "$ok" -eq 1 ] || deny "Path outside this agent's dispatched scope: $REL
Allowed (from $WT/.amk-scope):
$(sed 's/^/  /' "$WT/.amk-scope")
Write only within your crate. If the contract requires touching another path, STOP and report."
    fi
    ;;
esac

# 4. Boundary types. mail_parser/mail_auth/mail_send/smtp_proto are ergonomic and right there,
#    which makes them the likeliest accidental leak into the shapes that define our contract.
#    This is the P0 CI check, enforced at write time so the violation never reaches CI.
case "$FILE" in
  */crates/amk-types/*|*/crates/amk-core/*|*/crates/amk-store/*)
    if printf '%s' "$CONTENT" | grep -qE '(mail_parser|mail_auth|mail_send|mail_builder|smtp_proto)::'; then
      deny "stalwart-labs crate type in amk-types/amk-core/amk-store.
Those crates live only in amk-ingest/amk-outbound and are converted at the boundary.
Shape provenance: every type here derives from AgentMail's artifacts, never from Stalwart."
    fi
    # Strip comment lines FIRST, then look for the concept in what remains. Testing the whole
    # payload for "does any line look like a comment" exempts every Rust file ever written, since
    # they all carry doc comments — the check would have passed anything.
    code_only=$(printf '%s\n' "$CONTENT" | grep -vE '^\s*(//|///|//!|\*|/\*)' || true)
    if printf '%s' "$code_only" | grep -qiE '(jmap|sieve|rocksdb|mailbox_?role)'; then
      deny "Stalwart/JMAP concept in a protected crate:
$(printf '%s' "$code_only" | grep -inE '(jmap|sieve|rocksdb|mailbox_?role)' | head -5 | sed 's/^/    /')
These crates derive from AgentMail's artifacts only — not even as an optional or legacy field.
(A comment contrasting with Stalwart is fine; this was code.)"
    fi
    ;;
esac

exit 0
