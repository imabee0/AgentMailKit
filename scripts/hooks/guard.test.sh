#!/usr/bin/env bash
# Tests for the PreToolUse guard. A hook that has never been tested is a hook that silently
# stopped working — and this one is load-bearing for the anti-drift rules, so it gets both
# directions: violations must BLOCK (exit 2) and legitimate work must PASS (exit 0).
#
# HERMETIC BY CONSTRUCTION. The guard derives its repo root — and therefore the fan-out lock path —
# from its own location ($0/../..), so an earlier version of this suite tested the guard against
# the REAL .claude/fanout.lock. That made the result depend on ambient state: while a dispatch was
# in flight, every "MUST PASS" case touching a frozen path failed, and the suite could not be run
# at exactly the moment its guarantees mattered most. Worse, its lock-section cleanup was
# conditional, so a crash mid-run could have deleted a live lock and silently unfroze the project.
#
# So the suite now copies the guard into a throwaway repo root and drives the lock there. Nothing
# is written inside the project, no real lock is read or touched, and the verdict is the same
# whether or not a fan-out is running.
set -uo pipefail
cd "$(dirname "$0")" || { echo "FATAL: cannot cd to the hooks directory" >&2; exit 1; }
SRC="$PWD/guard.sh"

ROOT="$(mktemp -d)/repo"
trap 'rm -rf "$(dirname "$ROOT")"' EXIT
mkdir -p "$ROOT/scripts/hooks" "$ROOT/.claude"
cp "$SRC" "$ROOT/scripts/hooks/guard.sh" || { echo "cannot stage the guard under test"; exit 1; }

GUARD="$ROOT/scripts/hooks/guard.sh"
ORCH="$ROOT"
WT="$ROOT/.claude/worktrees/test-wt"
GWT="$ROOT/.grok/worktrees/test-gwt"
LOCK="$ROOT/.claude/fanout.lock"
PLAN=/home/imma/.claude/plans/download-agents-mail-sdk-drifting-frog.md

pass=0; fail=0
check() { # check <expected-exit> <name> <json>
  local want="$1" name="$2" json="$3" got
  printf '%s' "$json" | $GUARD >/dev/null 2>&1; got=$?
  if [ "$got" -eq "$want" ]; then pass=$((pass+1)); printf '  ok    %s\n' "$name"
  else fail=$((fail+1)); printf '  FAIL  %s (want exit %s, got %s)\n' "$name" "$want" "$got"; fi
}

j() { # j <tool> <file> <cwd> [content] [cmd]
  python3 - "$@" <<'PY'
import json,sys
t,f,c = sys.argv[1],sys.argv[2],sys.argv[3]
content = sys.argv[4] if len(sys.argv)>4 else ""
cmd     = sys.argv[5] if len(sys.argv)>5 else ""
ti={}
if f: ti["file_path"]=f
if content: ti["content"]=content
if cmd: ti["command"]=cmd
print(json.dumps({"tool_name":t,"tool_input":ti,"cwd":c}))
PY
}

# Grok shape only: camelCase keys, Grok tool names, no tool_name/tool_input.
# File key defaults to target_file (not Claude's file_path); pass a 6th arg for path/file_path.
jg() { # jg <tool> <file> <cwd> [content] [cmd] [file_key]
  python3 - "$@" <<'PY'
import json,sys
t,f,c = sys.argv[1],sys.argv[2],sys.argv[3]
content = sys.argv[4] if len(sys.argv)>4 else ""
cmd     = sys.argv[5] if len(sys.argv)>5 else ""
fkey    = sys.argv[6] if len(sys.argv)>6 else "target_file"
ti={}
if f: ti[fkey]=f
if content:
    ti["new_string" if t == "search_replace" else "content"] = content
if cmd: ti["command"]=cmd
print(json.dumps({"toolName":t,"toolInput":ti,"cwd":c}))
PY
}

echo "== MUST BLOCK (exit 2) =="
check 2 "subagent edits amk-types (frozen)" \
  "$(j Write "$WT/crates/amk-types/src/ids.rs" "$WT" "pub struct X;")"
check 2 "subagent edits the plan" \
  "$(j Edit "$PLAN" "$WT" "text")"
# The plan moved INTO the repo (docs/PLAN.md) at the GitHub migration so a cloud-sandbox session
# reads the same contract. Rule 1 has to follow it there, or "orchestrator-only" silently became
# "orchestrator-only at a path nobody uses any more".
check 2 "subagent edits the in-repo plan (docs/PLAN.md)" \
  "$(j Edit "$ORCH/docs/PLAN.md" "$WT" "text")"
