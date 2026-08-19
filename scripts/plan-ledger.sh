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
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

# Is a gate transcript still evidence about THIS tree?
#
# The p*-gate-* checks grep a committed transcript. That was itself the fix for a worse defect (a
# `pend` wired statically to PENDING, which no implementation could ever flip), but it trades
# "can never pass" for "passes forever": the file says the gate was clean once, and keeps saying
# so after the code it gated has been rewritten underneath it.
#
# Freshness is asked of git rather than of a convention: if the code a gate covers was last
# touched AFTER the transcript was last committed, the transcript describes a tree that no longer
# exists. Keyed on COMMIT times, not file mtimes -- a fresh checkout gives every file the same
# mtime, which would make this silently true everywhere. No new convention is demanded of
# transcript authors: an earlier draft wanted a `gated-at: <sha>` line in each fixture, but only
# fixture 28 records a commit at all today, so that would have been a rule enforced retroactively
# against four files that predate it.
gate_evidence_fresh() { # $1 = transcript path; $2.. = the code paths it gates
  local fx="$1"; shift
  local fx_at code_at
  fx_at=$(git log -1 --format=%ct -- "$fx" 2>/dev/null)
  [ -n "$fx_at" ] || return 1          # uncommitted/unknown: cannot be dated, cannot be trusted
  code_at=$(git log -1 --format=%ct -- "$@" 2>/dev/null)
  [ -n "$code_at" ] || return 0        # gated code has no history yet; nothing can have invalidated it
  [ "$fx_at" -ge "$code_at" ]
}

# The single source of truth for where we are -- DERIVED, never transcribed.
#
# This was `CURRENT_PHASE=P0`, a hand-edited literal, and it still read P0 while P2's
# message/thread surface, outbound send and SMTP ingest had all merged. Three other documents
# disagreed with it and with each other: README.md said P1, docs/RESUME.md said P2, and
# docs/execute-plan-v1.md said `amk-ingest` did not exist (it merged at 28e6afa). Four records of
# one fact, in the file whose whole purpose is that an obligation has exactly one.
#
# Computed from the same gate fixtures the p*-gate-* checks read, so there is one body of evidence
# and not a second opinion about it. Delete a gate transcript and the phase moves back.
#
# CONTENT ONLY, deliberately -- freshness is a different question and belongs to the individual
# gates, which report it as STALE. An earlier revision applied freshness here too, for consistency,
# and the result was useless: any edit under crates/amk-http rewound this to P-1, so the furthest
# verified point read as "nothing has been done" every time somebody touched a handler. A gate that
# ran and was clean is a historical fact; it does not un-happen because the code moved on. What
# moving on means is "re-run it", which is exactly what STALE says, next to the gate it concerns.
#
# Lane R (the credentialed conformance diff) is what advances a phase, per docs/PLAN.md's own lane
# split: Lane L green makes a phase code-complete, not gated.
derive_current_phase() {
  local f
  # P0: the official Python SDK drove auth.me() against our own server.
  f=reference/fixtures/24-p0-gate-sdk-authme.txt
  [ -f "$f" ] && grep -q "organization_id" "$f" || { echo "P-1"; return; }
  # P1: Lane R -- the dual-target diff against the live reference, clean.
  f=reference/fixtures/25-p1-gate-conformance.txt
  [ -f "$f" ] && grep -q "0 skipped, 0 with structural diffs" "$f" || { echo "P0"; return; }
  # P2: Lane R, this phase's endpoints. Not captured yet; when it lands, add its fixture HERE
  # rather than editing a literal anywhere.
  f=reference/fixtures/29-p2-conformance.txt
  [ -f "$f" ] && grep -q "dual_target.py exit: 0" "$f" || { echo "P1"; return; }
  echo "P2"
}
CURRENT_PHASE=$(derive_current_phase)

