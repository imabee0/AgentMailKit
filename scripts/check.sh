#!/usr/bin/env bash
# The project verify command. One entry point: CI, the Stop hook, and humans all run this.
#
# Must stay FAST and TERMINATING — the Stop hook blocks a turn from ending until it passes, and
# Claude Code overrides a Stop hook after 8 consecutive blocks, so a slow or hanging check
# degrades into no gate at all.
#   --fast   skip clippy and the binary smoke (used by the Stop hook, which must stay quick)
#
# NOTHING HERE SKIPS SILENTLY.
#
# It used to, three ways, and an audit on 2026-08-19 found all three reporting `check: PASS`:
#   - a missing rustfmt or clippy printed "skipping" and never touched `fail`, so an unformatted
#     or unlinted tree passed;
#   - an unreachable Postgres skipped 336 of 705 tests -- 48% of the suite -- and printed the same
#     final line as a full run;
#   - the composed binary was never started at all, so `amkd --role api` shipped for weeks unable
#     to send a single message with every gate green.
# The first is now a failure, the second starts the database rather than shrugging at it, and the
# third is `scripts/binary-smoke.sh`. When a prerequisite genuinely cannot be met, the final line
# SAYS SO and differs from a clean pass, because "did not run" reported as "passed" is the exact
# failure mode this file exists to prevent.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

fail=0
degraded=""
step() { printf '\n== %s ==\n' "$1"; }
run() { "$@" || fail=1; }
note_degraded() { degraded="${degraded}${degraded:+; }$1"; }

# A tool this project mandates is missing -> FAIL, not skip. `rust-toolchain.toml` pins the
# channel and names rustfmt and clippy as components, so rustup installs both on first use; a
# genuinely absent one means the pinned toolchain is not in effect, which is a broken environment
# and not something to pass over.
require_tool() { # $1=display name  $2..=version probe
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then return 0; fi
  echo "  FAIL: $name is not available. rust-toolchain.toml pins it as a component; run"
  echo "        'rustup component add ${name}' or check that the pinned toolchain is active."
  fail=1
  return 1
}

step "fmt"
require_tool rustfmt cargo fmt --version && run cargo fmt --all -- --check

if [ "$FAST" -eq 0 ]; then
  step "clippy"
  require_tool clippy cargo clippy --version && run cargo clippy --workspace --all-targets -- -D warnings
fi

step "tests"
# `AMK_REQUIRE_DB=1` turns an unreachable-database skip into a panic
# (crates/amk-store/tests/support/mod.rs), which is what makes a DB-backed run distinguishable
# from a DB-less one. Previously this script only SET that when the database already answered,
# and otherwise printed a warning and carried on -- so the common case (nobody started it) was
# also the silent one. Now it starts the database itself: `./scripts/dev-db.sh up` drives
# initdb/pg_ctl directly and needs no Docker, so there is no reason this has to be a human step.
if ! timeout 1 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432)' 2>/dev/null; then
  echo "  dev database not up; starting it (./scripts/dev-db.sh up)"
  ./scripts/dev-db.sh up >/dev/null 2>&1
fi
if timeout 1 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432)' 2>/dev/null; then
  export AMK_REQUIRE_DB=1
else
  note_degraded "DB-backed tests SKIPPED (no Postgres on 127.0.0.1:55432)"
  echo "  WARNING: could not reach or start the dev database. 336 of ~705 tests will be SKIPPED,"
  echo "           and this run cannot tell 'passed' from 'verified nothing' for amk-store,"
  echo "           amk-http, amk-cli and amk-ingest. The final line records that."
fi
run cargo test --workspace

step "shape provenance"
run ./scripts/shape-provenance.sh

# The composed binary, configured the way an operator would configure it. Skipped by --fast: it
# builds in release and stands up two servers, which is far too slow for a Stop hook, but it is
# NOT optional for a full run -- it is the only gate that has ever observed the shipped binary.
if [ "$FAST" -eq 0 ]; then
  step "binary smoke"
  ./scripts/binary-smoke.sh
  case $? in
    0) ;;
    2) note_degraded "binary smoke NOT RUN (prerequisites unavailable)" ;;
    *) fail=1 ;;
  esac
fi

# The plan's obligations, mechanically. Runs LAST so its output is the final thing read: an audit
# found eleven plan steps skipped, and the failure mode was always the same — the obligation was
# remembered rather than checked. A due-but-unmet obligation now fails the build.
step "plan ledger"
run ./scripts/plan-ledger.sh

printf '\n'
if [ "$fail" -ne 0 ]; then
  echo "check: FAIL"
elif [ -n "$degraded" ]; then
  # Deliberately NOT the string "check: PASS". Anything grepping for a clean run -- CI, a human
  # skimming, a future ledger check -- must not match a run that skipped half the suite.
  echo "check: PASS WITH GAPS -- $degraded"
else
  echo "check: PASS"
fi
exit "$fail"