check 2 "mail_parser type into amk-core" \
  "$(j Write "$WT/crates/amk-core/src/threading.rs" "$WT" "use mail_parser::Message;")"
check 2 "mail_auth type into amk-store" \
  "$(j Write "$ORCH/crates/amk-store/src/lib.rs" "$ORCH" "fn v() -> mail_auth::DkimResult {}")"
check 2 "JMAP concept in amk-core code" \
  "$(j Write "$WT/crates/amk-core/src/labels.rs" "$WT" "struct JmapMailboxRole;")"
check 2 "git reset from a worktree" \
  "$(j Bash "" "$WT" "" "git reset --hard HEAD~1")"
check 2 "git -C redirect from a worktree" \
  "$(j Bash "" "$WT" "" "git -C /home/imma/projects/AgentMailKit commit -am wip")"

# The hole that CWD-only detection left: a writer sitting in the primary checkout writing INTO a
# worktree skipped every implementer rule.
check 2 "amk-types write INTO a worktree from a primary-checkout cwd" \
  "$(j Write "$WT/crates/amk-types/src/ids.rs" "$ORCH" "pub struct X;")"
check 2 "plan write INTO a worktree path from a primary cwd" \
  "$(j Edit "$WT/.claude/plans/download-agents-mail-sdk-drifting-frog.md" "$ORCH" "text")"

echo "== MUST PASS (exit 0) =="
check 0 "subagent writes its own crate" \
  "$(j Write "$WT/crates/amk-core/src/scope.rs" "$WT" "pub struct Scope;")"
check 0 "orchestrator edits amk-types" \
  "$(j Write "$ORCH/crates/amk-types/src/ids.rs" "$ORCH" "pub struct X;")"
check 0 "orchestrator edits the plan" \
  "$(j Edit "$PLAN" "$ORCH" "text")"
check 0 "orchestrator edits the in-repo plan (docs/PLAN.md)" \
  "$(j Edit "$ORCH/docs/PLAN.md" "$ORCH" "text")"
check 0 "comment mentioning Stalwart/JMAP is documentation" \
  "$(j Write "$WT/crates/amk-core/src/threading.rs" "$WT" "// Unlike JMAP, threading here is per-inbox.")"
check 0 "doc comment mentioning JMAP alongside real code" \
  "$(j Write "$WT/crates/amk-core/src/threading.rs" "$WT" "//! Unlike JMAP, per-inbox.
pub struct ThreadIndex;")"
# The regression that motivated stripping comments per-line: a real file carries doc comments AND
# code, and the old check exempted the whole payload the moment any line looked like a comment.
check 2 "JMAP in code is caught even when the file also has doc comments" \
  "$(j Write "$WT/crates/amk-core/src/labels.rs" "$WT" "//! Labels module.
/// Role of a mailbox.
pub struct JmapMailboxRole;")"
check 0 "mail_parser inside amk-ingest is correct" \
  "$(j Write "$WT/crates/amk-ingest/src/parse.rs" "$WT" "use mail_parser::Message;")"
check 0 "ordinary git commit from a worktree" \
  "$(j Bash "" "$WT" "" "git commit -am 'feat: scope resolution'")"
check 0 "cargo test from a worktree" \
  "$(j Bash "" "$WT" "" "cargo test -p amk-core")"
check 0 "malformed payload does not block work" \
  "not json at all"

echo "== fan-out lock (identity-independent: freezes everyone) =="
# Driven against the staged root, never the project's own lock — see the header.
touch "$LOCK"
check 2 "orchestrator cannot edit amk-types while a dispatch is in flight" \
  "$(j Write "$ORCH/crates/amk-types/src/api_key.rs" "$ORCH" "pub struct ApiKey;")"
check 2 "orchestrator cannot edit the plan while a dispatch is in flight" \
  "$(j Edit "$PLAN" "$ORCH" "text")"
check 2 "nobody can edit the guard itself while a dispatch is in flight" \
  "$(j Edit "$ORCH/scripts/hooks/guard.sh" "$ORCH" "exit 0")"
check 0 "ordinary crate work is unaffected by the lock" \
  "$(j Write "$ORCH/crates/amk-store/src/lib.rs" "$ORCH" "pub struct S;")"
rm -f "$LOCK"
check 0 "amk-types is editable again once the lock is gone" \
  "$(j Write "$ORCH/crates/amk-types/src/api_key.rs" "$ORCH" "pub struct ApiKey;")"