fail=0
emitted=""
ok()     { emitted="$emitted $1"; printf '  \033[32mMET\033[0m      %-38s %s\n' "$1" "$2"; }
bad()    { emitted="$emitted $1"; printf '  \033[31mSKIPPED\033[0m  %-38s %s\n' "$1" "$2"; fail=$((fail+1)); }
pend()   { emitted="$emitted $1"; printf '  PENDING  %-38s %s\n' "$1" "$2"; }
# STALE: this gate WAS run and WAS clean, and the code it covers has changed since. Distinct from
# PENDING (never run) and MET (current), because the three call for different actions -- run it,
# re-run it, nothing. Yellow, and never rendered as MET, so it cannot be skim-read as a pass. It
# does not fail the build: nothing is known broken, and a Lane R gate needs a credential this
# machine may not have.
stale()  { emitted="$emitted $1"; printf '  \033[33mSTALE\033[0m    %-38s %s\n' "$1" "$2 [re-run: code moved since the transcript]"; }

# gate_check <id> <desc> <transcript> <content-fn> <code paths...>
# Splits "was it clean" from "is it still current" so each is reported as itself.
gate_check() {
  local id="$1" desc="$2" fx="$3" content="$4"; shift 4
  if ! "$content"; then pend "$id" "$desc"; return; fi
  if gate_evidence_fresh "$fx" "$@"; then ok "$id" "$desc"; else stale "$id" "$desc"; fi
}
attest() { emitted="$emitted $1"; printf '  ATTEST   %-38s %s\n' "$1" "$2"; }

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
#
# The key-extraction pattern must accept ANY key shape, not just pure letters. Its first version
# was `^[a-zA-Z][a-zA-Z]*:`, which never extracted `max_tokens:` or `top-p:` at all — so an
# unsupported key containing a digit, underscore or hyphen was invisible to the allowlist and the
# check reported MET. That is precisely the class of key most likely to be a real-but-unsupported
# frontmatter field, i.e. the exact thing this check exists to catch. Extract permissively, then
# judge against the allowlist; never filter before judging.
check harness-agent-frontmatter yes \
  "agent frontmatter uses only evidenced keys" \
  bash -c 'for f in .claude/agents/*.md; do
             sed -n "2,/^---$/p" "$f" | grep -E "^[a-zA-Z][a-zA-Z0-9_-]*:" | cut -d: -f1 |
               grep -qvE "^(name|description|model|tools|disallowedTools)$" && exit 1
           done; exit 0'

# `harness-no-github` ("no .github/, Gitea only") was RETIRED at the 2026-08-17 GitHub migration.
# It asserted the absence of the whole directory, which was only ever a proxy for "this project is
# not on GitHub" — a premise the user reversed. Kept as a comment rather than deleted silently
# because a check that vanishes and a check that never existed are indistinguishable later.
#
# What it was actually protecting is the no-CI decision, and `ci-layer-local-only` below already
# holds that, keyed on the workflow directories rather than on the forge. Re-adding a second check
# here would give one obligation two records, which is the failure mode the ledger exists to
# prevent: the two disagree eventually, and the one that disagrees quietly wins. `.github/` may now
# hold non-CI GitHub metadata (issue templates, CODEOWNERS); `.github/workflows/` may not.

# `ci-layer-local-only` ("no forge CI — local-only gating, decided") was RETIRED 2026-08-19, and
# is kept as a comment for the same reason `harness-no-github` above is: a check that vanishes and
# a check that never existed are indistinguishable later.
#
# It asserted `[ ! -d .gitea/workflows ] && [ ! -d .github/workflows ]`, pinning the user's
# 2026-08-15 decision that this project has no forge CI. That decision rested on a premise — one
# operator, one machine, every gate run by the person reading its output — and the 2026-08-17
# GitHub migration ended it: the repository moved specifically so cloud sandboxes could drive it,
# and a gate that runs in the same session as the agent it gates is not an independent gate.
#
# What the premise cost, measured on 2026-08-19 rather than argued:
#   - 336 of 705 tests (48%) skipped themselves without Postgres while `check.sh` printed PASS;
#   - `main` sat RED on `every_fixture_is_either_asserted_or_explicitly_deferred` from 0e7d345
#     onward, because the fixture landed in a docs-only commit and nobody re-ran the suite;
#   - `amkd --role api` could not send a single message, with every local gate green.
# All three are the same shape: a verification that nothing independent re-ran.
#
# The decision is REVERSED, not bypassed. `.github/workflows/` now holds ci/release/nightly/
# conformance, and the checks below (`ci-workflows-present`) assert they exist rather than that
# they do not — the obligation moved, it was not dropped.

