#!/usr/bin/env bash
# THE definition of every verification step. Humans, the Stop hook and GitHub Actions all run
# these exact commands — CI runs one step per job, `scripts/check.sh` runs several in a row.
#
# WHY THIS EXISTS RATHER THAN CI CALLING check.sh. `scripts/check.sh` is built to a constraint CI
# does not share: the Stop hook blocks a turn until it passes, and Claude Code disables a Stop hook
# after 8 consecutive blocks, so check.sh must stay fast and must never hang. It pays for that by
# DEGRADING QUIETLY — it skips rustfmt if rustfmt is absent, skips clippy if clippy is absent, and
# exits PASS when Postgres is unreachable having run none of the DB-backed suite. Those are
# defensible in a local pre-flight and disqualifying in a merge gate: a gate that passes because a
# tool was missing is worse than no gate, because it reports the same green.
#
# So the steps live here and are STRICT, everywhere, with no permissive local mode: a missing tool
# is a failure and an unreachable database is a failure, on a workstation exactly as in CI.
# check.sh is a thin wrapper that calls them. Same commands, same flags, same exit semantics in
# both places; a CI failure reproduces locally by running the step it names.
#
#   ./scripts/verify.sh <step> [step...]     run steps in order, stop at the first failure
#   ./scripts/verify.sh --list               what steps exist
#
# Every step prints `verify:<step>: PASS|FAIL` as its last line so a human, a log scraper and a
# GitHub Actions annotation all read the same verdict.
set -uo pipefail
cd "$(dirname "$0")/.."

# The database contract, in one place. Tests bind to AMK_DATABASE_URL; everything that provisions a
# database for them — scripts/dev-db.sh locally, a `services:` container in Actions — must land on
# THIS host/port/role/name. The provisioner differs by environment on purpose (Actions has Docker
# and a warm postgres image; the workstation and the sandbox may have neither). The DSN does not.
DB_HOST="${AMK_DB_HOST:-127.0.0.1}"
DB_PORT="${AMK_DB_PORT:-55432}"
DEV_DSN="postgres://amk:amk-dev-local@${DB_HOST}:${DB_PORT}/amk"

STEPS="fmt clippy build test fixtures provenance ledger hooks audit gate-lane-l"

say()  { printf '\n\033[1m== verify:%s ==\033[0m\n' "$1"; }
die()  { printf '\033[31mverify:%s: FAIL\033[0m — %s\n' "$STEP" "$1" >&2; exit 1; }
ok()   { printf '\033[32mverify:%s: PASS\033[0m\n' "$STEP"; }

# THREE outcomes, and TWO of them are failures.
#
#   exit 0  PASS               the step ran and the code satisfied it
#   exit 1  FAIL               the step ran and the code did not satisfy it
#   exit 3  DEPENDENCY MISSING the step could not run — and that is a FAILURE, not a caveat
#
# Exit 3 exists only to separate "your code is wrong" from "this machine is not provisioned", so
# the remedy printed is the right one. It is NEVER a pass, in any environment, under any flag.
#
# This is deliberate and was tightened after an earlier revision let a missing tool exit 0 with an
# INCOMPLETE banner. That is the same defect this project already shipped once: `check.sh` used to
# print PASS with no Postgres, having run none of the DB-backed suite. A banner is not a gate. If a
# dependency is absent the run must go red, because the alternative is a green run that examined
# less than it appears to have examined.
#
# It is safe to be this strict because provisioning is solved rather than assumed. This project
# targets exactly two environments, and `./scripts/bootstrap.sh` fully provisions both:
#
#   the workstation      already carries the toolchain
#   the Claude sandbox   ships Rust, Python 3 + pip, Node 20/21/22 and PostgreSQL 16 preinstalled
#                        (Postgres installed but NOT started), on Ubuntu 24.04 as root, with
#                        `Trusted` network access reaching crates.io, PyPI and npm. cargo-deny is
#                        the only thing this project needs that is not preinstalled, and bootstrap
#                        installs it; the environment cache keeps it for later sessions.
#
# So "dependency missing" means bootstrap was not run or bootstrap failed. Both are worth a red run.
dep_missing() {
  printf '\033[31mverify:%s: DEPENDENCY MISSING\033[0m — %s\n' "$STEP" "$1" >&2
  printf '  run \033[1m./scripts/bootstrap.sh\033[0m to provision this machine, then re-run.\n' >&2
  exit 3
}

need() {
  command -v "$1" >/dev/null 2>&1 || dep_missing "required tool '$1' is not installed. $2"
}

db_up() {
  timeout 2 bash -c "(exec 3<>/dev/tcp/${DB_HOST}/${DB_PORT}) 2>/dev/null"
}