# The suite must not be able to pass by accident because it happened to run in a project with no
# lock: assert the lock rule actually fires on the staged root before trusting the section above.
touch "$LOCK"
check 2 "lock rule is armed by the staged root, not the project's" \
  "$(j Write "$ORCH/crates/amk-types/src/ids.rs" "$ORCH" "pub struct X;")"
rm -f "$LOCK"

echo "== .amk-scope enforcement =="
mkdir -p "$WT"
printf 'crates/amk-core/*\ncrates/amk-core/src/*\n' > "$WT/.amk-scope"
check 0 "in-scope write allowed"  "$(j Write "$WT/crates/amk-core/src/scope.rs" "$WT" "pub struct S;")"
check 2 "out-of-scope write blocked" "$(j Write "$WT/crates/amk-store/src/lib.rs" "$WT" "pub struct S;")"
# Scope is a property of the WRITER, so it must catch an implementer escaping its worktree
# entirely — and must NOT catch the orchestrator writing the dispatch contract in.
check 2 "implementer writes out of its worktree into the primary checkout" \
  "$(j Write "$ORCH/crates/amk-core/src/scope.rs" "$WT" "pub struct S;")"
check 2 "implementer climbs out with a relative path" \
  "$(j Write "../../../etc/passwd" "$WT" "root::0:0")"
# The hole that keying rule 3 on the WRITER alone left open. A subagent inherits the parent's cwd,
# so this is the normal case for a dispatched implementer, not an edge case — it was measured
# against a real dispatch before being fixed.
check 2 "subagent (cwd=primary) writes into a worktree OUT of scope" \
  "$(j Write "$WT/scripts/check.sh" "$ORCH" "echo hi")"
check 0 "subagent (cwd=primary) writes into a worktree IN scope" \
  "$(j Write "$WT/crates/amk-core/src/scope.rs" "$ORCH" "pub struct S;")"
# ...and the case that keying it on the TARGET alone left open, kept closed.
check 2 "implementer escapes its worktree to the primary checkout" \
  "$(j Write "$ORCH/crates/amk-core/src/labels.rs" "$WT" "pub struct S;")"
# Ordering, not an exemption, is what lets the orchestrator write a dispatch contract: .amk-scope
# arms the rule, so contract files are written BEFORE it exists. An exemption here is precisely
# what an agent would use to rewrite its own contract.
rm -f "$WT/.amk-scope"
check 0 "orchestrator writes the dispatch contract before .amk-scope exists" \
  "$(j Write "$WT/CLAUDE.md" "$ORCH" "# amk-store contract")"

echo "== Grok payload + grok worktree (same outcomes; no Claude keys) =="
check 2 "grok write to amk-types from a grok worktree" \
  "$(jg write "$GWT/crates/amk-types/src/ids.rs" "$GWT" "pub struct X;")"
check 2 "grok search_replace to amk-types from a grok worktree" \
  "$(jg search_replace "$GWT/crates/amk-types/src/ids.rs" "$GWT" "pub struct X;")"
check 2 "grok write to amk-types via path key from a grok worktree" \
  "$(jg write "$GWT/crates/amk-types/src/ids.rs" "$GWT" "pub struct X;" "" path)"
check 2 "grok write to amk-types via file_path key from a grok worktree" \
  "$(jg write "$GWT/crates/amk-types/src/ids.rs" "$GWT" "pub struct X;" "" file_path)"
check 2 "grok run_terminal_command git reset from a grok worktree" \
  "$(jg run_terminal_command "" "$GWT" "" "git reset --hard HEAD~1")"
check 2 "grok write to amk-types (Grok payload, Claude worktree)" \
  "$(jg write "$WT/crates/amk-types/src/ids.rs" "$WT" "pub struct X;")"
mkdir -p "$GWT"
printf 'crates/amk-core/*\ncrates/amk-core/src/*\n' > "$GWT/.amk-scope"
check 0 "grok in-scope write under a grok worktree" \
  "$(jg write "$GWT/crates/amk-core/src/scope.rs" "$GWT" "pub struct S;")"
check 2 "grok out-of-scope write under a grok worktree" \
  "$(jg write "$GWT/crates/amk-store/src/lib.rs" "$GWT" "pub struct S;")"

# Production Grok layout is two levels: ~/.grok/worktrees/<repo>/<subagent-id>/.
# .amk-scope lives only at that second level — never on the repo-slug parent.
GWT2="$ROOT/.grok/worktrees/projects-agentmailkit/subagent-test"
mkdir -p "$GWT2/crates/amk-core/src"
printf 'crates/amk-core/*\ncrates/amk-core/src/*\n' > "$GWT2/.amk-scope"
check 0 "grok two-level worktree in-scope write" \
  "$(jg write "$GWT2/crates/amk-core/src/scope.rs" "$GWT2" "pub struct S;")"