# The inverse of what `ci-layer-local-only` used to assert. Deleting the pipeline is now the
# regression, so it is what fails the build. `ci-ok` is named explicitly because it is the single
# required status check: a workflow that stopped defining it would leave branch protection
# gating on a job that never reports.
check ci-workflows-present yes \
  "forge CI exists: ci/release/nightly/conformance, with ci-ok as the required check" \
  bash -c '
    for w in ci release nightly conformance; do
      [ -f ".github/workflows/$w.yml" ] || exit 1
    done
    [ -f .github/actions/setup-rust/action.yml ] || exit 1
    grep -q "^  ci-ok:" .github/workflows/ci.yml'

# Every third-party action pinned to a full 40-hex commit SHA. A tag is mutable: `@v4` is whatever
# the tag points at today, which is a supply-chain hole a compromised action repository walks
# straight through. Asserted mechanically because it is exactly the discipline that erodes when
# someone adds "just one" action in a hurry.
check ci-actions-sha-pinned yes \
  "every third-party action is pinned to a commit SHA, not a tag" \
  bash -c '
    bad=$(grep -rhoE "uses: [^ ]+@[^ ]+" .github/workflows .github/actions \
          | grep -v "uses: \./" \
          | grep -vE "@[0-9a-f]{40}$" || true)
    [ -z "$bad" ] || { printf "%s\n" "$bad"; exit 1; }'

