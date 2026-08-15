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
run cargo test --workspace

step "shape provenance"
run ./scripts/shape-provenance.sh

printf '\n'
if [ "$fail" -eq 0 ]; then
  echo "check: PASS"
else
  echo "check: FAIL"
fi
exit "$fail"