check 2 "grok two-level worktree out-of-scope write" \
  "$(jg write "$GWT2/crates/amk-store/src/lib.rs" "$GWT2" "pub struct S;")"
check 2 "grok two-level worktree escape to primary" \
  "$(jg write "$ORCH/crates/amk-core/src/scope.rs" "$GWT2" "pub struct S;")"
check 2 "grok two-level out-of-scope from nested cwd" \
  "$(jg write "$GWT2/crates/amk-store/src/lib.rs" "$GWT2/crates/amk-core/src" "pub struct S;")"
check 2 "grok two-level (cwd=primary) write into worktree OUT of scope" \
  "$(jg write "$GWT2/crates/amk-store/src/lib.rs" "$ORCH" "pub struct S;")"
check 0 "grok two-level (cwd=primary) write into worktree IN scope" \
  "$(jg write "$GWT2/crates/amk-core/src/scope.rs" "$ORCH" "pub struct S;")"

# ---------------------------------------------------------------- Bash writes to frozen paths
# Rules 0/1/2/4 only ever saw Write|Edit|NotebookEdit, so `sed -i`, `cp`, `tee` and `>` reached the
# same files untouched. These cover the narrowing, and -- more importantly -- the NEGATIVE side:
# an over-broad version of this rule would block ordinary shell work in a worktree, which is worse
# than the hole, because a guard that blocks legitimate work gets disabled.

echo
echo "-- Bash writes: implementer, no lock --"
check 2 "sed -i into amk-types is blocked" \
  "$(j Bash "" "$WT" "" "sed -i 's/a/b/' crates/amk-types/src/ids.rs")"
check 2 "append-redirect into amk-types is blocked" \
  "$(j Bash "" "$WT" "" "echo 'pub struct X;' >> crates/amk-types/src/ids.rs")"
check 2 "cp over an amk-types file is blocked" \
  "$(j Bash "" "$WT" "" "cp /tmp/x.rs crates/amk-types/src/ids.rs")"
check 2 "tee into amk-types is blocked" \
  "$(j Bash "" "$WT" "" "cat /tmp/x | tee crates/amk-types/src/ids.rs")"
check 2 "rm of an amk-types file is blocked" \
  "$(j Bash "" "$WT" "" "rm crates/amk-types/src/ids.rs")"
check 2 "an implementer rewriting the guard is blocked" \
  "$(j Bash "" "$WT" "" "sed -i 's/deny/allow/' scripts/hooks/guard.sh")"
check 2 "an implementer rewriting the plan is blocked" \
  "$(j Bash "" "$WT" "" "echo x >> docs/PLAN.md")"

echo "-- Bash writes: the NEGATIVE side (must PASS) --"
check 0 "reading amk-types is not a write" \
  "$(j Bash "" "$WT" "" "grep -rn TypeName crates/amk-types/src")"
check 0 "testing amk-types is not a write" \
  "$(j Bash "" "$WT" "" "cargo test -p amk-types")"
check 0 "writing inside the implementer's own crate is fine" \
  "$(j Bash "" "$WT" "" "sed -i 's/a/b/' crates/amk-ingest/src/smtp.rs")"
check 0 "a write verb naming nothing frozen is fine" \
  "$(j Bash "" "$WT" "" "cp /tmp/a.rs /tmp/b.rs")"
check 0 "the orchestrator may edit amk-types from the primary checkout" \
  "$(j Bash "" "$ORCH" "" "sed -i 's/a/b/' crates/amk-types/src/ids.rs")"

echo "-- Bash writes: the fan-out lock freezes EVERYONE, orchestrator included --"
: > "$LOCK"
check 2 "orchestrator sed -i into amk-types is blocked while a dispatch is live" \
  "$(j Bash "" "$ORCH" "" "sed -i 's/a/b/' crates/amk-types/src/ids.rs")"
check 2 "orchestrator redirect into the plan is blocked while a dispatch is live" \
  "$(j Bash "" "$ORCH" "" "echo note >> docs/PLAN.md")"
check 0 "reading amk-types is still fine while a dispatch is live" \
  "$(j Bash "" "$ORCH" "" "grep -rn TypeName crates/amk-types/src")"
rm -f "$LOCK"
check 0 "orchestrator may edit amk-types again once the lock is gone" \
  "$(j Bash "" "$ORCH" "" "sed -i 's/a/b/' crates/amk-types/src/ids.rs")"

printf '\nguard tests: %d passed, %d failed\n' "$pass" "$fail"
exit $(( fail > 0 ))
