#!/usr/bin/env bash
# The local pre-flight. A thin wrapper over scripts/verify.sh, which holds the actual step
# definitions and is the SAME code GitHub Actions runs.
#
#   ./scripts/check.sh            every step
#   ./scripts/check.sh --fast     drops clippy and the audit (what the Stop hook runs)
#
# WHAT CHANGED, AND WHY IT MATTERS. This script used to define the steps itself, and it degraded
# quietly: no rustfmt -> skip, no clippy -> skip, no Postgres -> run the suite anyway and exit PASS
# having verified none of the DB-backed integration tests. That is defensible for a hook that must
# never hang and disqualifying for anything anyone treats as a gate — and it WAS treated as one,
# because the project had no CI. It does now, so the roles separate cleanly:
#
#   this script   a local pre-flight, run before you push
#   CI            the authoritative gate, strict, no skips, on every pull request
#
# The steps are identical in both. When CI fails on `verify:clippy`, you reproduce it here with
# `./scripts/verify.sh clippy` — same command, same flags, same compiler (rust-toolchain.toml).
#
# SANDBOXES ARE THE NORMAL CASE, NOT THE EXCEPTION. Much of this project's work happens where
# cargo-deny is not installed and Postgres may not be running. So no step is ever dropped from the
# list to make the run green: a step whose prerequisite is missing reports NOT RUN, is counted, and
# is named again in the final banner. The banner never reads a bare PASS unless every step
# actually executed — because "it wasn't in the list" is precisely how a check gets missed.
set -uo pipefail
cd "$(dirname "$0")/.."

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

# Lets verify.sh distinguish "cannot run here" (exit 3) from "ran and failed" (exit 1). CI never
# sets this, so a missing prerequisite there is a hard failure rather than a tolerated gap.
export AMK_PERMIT_NOT_RUN=1

# The DB-backed suite is most of amk-store's and amk-http's real coverage, so this script tries to
# start the database rather than stepping around it. dev-db.sh is idempotent and needs no Docker.
# If it cannot, `test` reports NOT RUN below — it is never silently omitted.
if ! timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'; then
  printf '== starting the dev database ==\n'
  ./scripts/dev-db.sh up >/dev/null 2>&1 \
    || printf '\033[33mcheck: could not start Postgres — the test step will report NOT RUN\033[0m\n'
fi

# Order is deliberate: cheapest and most-likely-to-fail first, so a formatting slip costs seconds
# rather than a full compile. `ledger` runs last so its obligation table is the final thing read.
if [ "$FAST" -eq 1 ]; then
  STEPS="fmt fixtures test provenance ledger"
else
  STEPS="fmt clippy fixtures test provenance hooks audit ledger"
fi

passed=""; failed=""; skipped=""
for step in $STEPS; do
  ./scripts/verify.sh "$step"
  case $? in
    0) passed="$passed $step" ;;
    3) skipped="$skipped $step" ;;
    *) failed="$failed $step" ;;
  esac
done

printf '\n\033[1m== check summary ==\033[0m\n'
printf '  ran and passed:%s\n' "${passed:- (none)}"
[ -n "$failed" ]  && printf '  \033[31mFAILED:%s\033[0m\n' "$failed"
[ -n "$skipped" ] && printf '  \033[33mNOT RUN:%s\033[0m\n' "$skipped"

if [ -n "$failed" ]; then
  printf '\n\033[31mcheck: FAIL\033[0m\n'
  exit 1
fi

if [ -n "$skipped" ]; then
  # Deliberately NOT the word PASS, and deliberately still exit 0 — a non-zero exit here would
  # block the Stop hook on every turn in a sandbox, and Claude Code disables a Stop hook after 8
  # consecutive blocks, which would destroy the gate that does work. The banner carries the signal.
  printf '\n\033[33mcheck: INCOMPLETE\033[0m — everything that could run passed, but the steps above did NOT run.\n'
  printf 'This is not equivalent to CI. Install the missing prerequisite, or let CI cover it:\n'
  printf '  audit -> cargo install --locked cargo-deny\n'
  printf '  test  -> ./scripts/dev-db.sh up\n'
  exit 0
fi

printf '\n\033[32mcheck: PASS\033[0m  (every step ran; CI remains the authoritative gate)\n'
exit 0
