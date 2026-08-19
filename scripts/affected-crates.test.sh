#!/usr/bin/env bash
# Tests for scripts/affected-crates.sh, in BOTH directions: an under-selection must fail (that is
# a green build on an untested regression) and an over-selection must fail too (that is the whole
# saving evaporating silently). `guard.test.sh` is the precedent -- a gate that is not itself
# tested is a gate that stops working without telling anyone.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

pass=0; fail=0
run() { ./scripts/affected-crates.sh "$@" </dev/null | tr '\n' ' ' | sed 's/ *$//'; }

expect() { # $1=description  $2=expected  $3..=paths
  local desc="$1" want="$2"; shift 2
  local got; got=$(run "$@")
  if [ "$got" = "$want" ]; then
    pass=$((pass+1)); printf '  ok    %s\n' "$desc"
  else
    fail=$((fail+1)); printf '  FAIL  %s\n        want: %s\n        got:  %s\n' "$desc" "$want" "$got"
  fi
}

echo "== affected-crates.sh =="

# --- the closure is real, not directory matching -------------------------------------------
expect "amk-types reaches every crate"            "ALL" crates/amk-types/src/ids.rs
expect "amk-core reaches its dependents but NOT amk-outbound"          "-p amk-cli -p amk-core -p amk-http -p amk-ingest -p amk-store" crates/amk-core/src/scope.rs
expect "amk-outbound reaches http and cli"        "-p amk-cli -p amk-http -p amk-outbound" crates/amk-outbound/src/signing.rs
expect "amk-ingest reaches cli only"              "-p amk-cli -p amk-ingest" crates/amk-ingest/src/smtp.rs
expect "amk-cli reaches nothing further"          "-p amk-cli" crates/amk-cli/src/args.rs
expect "a crate's tests select like its src"      "-p amk-cli -p amk-ingest" crates/amk-ingest/tests/persist.rs
expect "a crate's Cargo.toml selects that crate"  "-p amk-cli -p amk-ingest" crates/amk-ingest/Cargo.toml

# --- two changed crates union their closures -----------------------------------------------
expect "two crates union their closures" "-p amk-cli -p amk-http -p amk-ingest -p amk-outbound" \
  crates/amk-ingest/src/smtp.rs crates/amk-outbound/src/build.rs

# --- documentation reaches no crate ---------------------------------------------------------
expect "docs change selects nothing"   "" docs/PLAN.md
expect "README selects nothing"        "" README.md
expect "CLAUDE.md selects nothing"     "" CLAUDE.md
expect "agent prose selects nothing"   "" .claude/contracts/amk-http.md
expect "docs plus a crate still selects that crate" "-p amk-cli -p amk-ingest" \
  docs/RESUME.md crates/amk-ingest/src/accept.rs

# --- global triggers widen ------------------------------------------------------------------
expect "Cargo.lock widens"        "ALL" Cargo.lock
expect "workspace manifest widens" "ALL" Cargo.toml
expect "toolchain pin widens"     "ALL" rust-toolchain.toml
expect "a gate script widens"     "ALL" scripts/check.sh
expect "a workflow widens"        "ALL" .github/workflows/ci.yml
expect "a fixture widens"         "ALL" reference/fixtures/21-unbracketed.txt
expect "the Dockerfile widens"    "ALL" Dockerfile

# --- fail OPEN, never closed ------------------------------------------------------------------
expect "an unrecognised path widens rather than selecting nothing" "ALL" some/brand/new/dir/x.rs
expect "a bare new top-level file widens"                          "ALL" newthing.rs
# The dangerous direction: a path the script does not understand must never come back empty,
# because empty means "run no tests" and the PR goes green having verified nothing.

# --- degenerate input --------------------------------------------------------------------------
expect "no paths at all selects nothing" ""
expect "blank lines are ignored"         "" ""

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
