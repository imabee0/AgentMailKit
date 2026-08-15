#!/usr/bin/env bash
# Obligation ledger. The plan is a contract; this is the part of it a machine can hold.
#
# WHY THIS EXISTS: the plan is ~600 lines with obligations embedded in prose, so "did we do that?"
# was answered from memory — and answered wrong eleven times before an audit caught it. One of the
# eleven had been masked by a second record of the same obligation that quietly re-dated it.
#
# THE RULE THIS ENFORCES: an obligation is recorded in exactly ONE place. A second record is not
# redundancy, it is somewhere for the two to disagree, and the one that disagrees quietly wins.
# Counts, versions, deferral tables and file inventories live here — never transcribed into prose.
#
# Exit 0 when every obligation that is DUE is met. Exit 1 otherwise. Obligations that cannot be
# machine-checked are printed as ATTEST lines rather than omitted: an omitted check reads as a
# passing one, which is how this project got eleven of them.
set -uo pipefail
cd "$(dirname "$0")/.."

# The single source of truth for where we are. Everything below keys its due-ness off this.
CURRENT_PHASE=P0

fail=0
ok()     { printf '  \033[32mMET\033[0m      %-38s %s\n' "$1" "$2"; }
bad()    { printf '  \033[31mSKIPPED\033[0m  %-38s %s\n' "$1" "$2"; fail=$((fail+1)); }
pend()   { printf '  PENDING  %-38s %s\n' "$1" "$2"; }
attest() { printf '  ATTEST   %-38s %s\n' "$1" "$2"; }

# check <id> <due:yes|no> <description> <command...>
check() {
  local id="$1" due="$2" desc="$3"; shift 3
  if "$@" >/dev/null 2>&1; then ok "$id" "$desc"
  elif [ "$due" = yes ]; then bad "$id" "$desc"
  else pend "$id" "$desc"; fi
}

echo "== plan ledger (phase $CURRENT_PHASE) =="

# ---------------------------------------------------------------- harness enforcement
check harness-guard-tests yes \
  "guard's own tests pass, both directions" \
  ./scripts/hooks/guard.test.sh

check harness-permissions-allow yes \
  "settings.json carries an allow list (not deny-only)" \
  python3 -c "import json;d=json.load(open('.claude/settings.json'));assert d['permissions']['allow']"

check harness-permissions-deny yes \
  "settings.json records deny explicitly" \
  python3 -c "import json;d=json.load(open('.claude/settings.json'));assert d['permissions']['deny']"

# THE CHECK THAT WAS A STRING MATCH, AND SO CHECKED NOTHING.
#
# This previously grepped each reviewer file for the literal "deny:" and passed. The files did
# contain `permissions: { deny: [...] }` — but `permissions:` is a settings.json construct and is
# NOT valid agent frontmatter, so the block bound nothing at all. Worse, the unsupported key
# plausibly cost the agents their registration: dispatching any of the five failed with "Agent type
# not found" while only the built-ins were listed.
#
# So the check now asserts the KEY THAT ACTUALLY BINDS, and the next one asserts that no
# frontmatter key outside the evidenced set can reappear. A check that matches a string the runtime
# never reads is indistinguishable from no check.
#
# Match the tool name as a WHOLE FIELD. A plain `grep Edit` reports MET on
# `disallowedTools: Write, NotebookEdit` — "Edit" is a substring of "NotebookEdit" — so the first
# version of this check passed a reviewer that could still call Edit. Found by mutating the check,
# not by reading it.
check harness-permissions-per-role yes \
  "each reviewer denies Write/Edit via disallowedTools (the key that binds)" \
  bash -c 'set -e; n=0; for f in .claude/agents/*review*.md; do
             d=$(sed -n "s/^disallowedTools:*//p" "$f" | tr "," "\n" | tr -d " ");
             printf "%s\n" "$d" | grep -qx "Write" || exit 1;
             printf "%s\n" "$d" | grep -qx "Edit"  || exit 1;
             n=$((n+1)); done; [ "$n" -ge 3 ]'

# The generalisation of that bug: an unsupported frontmatter key can cost an agent its
# registration, and a silently-unregistered agent means every dispatch quietly ran under the
# DEFAULT model, effort and tool set instead of the per-role ones the plan assigns. That failure is
# invisible from inside a dispatch. Allowlist the keys with positive evidence for this Claude Code
# version; anything else fails here rather than at dispatch.
check harness-agent-frontmatter yes \
  "agent frontmatter uses only evidenced keys" \
  bash -c 'for f in .claude/agents/*.md; do
             sed -n "2,/^---$/p" "$f" | grep -E "^[a-zA-Z][a-zA-Z]*:" | cut -d: -f1 |
               grep -qvE "^(name|description|model|tools|disallowedTools)$" && exit 1
           done; exit 0'

check harness-no-github yes \
  "no .github/ (Gitea only)" \
  bash -c '[ ! -d .github ]'

