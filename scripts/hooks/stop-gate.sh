#!/usr/bin/env bash
# Stop hook: refuse to end a turn on a broken tree.
#
# Deliberately SCOPED: it runs only when Rust sources actually differ from HEAD. An unconditional
# gate fires on conversational turns too, and since Claude Code overrides a Stop hook after 8
# consecutive blocks, a noisy gate spends its blocks on turns it had no business gating and is
# disabled exactly when a real failure shows up. Gate the change, not the conversation.
#
# Skips clippy (slow); scripts/check.sh remains the full check for CI and humans.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 0

# Nothing Rust-shaped changed -> not our business.
if git diff --quiet -- '*.rs' 'Cargo.toml' '*/Cargo.toml' 2>/dev/null \
   && git diff --cached --quiet -- '*.rs' 'Cargo.toml' '*/Cargo.toml' 2>/dev/null; then
  exit 0
fi

out=$(./scripts/check.sh --fast 2>&1)
if [ $? -ne 0 ]; then
  printf 'Tree has uncommitted Rust changes and scripts/check.sh --fast FAILS.\n\n%s\n\nFix it or revert before ending the turn.\n' \
    "$(printf '%s' "$out" | tail -40)" >&2
  exit 2
fi
exit 0