require_db() {
  db_up && return 0
  dep_missing "no Postgres at ${DB_HOST}:${DB_PORT}.
  local:  ./scripts/dev-db.sh up
  CI:     the 'postgres' service container failed to start — check the job's service logs
  Without it every amk-store and amk-http integration test would be skipped, so this step
  fails rather than run a fraction of itself and report success."
}

# ---------------------------------------------------------------------------- steps

step_fmt() {
  need cargo "rustup component add rustfmt"
  cargo fmt --version >/dev/null 2>&1 || die "rustfmt is not installed (rustup component add rustfmt)"
  cargo fmt --all -- --check || die "formatting differs; run: cargo fmt --all"
}

step_clippy() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  cargo clippy --version >/dev/null 2>&1 || die "clippy is not installed (rustup component add clippy)"
  cargo clippy --workspace --all-targets --locked -- -D warnings || die "clippy raised warnings"
}

# Compiles the workspace AND every test target, so the jobs that follow restore a warm cache
# instead of recompiling. `--locked` makes a stale Cargo.lock a build failure rather than a silent
# dependency bump: a lockfile CI is willing to rewrite is not a lockfile.
step_build() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  cargo build --workspace --all-targets --locked || die "workspace build failed"
}

step_test() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  require_db
  # AMK_REQUIRE_DB turns an unreachable-database skip into a panic inside
  # crates/amk-store/tests/support/mod.rs. Set unconditionally here: require_db already proved the
  # database is up, so the only thing this can now catch is a test that would have skipped anyway.
  AMK_REQUIRE_DB=1 AMK_DATABASE_URL="${AMK_DATABASE_URL:-$DEV_DSN}" \
    cargo test --workspace --locked || die "tests failed"
}

step_provenance() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  ./scripts/shape-provenance.sh || die "shape provenance failed"
}
step_ledger()     { ./scripts/plan-ledger.sh     || die "a due plan obligation is unmet"; }
step_hooks()      { ./scripts/hooks/guard.test.sh || die "the write-guard's own tests failed"; }

# The fixture corpus IS the regression suite, so a capture nothing asserts against is a silent gap.
# Cheap and dependency-free, which is why it is its own step: it catches the "added a fixture,
# never wired it in" mistake in seconds rather than behind a full workspace compile.
step_fixtures() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  cargo test -p amk-types --locked --test fixtures || die "fixture reconciliation failed"
}

# Supply-chain gate. Advisories, licence policy and duplicate/banned crates in one pass. Kept
# separate from `build` because it needs no compilation and must be able to fail the pipeline on a
# day when nothing in this repository changed at all.
step_audit() {
  need cargo-deny "cargo install --locked cargo-deny"
  cargo deny --all-features check || die "cargo-deny found advisories, licence or ban violations"
}

# The Lane L half of the phase gate: schemathesis over the implemented paths, plus both official
# SDK smokes, against a real served amkd.
#
# ONE step, not three, because all of them share a single expensive setup — throwaway database,
# migrate, `amk init`, seeded fixtures, `amkd` serving on :8111. Splitting them into separate CI
# jobs would repeat that standup per job for no isolation benefit; they are not independent work.
#
# scripts/p1-gate.sh already stands that up, including the parts that are easy to get wrong (root
# key never in argv, 0600 curlrc, throwaway database dropped by an exit trap). `--lane-l` runs all
# of it EXCEPT the credentialed dual-target diff, which must never hold a live third-party API key
# in a pull-request runner.
step_gate_lane_l() {
  need cargo "install Rust via https://rustup.rs, then rustup will honour rust-toolchain.toml"
  need psql "install a postgresql-client package"
  need node "install Node 22+ for the official Node SDK smoke"
  need python3 "install Python 3.12+"
  require_db
  ./scripts/p1-gate.sh --lane-l || die "the Lane L gate failed"
}

# ---------------------------------------------------------------------------- dispatch

if [ "$#" -eq 0 ] || [ "${1:-}" = "--list" ]; then
  printf 'usage: %s <step> [step...]\nsteps: %s\n' "$0" "$STEPS"
  [ "$#" -eq 0 ] && exit 2
  exit 0
fi

for STEP in "$@"; do
  case "$STEP" in
    fmt)          say "$STEP"; step_fmt ;;
    clippy)       say "$STEP"; step_clippy ;;
    build)        say "$STEP"; step_build ;;
    test)         say "$STEP"; step_test ;;
    audit)        say "$STEP"; step_audit ;;
    provenance)   say "$STEP"; step_provenance ;;
    ledger)       say "$STEP"; step_ledger ;;
    hooks)        say "$STEP"; step_hooks ;;
    fixtures)     say "$STEP"; step_fixtures ;;
    gate-lane-l)  say "$STEP"; step_gate_lane_l ;;
    *) printf 'verify: unknown step %s\nsteps: %s\n' "$STEP" "$STEPS" >&2; exit 2 ;;
  esac
  ok
done