# ---------------------------------------------------------------- dispatch contracts
# 'Every implementer dispatch states, explicitly and in full' six things. Four of six is skipped.
for c in .claude/contracts/*.md; do
  id="contract-$(basename "$c" .md)"
  check "$id" yes "6 mandated dispatch elements present" bash -c "
    grep -q 'SPEC:' '$c' &&
    grep -qi 'writable' '$c' &&
    grep -q 'reference/fixtures/' '$c' &&
    grep -qi 'edge case' '$c' &&
    grep -qi 'prohibition' '$c' &&
    grep -qi 'STOP and report' '$c'"
done

# ---------------------------------------------------------------- evidence integrity
# THE MASKING CHECK. amk-types' fixture registry defers some captures to a later phase. That
# registry is a SECOND record of an obligation the plan also states — which is how C3's required
# code change hid: the plan said 'apply at the amk-store merge', the registry said 'P2', and the
# tripwire passed. The deferral table below is authoritative; the registry must agree with it.
DEFERRALS=$(cat <<'EOF'
00-probe-teardown.txt|operational ledger, not a wire shape
06-download-url-expiry.txt|amk-store signed downloads, P2
07-webhook-retry-curve.txt|amk-events retry engine, P4
09-event-payloads.txt|amk-events payload shapes, P4
09b-unauthenticated-variant.txt|amk-ingest labelling + list exclusion, P2
10-dkim-keys.txt|migration evidence, P6
10b-dkim-extraction.txt|migration evidence, P6
11-cp-smtp-relay.txt|migration evidence, P6
12-stalwart-dependents.txt|migration evidence, P6
13-source-ip-echo.txt|deployment evidence, P6
14-imap-crate-survey.txt|survey, no wire shape
15-compile-spike.txt|dependency pins, asserted by the build itself
16-threading-matrix|amk-core threading rules, P2
17-message-complained.txt|amk-events complaint payload, P4
20-search-and-label-precedence.txt|amk-core label access modes, P1
C1-domain-shape.txt|amk-types domain shapes, P5
EOF
)
if reg=$(python3 - <<'PY' 2>/dev/null
import re
src = open("crates/amk-types/tests/fixtures.rs").read()
block = src.split("DEFERRED: &[(&str, &str)] = &[", 1)[1].split("];", 1)[0]
for name, reason in re.findall(r'\("([^"]+)",\s*"([^"]+)"\)', block):
    print(f"{name}|{reason}")
PY
); then
  if [ "$reg" = "$DEFERRALS" ]; then
    ok evidence-deferral-table "fixture registry matches the ledger's table"
  else
    bad evidence-deferral-table "registry and ledger disagree — one obligation, two records"
    diff <(printf '%s\n' "$DEFERRALS") <(printf '%s\n' "$reg") | sed 's/^/      /'
  fi
else
  bad evidence-deferral-table "could not parse the fixture registry"
fi

# openapi.json is the upstream contract. 'Check its hash at each phase gate' needs a baseline to
# check against; without one the check cannot be performed even by someone who remembers it.
if [ -f reference/openapi.sha256 ]; then
  check evidence-openapi-hash yes "openapi.json matches its recorded hash" \
    bash -c 'cd reference && sha256sum -c openapi.sha256'
else
  bad evidence-openapi-hash "no reference/openapi.sha256 baseline recorded"
fi

# ---------------------------------------------------------------- crate obligations
# Register C3: closed by fixture 21, which disproved the shipped behaviour. The plan queued the
# code change for the amk-store merge; amk-store is merged.
check regc-c3-applied yes \
  "C3: unbracketed linkage headers are coerced (fixture 21)" \
  bash -c '! grep -q "an_unbracketed_linkage_header_is_not_coerced_into_a_match" crates/amk-core/src/threading.rs'

check deps-pinned-exactly yes \
  "workspace deps pinned to exact versions" \
  bash -c '! grep -E "^(tokio|serde|serde_json|chrono|uuid|thiserror|base64|percent-encoding|sqlx) *= *\{? *(version *= *)?\"[0-9]+\"" Cargo.toml'

# ---------------------------------------------------------------- hygiene
check hygiene-worktrees-swept yes \
  "no stale worktrees when no dispatch is in flight" \
  bash -c '[ -f .claude/fanout.lock ] || [ "$(git worktree list | wc -l)" -eq 1 ]'

check hygiene-lock-released yes \
  "fan-out lock is not left set with no worktree" \
  bash -c '[ ! -f .claude/fanout.lock ] || [ "$(git worktree list | wc -l)" -gt 1 ]'

# ---------------------------------------------------------------- not yet due
pend p0-gate-sdk-authme      "P0 gate: official Python SDK auth.me() vs localhost (needs amk-http)"
pend p1-gate-conformance     "P1 gate: dual-target conformance diff clean for P1 endpoints"
pend p6-restore-drill        "P6: restore drill passes from backups alone, before any cutover step"

# ---------------------------------------------------------------- cannot be machine-checked
# Listed, never omitted. An omitted check reads as a passing one.
attest review-panel-per-diff "three lenses returned on the last merged diff"
attest mutation-at-gate      "mutation set run for the crate at its gate; survivors accounted for"
attest evidence-not-assert   "every completion claim in the last report carried its command output"
attest ci-layer-decision     "CI layer: plan requires it, global rule needs opt-in — USER DECISION OPEN"
# Named rather than dropped. The plan DECIDES reviewers get memory ON and implementers OFF; the
# `memory:` frontmatter key is unverified for Claude Code 2.1.233 and an unsupported key can cost
# an agent its registration, so it is currently absent from all five files and the decision binds
# nothing. Verify the key in a fresh session, then add it to harness-agent-frontmatter's allowlist
# and to the reviewer files — in that order, so the allowlist can never be the thing that lags.
attest mem-subagent-memory   "subagent memory split: DECIDED in the plan, NOT bound — memory: key unverified on 2.1.233"

echo
if [ "$fail" -gt 0 ]; then
  printf 'plan-ledger: \033[31mFAIL\033[0m (%d due obligation(s) unmet)\n' "$fail"
  exit 1
fi
echo "plan-ledger: PASS"
