#!/usr/bin/env bash
# PreToolUse guard. Turns the plan's anti-drift rules from prompts into guarantees.
#
# A prompt is a request; a hook is a guarantee. Every rule here is one the plan states must hold,
# so it is enforced at write time rather than trusted to a subagent's judgement.
#
# Contract: reads the hook JSON on stdin, exits 2 to BLOCK (reason on stderr, shown to the model),
# exits 0 to allow. Runnable outside Claude Code for testing:
#     echo '{"tool_name":"Write","tool_input":{"file_path":"..."}}' | scripts/hooks/guard.sh
#     echo '{"toolName":"write","toolInput":{"target_file":"..."}}' | scripts/hooks/guard.sh
#
# How "is this a subagent?" is decided: by PATH, not by identity. Implementer subagents work inside
# .claude/worktrees/<id>/ or ~/.grok/worktrees/<repo>/<id>/; the orchestrator works in the primary
# checkout. That is an observable, deterministic discriminator that matches the project's actual
# isolation model — no guessing at session identity.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
# The plan lives IN the repo at docs/PLAN.md as of the 2026-08-17 GitHub migration, so that a
# session in Claude's cloud sandbox — which never sees ~/.claude — reads the same contract this
# machine does. The pre-migration external path stays matched because a stale copy still sits at
# ~/.claude/plans/ and must not become a second, quietly-diverging record of the same obligations.
PLAN_GLOB='plans/download-agents-mail-sdk-drifting-frog.md'
PLAN_IN_REPO='docs/PLAN.md'

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
# Grok sends camelCase keys and its own tool names; map onto the names the rules already match.
ti = d.get("tool_input") or d.get("toolInput") or {}
if not isinstance(ti, dict):
    ti = {}
tool = d.get("tool_name") or d.get("toolName") or ""
tool = {"run_terminal_command": "Bash", "search_replace": "Edit", "write": "Write"}.get(tool, tool)
path = ti.get("file_path") or ti.get("target_file") or ti.get("path") or ""
emit("TOOL", tool)
emit("FILE", path)
emit("CMD", ti.get("command"))
# Write carries content; Edit / search_replace carry new_string
emit("CONTENT", (ti.get("content") or "") + (ti.get("new_string") or ""))
emit("CWD", d.get("cwd"))
')"

TOOL="${TOOL:-}"; FILE="${FILE:-}"; CMD="${CMD:-}"; CONTENT="${CONTENT:-}"; CWD="${CWD:-}"

