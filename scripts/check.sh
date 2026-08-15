#!/usr/bin/env bash
# The project verify command. One entry point: CI, the Stop hook, and humans all run this.
#
# Must stay FAST and TERMINATING — the Stop hook blocks a turn from ending until it passes, and
# Claude Code overrides a Stop hook after 8 consecutive blocks, so a slow or hanging check
# degrades into no gate at all.
#   --fast   skip clippy (used by the Stop hook, which must stay quick)
set -uo pipefail
cd "$(dirname "$0")/.."

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

fail=0
step() { printf '\n== %s ==\n' "$1"; }
run() { "$@" || fail=1; }

step "fmt"
if cargo fmt --version >/dev/null 2>&1; then
  run cargo fmt --all -- --check
else
  echo "  rustfmt not installed; skipping"
fi

if [ "$FAST" -eq 0 ]; then
  step "clippy"
  if cargo clippy --version >/dev/null 2>&1; then
    run cargo clippy --workspace --all-targets -- -D warnings
  else
    echo "  clippy not installed; skipping"
  fi
fi

step "tests"
# amk-store's DB-backed integration tests skip cleanly when Postgres is unreachable, per its
# dispatch contract — but a gate that reports "ok" whether or not it touched a database cannot
# tell "passed" from "silently verified nothing". So: if the dev database answers, require it
# (AMK_REQUIRE_DB=1 turns an unreachable-database skip into a panic in
# crates/amk-store/tests/support/mod.rs), and if it does not, say so out loud instead of passing
# quietly. Never started here — scripts/dev-db.sh is a human/CI step, not this script's job.
if timeout 1 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'; then
  export AMK_REQUIRE_DB=1
else
  echo "  dev database unreachable at 127.0.0.1:55432 — amk-store's DB-backed integration tests are SKIPPED, not verified (run ./scripts/dev-db.sh up to cover them)"
fi
run cargo test --workspace

step "shape provenance"
run ./scripts/shape-provenance.sh

# The plan's obligations, mechanically. Runs LAST so its output is the final thing read: an audit
# found eleven plan steps skipped, and the failure mode was always the same — the obligation was
# remembered rather than checked. A due-but-unmet obligation now fails the build.
step "plan ledger"
run ./scripts/plan-ledger.sh

printf '\n'
if [ "$fail" -eq 0 ]; then
  echo "check: PASS"
else
  echo "check: FAIL"
fi
exit "$fail"
