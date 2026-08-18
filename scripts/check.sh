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
# A MISSING DEPENDENCY IS A FAILURE. There is no partial pass and no INCOMPLETE state. No step is
# ever dropped from the list, and a step whose prerequisite is absent turns the whole run red.
#
# That is affordable because provisioning is solved rather than assumed: this project targets the
# workstation and the Claude sandbox, and ./scripts/bootstrap.sh fully provisions both. So a
# missing dependency means bootstrap was not run or failed — which is worth a red run, because the
# alternative is a green run that quietly examined less than it appears to have examined. This
# project has already shipped that defect once, when this script printed PASS with no Postgres.
set -uo pipefail
cd "$(dirname "$0")/.."

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

# The DB-backed suite is most of amk-store's and amk-http's real coverage, so this script tries to
# start the database rather than stepping around it. dev-db.sh is idempotent and needs no Docker.
# If it cannot, the `test` step FAILS the run — it is never silently omitted and never tolerated.
if ! timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'; then
  printf '== starting the dev database ==\n'
  ./scripts/dev-db.sh up >/dev/null 2>&1 \
    || printf '\033[31mcheck: could not start Postgres — the test step will FAIL\033[0m\n'
fi

# Order is deliberate: cheapest and most-likely-to-fail first, so a formatting slip costs seconds
# rather than a full compile. `ledger` runs last so its obligation table is the final thing read.
if [ "$FAST" -eq 1 ]; then
  STEPS="fmt fixtures test provenance ledger"
else
  STEPS="fmt clippy fixtures test provenance hooks audit ledger"
fi

passed=""; failed=""; missing=""
for step in $STEPS; do
  ./scripts/verify.sh "$step"
  case $? in
    0) passed="$passed $step" ;;
    3) missing="$missing $step" ;;
    *) failed="$failed $step" ;;
  esac
done

printf '\n\033[1m== check summary ==\033[0m\n'
printf '  passed:%s\n' "${passed:- (none)}"
[ -n "$failed" ]  && printf '  \033[31mFAILED:%s\033[0m\n' "$failed"
[ -n "$missing" ] && printf '  \033[31mDEPENDENCY MISSING:%s\033[0m\n' "$missing"

if [ -n "$missing" ]; then
  printf '\n\033[31mcheck: FAIL\033[0m — this machine is not provisioned, so the steps above did not run.\n'
  printf 'Run \033[1m./scripts/bootstrap.sh\033[0m and try again. A run that skipped checks is not a pass.\n'
  exit 1
fi

if [ -n "$failed" ]; then
  printf '\n\033[31mcheck: FAIL\033[0m\n'
  exit 1
fi

printf '\n\033[32mcheck: PASS\033[0m  (every step ran; CI remains the authoritative gate)\n'
exit 0
