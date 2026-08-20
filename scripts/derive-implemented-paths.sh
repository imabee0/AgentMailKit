#!/usr/bin/env bash
# Scope derivation for anything that reaches across the mounted HTTP surface: which operations
# amk-http serves, which openapi.json describes, and the path filter that selects exactly the
# former. Paste this script's output into a contract's `Scope-derivation:` line.
#
# The parse itself lives in conformance/schemathesis_scope.py, which scripts/p1-gate.sh also calls
# for its `--include-path` arguments — one owner, so the gate and the contract can never scope
# themselves differently. Exits non-zero when router() and the spec disagree.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }
exec python3 conformance/schemathesis_scope.py --list