# ---------------------------------------------------------------- dispatch contracts
# 'Every implementer dispatch states, explicitly and in full' six things. Four of six is skipped.
#
# Matched against the file FLATTENED to one line, because the first version matched raw lines and
# a compliant contract failed it: markdown soft-wrap had split '**STOP and\n  report**' across a
# newline, so the mandated element was present to every reader and absent to grep.
#
# The text assertion is separate and comes first. One literal NUL byte, inside a quoted example of
# the very input the id-safety dispatch is about, made GNU grep treat the contract as binary and
# return no matches for ANY of the six — reporting six missing elements instead of one bad byte.
# It failed closed, which is right, but named the wrong defect.
for c in .claude/contracts/*.md; do
  id="contract-$(basename "$c" .md)"
  check "$id" yes "6 mandated dispatch elements present, in a text file" bash -c "
    LC_ALL=C tr -d '\000' < '$c' | cmp -s - '$c' || exit 1
    flat=\$(tr '\n' ' ' < '$c' | tr -s ' ')
    printf '%s' \"\$flat\" | grep -q 'SPEC:' &&
    printf '%s' \"\$flat\" | grep -qi 'writable' &&
    printf '%s' \"\$flat\" | grep -q 'reference/fixtures/' &&
    printf '%s' \"\$flat\" | grep -qi 'edge case' &&
    printf '%s' \"\$flat\" | grep -qi 'prohibition' &&
    printf '%s' \"\$flat\" | grep -qi 'STOP and report'"
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
24-p0-gate-sdk-authme.txt|P0 gate transcript, asserted by plan-ledger
25-p1-gate-conformance.txt|P1 gate diff, asserted by the conformance run
26-p1-gate-sdk-smoke.txt|P1 gate SDK smoke, asserted by plan-ledger
28-p2-lane-l.txt|P2 Lane L transcript, asserted by plan-ledger
C1-domain-shape.txt|amk-types domain shapes, P5
EOF
)
if reg=$(python3 - <<'PY' 2>/dev/null
import re
src = open("crates/amk-types/tests/fixtures.rs").read()
block = src.split("DEFERRED: &[(&str, &str)] = &[", 1)[1].split("];", 1)[0]
for name, reason in re.findall(r'\(\s*"([^"]+)",\s*"([^"]+)",?\s*\)', block):
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
# The fan-out lock lives in the PRIMARY checkout only — `.claude/fanout.lock` is gitignored, so a
# worktree never has a copy of it. Both checks below previously resolved it against the script's own
# parent directory, which inside a dispatch worktree is the WORKTREE root: the lock read as absent
# while `git worktree list` still reported two, so `hygiene-worktrees-swept` failed and
# `./scripts/check.sh` could never exit 0 from inside a worktree. Every implementer is told to make
# that command pass before reporting, so the check made its own instruction impossible to satisfy.
# Found by the implementer it happened to, who traced it correctly and reported rather than
# working around it.
#
# Resolve against the main worktree instead. `--git-common-dir` is `.git` in the primary and an
# absolute path to the primary's `.git` from inside any worktree, so its parent is the primary
# checkout in both cases.
GCD="$(git rev-parse --git-common-dir 2>/dev/null || echo .git)"
case "$GCD" in /*) ;; *) GCD="$PWD/$GCD" ;; esac
PRIMARY="$(cd "$GCD/.." 2>/dev/null && pwd || printf '%s' "$PWD")"
LOCK="$PRIMARY/.claude/fanout.lock"

check hygiene-worktrees-swept yes \
  "no stale worktrees when no dispatch is in flight" \
  bash -c '[ -f "$1" ] || [ "$(git worktree list | wc -l)" -eq 1 ]' _ "$LOCK"

check hygiene-lock-released yes \
  "fan-out lock is not left set with no worktree" \
  bash -c '[ ! -f "$1" ] || [ "$(git worktree list | wc -l)" -gt 1 ]' _ "$LOCK"

# A live worktree's copy of the contracts must match the primary's. The id-safety dispatch was
# handed a contract, the orchestrator then rewrote that contract on main, and the worktree never
# saw the rewrite — so the implementer worked a full round against a superseded document and its
# "gaps" were all real against the current one. Nothing in the plan forbade that ordering and
# nothing detected it. This does.
check hygiene-worktree-contract-fresh yes \
  "any live worktree carries the primary's contracts, not a superseded copy" \
  bash -c '
    for wt in "$1"/.claude/worktrees/*/; do
      [ -d "$wt" ] || continue
      diff -rq "$1/.claude/contracts" "$wt/.claude/contracts" >/dev/null 2>&1 || exit 1
    done' _ "$PRIMARY"