# Is the CALLER an implementer? For Bash there is no target path to key on, so this is CWD only --
# the same partial signal rule 0 exists because of. It is why the fan-out-lock half above is keyed
# on the lock rather than on identity, and why this half is the weaker of the two.
IN_WORKTREE_CMD=0
case "$CWD" in */.claude/worktrees/*|*/.grok/worktrees/*) IN_WORKTREE_CMD=1 ;; esac

# ---------------------------------------------------------------- Bash rules
if [ "$TOOL" = "Bash" ]; then
  # A refactor agent inheriting Bash has, in a reported incident, run `git reset` against a shared
  # checkout and destroyed uncommitted orchestrator work. Worktree-scoped agents get no history
  # rewriting and no redirecting git at another checkout.
  case "$CWD" in
    */.claude/worktrees/*|*/.grok/worktrees/*)
      case "$CMD" in
        *"git reset"*|*"git checkout"*|*"git clean"*|*"git -C"*|*"GIT_DIR="*|*"git worktree"*)
          deny "Implementer agents may not run history-rewriting or checkout-redirecting git.
Blocked command: $CMD
Commit on your own branch; the orchestrator merges. If you believe you need this, STOP and report."
          ;;
      esac
      ;;
  esac

  # ------------------------------------------------------------------ Bash writes to frozen paths
  #
  # Rules 0, 1, 2 and 4 below only ever see Write/Edit/NotebookEdit. An audit on 2026-08-19 pointed
  # out the consequence: `sed -i`, `cp`, `tee` and plain `>` redirection reach exactly the same
  # files and walked straight past every one of them. An agent forbidden to Edit crates/amk-types
  # could `echo ... >> crates/amk-types/src/ids.rs` and nothing fired.
  #
  # WHAT THIS IS AND IS NOT. Deciding "does this shell command write to path X" is undecidable in
  # general -- `python3 -c` with a base64 payload, a script written then executed, `eval` on a
  # constructed string. This does not attempt it and does not claim to. It requires TWO things
  # together: a write-shaped verb AND a frozen path named in the same command. That catches the
  # accidental case outright and raises the cost of the deliberate one from "type a different
  # command" to "write an obfuscator", which is a different kind of act and a visible one in a
  # transcript.
  #
  # Stated plainly so nobody reads more into it: this NARROWS the hole. The honest backstop is
  # still the orchestrator reviewing the returned diff before merging, exactly as the residual note
  # under rule 3 already says.
  #
  # Both conditions are required to keep false positives near zero: `grep amk-types` and
  # `cargo test -p amk-types` name a frozen path with no write verb, and `cp a.rs b.rs` is a write
  # verb naming nothing frozen. Neither trips.
  # Matched against a SPACE-PREFIXED copy. Without it the verb patterns have to choose between
  # `*" cp "*`, which misses a command starting with `cp`, and `*"cp "*`, which matches inside
  # `scp ` and `recp `. Prefixing normalises start-of-string to the same shape as mid-string, so
  # one pattern is both correct and anchored on a word boundary. (The first version of this rule
  # mixed the two forms and let `cp /tmp/x.rs crates/amk-types/src/ids.rs` through -- caught by
  # its own test, which is why the negative cases below exist.)
  bash_writes=0
  case " $CMD" in
    *">"*|*" sed -i"*|*" cp "*|*" mv "*|*" tee "*|*" install "*|*" truncate "*|*" dd "*|*" rm "*|*" chmod "*|*" touch "*|*" ln "*)
      bash_writes=1 ;;
  esac
  if [ "$bash_writes" -eq 1 ]; then
    # Frozen for EVERYONE while a dispatch is in flight -- the same set, and the same reasoning,
    # as rule 0. Derived from the lock, not from who is asking.
    if [ -f "$REPO/.claude/fanout.lock" ]; then
      case "$CMD" in
        *"crates/amk-types/"*|*"$PLAN_GLOB"*|*"$PLAN_IN_REPO"*|*".claude/plans/"*|*"scripts/hooks/"*)
          deny "A fan-out is IN FLIGHT ($REPO/.claude/fanout.lock exists) and this Bash command
appears to WRITE to a frozen path:

  $CMD

Frozen while dispatched: crates/amk-types/**, the plan, scripts/hooks/**.
The shell reaches the same files Write/Edit does; the rule does not change because the tool did.

If the change is genuinely needed now: stop the in-flight work, remove the lock, make the change,
and re-dispatch from the new base."
          ;;
      esac
    fi
    # Frozen for IMPLEMENTERS at all times, lock or no lock -- rules 1 and 2, via the shell.
    if [ "$IN_WORKTREE_CMD" -eq 1 ]; then
      case "$CMD" in
        *"crates/amk-types/"*)
          deny "amk-types is FROZEN for implementer agents (plan: five non-negotiables #2), and
that holds for the shell as much as for Edit:

  $CMD

If you need a type that does not exist, STOP and report -- do not add it."
          ;;
      esac
      case "$CMD" in
        *"$PLAN_GLOB"*|*"$PLAN_IN_REPO"*|*".claude/plans/"*|*"scripts/hooks/"*)
          deny "The plan and the hooks are ORCHESTRATOR-ONLY, through any tool:

  $CMD

An agent that can rewrite the guard can disable every other rule.
If the plan is wrong or ambiguous, STOP and report it -- do not edit it."
          ;;
      esac
    fi
  fi
  exit 0
fi

# ------------------------------------------------------- Write / Edit rules
case "$TOOL" in
  Write|Edit|NotebookEdit) ;;
  *) exit 0 ;;
esac
[ -n "$FILE" ] || exit 0

# Is this write subject to the implementer rules?
#
# Either the writer is working inside a worktree (CWD) or the write LANDS in one (FILE). Checking
# CWD alone left a hole: an agent whose shell sat in the primary checkout could write straight into
# a worktree and skip every rule below. Checking FILE alone would be wrong too — an implementer
# writing to an absolute path outside its worktree is exactly what rule 3 exists to catch. So:
# either condition makes the write an implementer write.
#
# The orchestrator merges by copying worktree files INTO the primary checkout, which writes primary
# paths from a primary CWD, and is unaffected.
IN_WORKTREE=0
case "$CWD" in */.claude/worktrees/*|*/.grok/worktrees/*) IN_WORKTREE=1 ;; esac
case "$FILE" in */.claude/worktrees/*|*/.grok/worktrees/*) IN_WORKTREE=1 ;; esac

# 0. THE FAN-OUT LOCK — the one rule that does not care who you are.
#
# CWD cannot actually identify a subagent: a subagent inherits the parent's working directory, so
# an implementer whose shell sits in the primary checkout is indistinguishable from the
# orchestrator. Measured: a dispatched implementer's write to <worktree>/scripts/check.sh was
# allowed, and replaying that payload showed the allow only happens when CWD is NOT in a worktree —
# so the payload carried the parent's cwd.
#
# (An earlier version of this comment cited that incident as proof of a pre-existing limitation.
# It was not: rule 3 had just been re-keyed from the target to the writer, and the previous,
# target-keyed version blocked that write at both cwds. The regression was self-inflicted. Rule 3
# is now keyed on both sides; this rule stands on its own reasoning, not on that incident.)
#
# So this rule drops identity entirely and enforces the plan's rule 2 as literally written: while a
# dispatch is in flight, the frozen paths are frozen for EVERYONE, orchestrator included. That is
# not a limitation of the check, it is the actual rule — a type change mid-fan-out invalidates
# every parallel worker's assumptions, and the orchestrator is the likeliest person to make one
# "quickly" while waiting for an agent to return.
#
# Lifecycle: the orchestrator creates .claude/fanout.lock at dispatch and removes it at merge.
# Removing it is a deliberate act; forgetting the rule is not.
if [ -f "$REPO/.claude/fanout.lock" ]; then
  case "$FILE" in
    */crates/amk-types/*|*"$PLAN_GLOB"|*"$PLAN_IN_REPO"|*/.claude/plans/*|*/scripts/hooks/*)
      deny "A fan-out is IN FLIGHT ($REPO/.claude/fanout.lock exists) and this path is frozen for
everyone — orchestrator included — until it completes:
  $FILE

Frozen while dispatched: crates/amk-types/**, the plan, scripts/hooks/**.
A type change mid-fan-out invalidates every parallel worker's assumptions.

If the change is genuinely needed now: stop the in-flight work, remove the lock, make the change,
and re-dispatch from the new base. That is the plan's rule 2, not a workaround."
      ;;
  esac
fi

# 1. The plan and its registers are orchestrator-only. A subagent that edits the contract it is
#    being judged against has silently redefined "correct".
case "$FILE" in
  *"$PLAN_GLOB"|*"$PLAN_IN_REPO"|*/.claude/plans/*)
    if [ "$IN_WORKTREE" -eq 1 ]; then
      deny "The plan is the contract and is ORCHESTRATOR-ONLY.
If the plan is wrong or ambiguous, STOP and report it — do not edit it."
    fi
    ;;
esac

# 2. Frozen types during fan-out. A type change mid-fan-out invalidates every parallel worker's
#    assumptions, so implementers never touch amk-types — the orchestrator makes the change and
#    workers restart from the new base.
case "$FILE" in
  */crates/amk-types/*)
    if [ "$IN_WORKTREE" -eq 1 ]; then
      deny "amk-types is FROZEN for implementer agents (plan: five non-negotiables #2).
Every other crate's correctness is downstream of these shapes.
If you need a type that does not exist, STOP and report — do not add it."
    fi
    ;;
esac

# 3. Per-worktree scope. The orchestrator writes .amk-scope at dispatch listing the paths this
#    agent may write.
#
#    Keyed on EITHER side, because each alone leaves a hole and both were actually hit:
#      * target-only -> never fires on an implementer writing an absolute path OUT of its worktree.
#      * writer-only -> never fires AT ALL for a subagent, because a subagent inherits the PARENT's
#        cwd. Measured, not assumed: a dispatched implementer wrote <worktree>/scripts/check.sh and
#        this rule was silent. Replaying that payload against each version of this file showed the
#        target-keyed version blocked it and the writer-keyed version did not.
#
#    So derive the worktree from CWD when the writer is inside one, else from the target path.
#
#    Dispatch ordering, which is what lets this stay strict: the orchestrator writes a worktree's
#    contract files BEFORE creating .amk-scope. The scope file's existence is what arms this rule,
#    so ordering solves the write-the-contract-in case with no exemption — and an exemption is
#    exactly what an agent would use to rewrite its own contract.
#
# Grok worktrees are ~/.grok/worktrees/<repo>/<id>/ (tests may use one level). Claude's
# cut -d/ -f1 would land on the repo-slug parent and miss .amk-scope at the real root.
grok_wt_root() {
  local p="${1:-}" next
  [ -n "$p" ] || return 1
  while :; do
    case "$p" in
      */.grok/worktrees/*) ;;
      *) return 1 ;;
    esac
    if [ -f "$p/.amk-scope" ] || [ -e "$p/.git" ]; then
      printf '%s' "$p"
      return 0
    fi
    next="${p%/*}"
    [ "$next" = "$p" ] && return 1
    p="$next"
  done
}

WT=""
case "$CWD" in
  */.claude/worktrees/*)
    WT="${CWD%%/.claude/worktrees/*}/.claude/worktrees/$(printf '%s' "${CWD#*/.claude/worktrees/}" | cut -d/ -f1)" ;;
  */.grok/worktrees/*)
    WT="$(grok_wt_root "$CWD")" ;;
  *)
    case "$FILE" in
      */.claude/worktrees/*)
        WT="${FILE%%/.claude/worktrees/*}/.claude/worktrees/$(printf '%s' "${FILE#*/.claude/worktrees/}" | cut -d/ -f1)" ;;
      */.grok/worktrees/*)
        WT="$(grok_wt_root "$FILE")" ;;
    esac ;;
esac
if [ -n "$WT" ] && [ -f "$WT/.amk-scope" ]; then
  case "$FILE" in
    "$WT"/*) REL="${FILE#"$WT"/}" ;;
    /*) deny "Write outside your worktree: $FILE
Your worktree is $WT. Everything you write goes inside it; the orchestrator merges.
If the contract requires touching another path, STOP and report." ;;
    # A relative path resolves against CWD, i.e. inside the worktree. Anything that climbs out of
    # it fails the pattern match below and is denied — fail closed.
    *) REL="$FILE" ;;
  esac
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
