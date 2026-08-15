#!/usr/bin/env bash
# Tests for the PreToolUse guard. A hook that has never been tested is a hook that silently
# stopped working — and this one is load-bearing for the anti-drift rules, so it gets both
# directions: violations must BLOCK (exit 2) and legitimate work must PASS (exit 0).
set -uo pipefail
cd "$(dirname "$0")"
GUARD=./guard.sh
WT=/home/imma/projects/AgentMailKit/.claude/worktrees/test-wt
ORCH=/home/imma/projects/AgentMailKit

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

echo "== MUST BLOCK (exit 2) =="
check 2 "subagent edits amk-types (frozen)" \
  "$(j Write "$WT/crates/amk-types/src/ids.rs" "$WT" "pub struct X;")"
check 2 "subagent edits the plan" \
  "$(j Edit "/home/imma/.claude/plans/download-agents-mail-sdk-drifting-frog.md" "$WT" "text")"
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

echo "== MUST PASS (exit 0) =="
check 0 "subagent writes its own crate" \
  "$(j Write "$WT/crates/amk-core/src/scope.rs" "$WT" "pub struct Scope;")"
check 0 "orchestrator edits amk-types" \
  "$(j Write "$ORCH/crates/amk-types/src/ids.rs" "$ORCH" "pub struct X;")"
check 0 "orchestrator edits the plan" \
  "$(j Edit "/home/imma/.claude/plans/download-agents-mail-sdk-drifting-frog.md" "$ORCH" "text")"
check 0 "comment mentioning Stalwart/JMAP is documentation" \
  "$(j Write "$WT/crates/amk-core/src/threading.rs" "$WT" "// Unlike JMAP, threading here is per-inbox.")"
check 0 "mail_parser inside amk-ingest is correct" \
  "$(j Write "$WT/crates/amk-ingest/src/parse.rs" "$WT" "use mail_parser::Message;")"
check 0 "ordinary git commit from a worktree" \
  "$(j Bash "" "$WT" "" "git commit -am 'feat: scope resolution'")"
check 0 "cargo test from a worktree" \
  "$(j Bash "" "$WT" "" "cargo test -p amk-core")"
check 0 "malformed payload does not block work" \
  "not json at all"

echo "== .amk-scope enforcement =="
mkdir -p "$WT"
printf 'crates/amk-core/*\ncrates/amk-core/src/*\n' > "$WT/.amk-scope"
check 0 "in-scope write allowed"  "$(j Write "$WT/crates/amk-core/src/scope.rs" "$WT" "pub struct S;")"
check 2 "out-of-scope write blocked" "$(j Write "$WT/crates/amk-store/src/lib.rs" "$WT" "pub struct S;")"
rm -f "$WT/.amk-scope"; rmdir "$WT" 2>/dev/null

printf '\nguard tests: %d passed, %d failed\n' "$pass" "$fail"
exit $(( fail > 0 ))