# ---------------------------------------------------------------- contract scope derivation
# THE RULE THE ID-SAFETY DISPATCH BOUGHT, at four correction rounds. Its contract listed "the five
# call paths the panel reproduced live" — recalled from a review report, never derived from the
# code. Five sites were missing: messages::insert's `references`, messages::list's cursor, and all
# of api_keys.rs (inbox_id at four functions plus the presented credential). Every one was found by
# somebody enumerating; none was ever found by re-reading the contract.
#
# So a contract must state where its scope came from. `Scope-derivation:` is a command whose output
# IS the scope, or an explicit `n/a` with a reason. `n/a` is a legitimate answer — a greenfield
# crate has no existing surface to enumerate — but it has to be written down, because an absent
# derivation and a deliberate one are indistinguishable until someone walks into the difference.
check contract-scope-derived yes \
  "every contract states how its scope was derived (command, or an explicit n/a)" \
  bash -c '
    for c in .claude/contracts/*.md; do
      grep -q "^Scope-derivation:" "$c" || exit 1
    done'

# ---------------------------------------------------------------- security invariants with one guardian
# `authenticate` must cost the same on every kind of miss. The obvious NUL fix — an early
# `return Ok(None)` — skips the argon2 verify and resolves in ~700ns against ~500ms for a real
# miss. The review panel confirmed by mutation that exactly ONE test catches that, and that the
# value-asserting tests all still pass. A single-guardian property whose guardian is a "slow" test
# is one #[ignore] away from silently reopening, so the guardian itself is now pinned.
check security-timing-guard-live yes \
  "authenticate's equal-cost timing test exists and is not ignored" \
  bash -c '
    f=crates/amk-store/tests/api_keys.rs
    grep -q "fn authenticate_with_a_nul_byte_still_pays_the_real_verify_cost" "$f" || exit 1
    ! grep -B4 "fn authenticate_with_a_nul_byte_still_pays_the_real_verify_cost" "$f" |
      grep -q "#\[ignore"'

# ---------------------------------------------------------------- not yet due
# Two prerequisites, named so neither is discovered at gate time. The SDK is not installed anywhere
# on this machine and the harness cannot supply it: `conformance/dual_target.py` is deliberately
# stdlib-only, so nothing today would fail if the client were missing. Version pinned in
# `conformance/requirements-gate.txt` — an acceptance test whose point is that the UNMODIFIED
# official client works is not that test if it runs against an arbitrary version.
# Was a bare `pend` — statically wired to PENDING, never routed through `check()`, so no amount of
# implementation could ever have flipped it. Caught by the pre-dispatch review of
# `.claude/contracts/amk-bins.md`, which noticed the contract claimed the binaries dispatch would
# make this line stop reading PENDING when nothing in that dispatch's writable paths could.
#
# A ledger line that cannot change state is not a check, it is a comment that looks like one — the
# same defect shape as `harness-permissions-per-role` grepping for a literal `deny:` that an inert
# key satisfied. Now it asserts the EVIDENCE, in this project's own idiom: the gate is run by hand
# against a live server (too heavy for every `check.sh`), and its verbatim transcript is captured
# as a fixture. The fixture must exist and must contain a real Identity response, not a placeholder.
chk_p0_gate() {
  bash -c '
    f=reference/fixtures/24-p0-gate-sdk-authme.txt
    [ -f "$f" ] || exit 1
    grep -q "organization_id" "$f" &&
    grep -qi "agentmail" "$f" &&
    ! grep -qi "placeholder\|TODO\|not yet run" "$f"'
}
gate_check p0-gate-sdk-authme "P0 gate: official Python SDK auth.me() vs localhost, transcript captured" \
  reference/fixtures/24-p0-gate-sdk-authme.txt chk_p0_gate \
  crates/amk-http crates/amk-cli
# Same shape as p0-gate-sdk-authme: the gate needs a live server and a live reference account, so
# it is run by hand (`./scripts/p1-gate.sh`) and its verbatim transcript captured. The ledger
# asserts the EVIDENCE. A run that is not clean must not be able to satisfy this, so the clean
# result line itself is what is matched — "0 skipped, 0 with structural diffs" — not merely the
# fixture's existence, plus the harness's own exit line.
#
# It does NOT grep for the word "placeholder" as a stub-detector, the way p0-gate-sdk-authme does.
# This fixture legitimately discusses `{placeholders}` at length — that is the harness feature the
# gate needed — so the guard matched real prose and reported PENDING on a clean run. A negative
# check has to be keyed on something that cannot occur in the document it is guarding.
chk_p1_conformance() {
  bash -c '
    f=reference/fixtures/25-p1-gate-conformance.txt
    [ -f "$f" ] || exit 1
    grep -q "0 skipped, 0 with structural diffs" "$f" &&
    grep -q "THIRD RUN — CLEAN" "$f" &&
    grep -q "dual_target.py exit: 0" "$f"'
}
gate_check p1-gate-conformance "P1 gate: dual-target conformance diff clean for P1 endpoints" \
  reference/fixtures/25-p1-gate-conformance.txt chk_p1_conformance \
  crates/amk-http crates/amk-store crates/amk-types
# The OTHER half of P1's gate wording: "Python+Node SDK smoke (create/list/delete across scopes)".
# Both clients, unmodified, driving a live server with nothing changed but the base URL.
#
# Three things are asserted rather than one, because each has failed silently somewhere in this
# project already. (1) The clean run: all three halves of p1-gate.sh exited 0. (2) The
# FALSIFICATION: the same fixture records a poisoned run where the Node half failed and the gate's
# exit followed it — a gate that has only ever passed is not evidence it can fail, and the plan says
# so about tests. (3) The captured client versions are still the versions pinned on disk, so bumping
# a pin without re-running the gate fails here instead of leaving a transcript that describes a run
# against a client nobody uses any more.
chk_p1_sdk_smoke() {
  bash -c '
    f=reference/fixtures/26-p1-gate-sdk-smoke.txt
    [ -f "$f" ] || exit 1
    grep -q "^sdk_smoke.py exit: 0" "$f" &&
    grep -q "^sdk_smoke.mjs exit: 0" "$f" &&
    grep -q "^p1-gate.sh exit: 0" "$f" &&
    grep -q "^sdk_smoke.mjs exit: 1" "$f" &&
    grep -q "^p1-gate.sh exit: 1" "$f" &&
    py=$(sed -n "s/^agentmail==//p" conformance/requirements-gate.txt) &&
    nd=$(tr -d " \",;" < conformance/package.json | sed -n "s/^agentmail://p") &&
    [ -n "$py" ] && [ -n "$nd" ] &&
    grep -q "agentmail==$py" "$f" &&
    grep -q "agentmail@$nd" "$f"'
}
gate_check p1-gate-sdk-smoke "P1 gate: both official SDKs drive a live server; pinned, falsified" \
  reference/fixtures/26-p1-gate-sdk-smoke.txt chk_p1_sdk_smoke \
  crates/amk-http crates/amk-store crates/amk-cli
# P2's Lane L conjuncts, same shape as the two P1 checks above: run by hand against a live server,
# transcript captured, ledger asserts the EVIDENCE.
#
# Added 2026-08-19 because nothing asserted fixture 28 at all. It landed on `main` at 0e7d345 and
# `amk-types`'s own coverage tripwire went red on it -- `main` was failing
# `every_fixture_is_either_asserted_or_explicitly_deferred` and stayed that way, because no CI ran
# the suite and the transcript was committed in a docs-only change nobody re-tested. Deferring the
# fixture without adding this check would have recorded a reason ("asserted by plan-ledger") that
# was not true.
chk_p2_lane_l() {
  bash -c '
    f=reference/fixtures/28-p2-lane-l.txt
    [ -f "$f" ] || exit 1
    grep -q "^sdk_smoke.py exit: 0" "$f" &&
    grep -q "^sdk_smoke.mjs exit: 0" "$f" &&
    grep -q "^schemathesis exit: 0" "$f" &&
    grep -q "45 operations" "$f"'
}
gate_check p2-gate-lane-l "P2 Lane L: schemathesis + both SDK smokes green against our own server" \
  reference/fixtures/28-p2-lane-l.txt chk_p2_lane_l \
  crates/amk-http crates/amk-outbound crates/amk-ingest
pend p6-restore-drill        "P6: restore drill passes from backups alone, before any cutover step"

# ---------------------------------------------------------------- cannot be machine-checked
# Listed, never omitted. An omitted check reads as a passing one.
attest review-panel-per-diff "three lenses returned on the last merged diff"
attest mutation-at-gate      "mutation set run for the crate at its gate; survivors accounted for"
attest evidence-not-assert   "every completion claim in the last report carried its command output"
# Named rather than dropped. The plan DECIDES reviewers get memory ON and implementers OFF; the
# `memory:` frontmatter key is unverified for Claude Code 2.1.233 and an unsupported key can cost
# an agent its registration, so it is currently absent from all five files and the decision binds
# nothing. Verify the key in a fresh session, then add it to harness-agent-frontmatter's allowlist
# and to the reviewer files — in that order, so the allowlist can never be the thing that lags.
attest mem-subagent-memory   "subagent memory split: DECIDED in the plan, NOT bound — memory: key unverified on 2.1.233"
# WAS AN ATTEST, IS NOW A CHECK — and reading the source narrowed the claim.
#
# The review panel caught the api-keys contract citing the am_eu_ host-routing as [SPEC:sdk] when
# nothing in this repository could verify it: reference/ vendors openapi.json and the SDK type
# extracts, but not the node client's own source. P1's Node SDK smoke changed that — the client is
# now pinned in conformance/package.json and installed at gate time, so the claim is checkable
# where it actually lives instead of being carried as an unverifiable citation.
#
# What the source says, which is narrower than the claim was: the prefix is consulted ONLY when the
# caller passes neither `environment` nor `baseUrl` (wrapper/Client.js), in which case an am_eu_ key
# selects AgentMailEnvironment.EuProd -> https://api.agentmail.eu. A caller who sets either one —
# as both our SDK smokes do — is never re-routed. So minting an am_eu_ key would not break the
# gate; it would break the caller who configures nothing and relies on AGENTMAIL_API_KEY, which is
# the default path the official docs show. The rule stands unchanged and is still fail-closed;
# only its reason is now exact.
#
# Skipped rather than failed when the tree is absent: conformance/node_modules is gitignored and
# installed by the gate, so a clean checkout must not report a violation it cannot see.
check evidence-sdk-routing no "node SDK routes am_eu_ to the EU host when no environment/baseUrl is set" \
  bash -c '
    c=conformance/node_modules/agentmail/dist/cjs/wrapper/Client.js
    e=conformance/node_modules/agentmail/dist/cjs/environments.js
    [ -f "$c" ] && [ -f "$e" ] || exit 1
    grep -q "startsWith(\"am_eu_\")" "$c" &&
    grep -q "!fernOptions.environment && !fernOptions.baseUrl" "$c" &&
    grep -q "https://api.agentmail.eu" "$e"'

echo
# THE LEDGER CHECKS ITSELF FOR THE ONE FAILURE MODE IT CANNOT OTHERWISE SEE: a line that stops
# being a line. `p0-gate-sdk-authme`'s `bash -c '…'` was left unterminated, and the runaway quote
# swallowed the FIVE obligations that followed it — two PENDING gates and three ATTEST lines —
# while the ledger printed PASS. An obligation that disappears is exactly the "omitted check reads
# as a passing one" defect this file exists to prevent, arriving through a shell quote rather than
# an edit, and invisible to `bash -n` because the result is still valid syntax.
#
# So the ids DECLARED in the source are diffed against the ids that actually PRINTED. Ids, not a
# count: a set difference names the obligation that vanished, and a count only says one did. Both
# sides are derived from the file and the contracts directory — never a transcribed number, which
# is a check that silently stops checking (the guard-suite count read 24 while the suite ran 32).
# `$id` in the contracts loop expands from the same glob the loop iterates.
missing=$(
  { grep -oE '^[[:space:]]*(check|gate_check|pend|attest|bad)[[:space:]]+"?[A-Za-z0-9_$-]*' "$0" \
      | sed -E 's/^[[:space:]]*[a-z_]+[[:space:]]+"?//' | grep -v '^\$id$'
    for c in .claude/contracts/*.md; do printf 'contract-%s\n' "$(basename "$c" .md)"; done
  } | sort -u | while read -r id; do
        case " $emitted " in *" $id "*) ;; *) printf '%s ' "$id" ;; esac
      done
)
if [ -n "$missing" ]; then
  printf '  \033[31mSKIPPED\033[0m  %-38s %s\n' "ledger-self-consistent" \
    "declared but never printed: ${missing}— a line was swallowed (check the quoting)"
  fail=$((fail+1))
fi
if [ "$fail" -gt 0 ]; then
  printf 'plan-ledger: \033[31mFAIL\033[0m (%d due obligation(s) unmet)\n' "$fail"
  exit 1
fi
echo "plan-ledger: PASS"
