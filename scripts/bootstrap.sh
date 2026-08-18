#!/usr/bin/env bash
# Provision a machine so that every check in ./scripts/check.sh can actually run. Idempotent.
#
#   ./scripts/bootstrap.sh          provision, then verify; EXITS NON-ZERO if anything is missing
#   ./scripts/bootstrap.sh --check  verify only, change nothing (same exit semantics)
#
# TWO TARGET ENVIRONMENTS, and nothing else is assumed:
#
#   1. The workstation. Already carries the toolchain; this script is then close to a no-op.
#
#   2. The Claude sandbox (Anthropic-hosted cloud environment). Ubuntu 24.04, running as root,
#      with sudo. Preinstalled and confirmed against the published environment reference:
#        Rust      rustc + cargo (rustup present, so rust-toolchain.toml is honoured)
#        Python    3.x with pip
#        Node      20, 21 and 22 under /opt/nodeNN, with 22 on PATH
#        Postgres  16 — INSTALLED BUT NOT RUNNING, and its server binaries (initdb, pg_ctl) live
#                  under /usr/lib/postgresql/16/bin, which is NOT on PATH. `psql` being present
#                  therefore does not imply `initdb` is.
#        Utilities git, jq, ripgrep
#      Network access is `Trusted` by default, which reaches crates.io, PyPI and npm. Whatever this
#      script installs is kept by the environment cache, so later sessions start already provisioned.
#
#      NOT preinstalled, and the only such thing this project needs: cargo-deny.
#
# WHY THIS IS TRACKED IN THE REPOSITORY. The sandbox starts with this repo and whatever the base
# image happens to carry. Anything a check needs must therefore ship AS A SCRIPT IN THE REPO rather
# than be assumed present — otherwise the checks that cannot run become the checks nobody runs.
#
# WHY IT EXITS NON-ZERO. scripts/verify.sh treats a missing dependency as a FAILURE, never as a
# tolerated gap, so this script must be able to say plainly that it did not finish. A bootstrap
# that prints a warning and exits 0 hands the caller a false green, which is the same defect as a
# gate that passes having examined nothing.
#
# CI does NOT use this script. GitHub Actions provisions through mechanisms its runners are built
# for — `services:` containers, setup-python, setup-node, a prebuilt cargo-deny — which are faster
# and cacheable there. This is for the workstation and the sandbox.
set -uo pipefail
cd "$(dirname "$0")/.."

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

note() { printf '\n\033[1m==\033[0m %s\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }
good() { printf '   \033[32mok\033[0m       %s\n' "$1"; }
bad()  { printf '   \033[31mMISSING\033[0m  %s\n' "$1"; }
info() { printf '            %s\n' "$1"; }

# ---------------------------------------------------------------- Rust
# rustup reads rust-toolchain.toml itself, so invoking cargo installs and selects the pinned
# toolchain and its components. Nothing here names a version — one declaration, one place.
note "Rust toolchain (version pinned by rust-toolchain.toml)"
if have rustup; then
  [ "$CHECK_ONLY" -eq 1 ] || rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install
  good "$(cargo --version 2>/dev/null || echo 'cargo present')"
elif have cargo; then
  good "$(cargo --version)"
  info "no rustup, so rust-toolchain.toml is NOT honoured — the compiler may differ from CI"
else
  bad "rustc/cargo — preinstalled in the Claude sandbox, so this is unexpected"
  info "workstation: install from https://rustup.rs"
fi

# ---------------------------------------------------------------- cargo-deny
# The one tool this project needs that no target environment preinstalls, and therefore the single
# most likely cause of a red `audit` step.
note "cargo-deny (supply-chain gate)"
if have cargo-deny; then
  good "$(cargo-deny --version 2>/dev/null)"
elif [ "$CHECK_ONLY" -eq 1 ]; then
  bad "cargo-deny"
elif have cargo-binstall; then
  info "installing via cargo-binstall (prebuilt)"
  cargo binstall --no-confirm cargo-deny >/dev/null 2>&1 && good "installed" || bad "install failed"
else
  # Compiles a large tree the first time. The sandbox's environment cache keeps the result, so
  # later sessions in the same environment skip this entirely.
  info "installing via cargo install — several minutes on a cold environment, cached afterwards"
  cargo install --locked cargo-deny >/dev/null 2>&1 && good "installed" || bad "install failed"
fi

# ---------------------------------------------------------------- Postgres
# Ubuntu ships only the CLIENT on PATH; the SERVER binaries sit under a versioned directory. In the
# Claude sandbox Postgres 16 is installed but not started, so both halves matter here.
note "Postgres (server binaries and a running cluster on 55432)"
PGBIN=""
if have initdb && have pg_ctl; then
  good "initdb/pg_ctl on PATH"
else
  PGBIN=$(ls -d /usr/lib/postgresql/*/bin /usr/pgsql-*/bin \
                /opt/homebrew/opt/postgresql*/bin /usr/local/opt/postgresql*/bin 2>/dev/null \
          | sort -Vr | head -1)
  if [ -n "$PGBIN" ] && [ -x "$PGBIN/initdb" ]; then
    export PATH="$PATH:$PGBIN"
    good "server binaries at $PGBIN (added to PATH)"
    # Persist for the rest of a Claude Code session when the SessionStart hook runs us.
    [ -n "${CLAUDE_ENV_FILE:-}" ] && printf 'export PATH="$PATH:%s"\n' "$PGBIN" >> "$CLAUDE_ENV_FILE"
  else
    bad "Postgres server binaries (initdb/pg_ctl)"
    info "sandbox: apt-get update && apt-get install -y postgresql-16"
  fi
fi

if [ "$CHECK_ONLY" -eq 0 ]; then
  if timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'; then
    good "cluster already answering on 55432"
  else
    info "starting the dev cluster"
    # dev-db.sh drives initdb/pg_ctl directly rather than `service postgresql start`, so it lands
    # on this project's own port/role/database instead of the distribution default on 5432.
    ./scripts/dev-db.sh up >/dev/null 2>&1 && good "started" || bad "could not start the cluster"
  fi
fi

# ---------------------------------------------------------------- conformance harness
note "conformance harness (pinned SDKs, schemathesis)"
if ! have python3; then
  bad "python3 — preinstalled in the Claude sandbox, so this is unexpected"
elif [ "$CHECK_ONLY" -eq 1 ]; then
  [ -x .venv-gate/bin/python ]         && good ".venv-gate"         || bad ".venv-gate"
  [ -x .venv-schemathesis/bin/python ] && good ".venv-schemathesis" || bad ".venv-schemathesis"
else
  [ -x .venv-gate/bin/python ]         || python3 -m venv .venv-gate
  [ -x .venv-schemathesis/bin/python ] || python3 -m venv .venv-schemathesis
  # Synced every run, not only on creation: a venv that exists but is stale is the bug that made
  # sdk_smoke.py fail with ModuleNotFoundError as though the SDK itself were broken (2026-08-18).
  .venv-gate/bin/pip install -q --disable-pip-version-check -r conformance/requirements-gate.txt \
    && good ".venv-gate synced" || bad ".venv-gate (pip failed — network access set to None?)"
  .venv-schemathesis/bin/pip install -q --disable-pip-version-check -r conformance/requirements-schemathesis.txt \
    && good ".venv-schemathesis synced" || bad ".venv-schemathesis (pip failed)"
fi

if ! have npm; then
  bad "npm — Node 22 is preinstalled at /opt/node22 in the Claude sandbox"
elif [ "$CHECK_ONLY" -eq 1 ]; then
  [ -d conformance/node_modules ] && good "conformance/node_modules" || bad "conformance/node_modules"
else
  # `npm ci`, not `npm install`: plan-ledger.sh's p1-gate-sdk-smoke asserts the EXACT official SDK
  # version in the gate transcript, so the lockfile is the contract and a resolver free to drift
  # would quietly break that obligation. Determinism beats a warm cache here.
  ( cd conformance && npm ci --silent ) >/dev/null 2>&1 \
    && good "conformance/node_modules installed" || bad "npm ci failed"
fi

# ---------------------------------------------------------------- verdict
# Re-derived from the machine, never from what the steps above believe they did. An install that
# reported success and left nothing behind must still come out MISSING here.
note "can ./scripts/check.sh run every step?"
gaps=0
req() {
  if eval "$2" >/dev/null 2>&1; then printf '   \033[32mready\033[0m    %-12s %s\n' "$1" "$3"
  else printf '   \033[31mBLOCKED\033[0m  %-12s %s\n' "$1" "$4"; gaps=$((gaps+1)); fi
}
req fmt        "command -v cargo"       "cargo"        "no cargo"
req clippy     "cargo clippy --version" "clippy"       "clippy component missing"
req build      "command -v cargo"       "cargo"        "no cargo"
req test       "timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'" \
                                        "database up"  "no Postgres on 55432"
req fixtures   "command -v cargo"       "cargo"        "no cargo"
req provenance "command -v cargo"       "cargo"        "no cargo"
req hooks      "true"                   "no deps"      ""
req audit      "command -v cargo-deny"  "cargo-deny"   "cargo-deny not installed"
req ledger     "true"                   "no deps"      ""

# The Lane L gate is heavier and is not part of check.sh; reported separately so its absence never
# reads as a failure of the everyday loop.
note "additionally, for ./scripts/verify.sh gate-lane-l"
for t in psql node python3; do
  have "$t" && good "$t" || bad "$t"
done
[ -d conformance/node_modules ] && good "conformance/node_modules" || bad "conformance/node_modules"

if [ "$gaps" -gt 0 ]; then
  printf '\n\033[31mbootstrap: INCOMPLETE\033[0m — %d step(s) cannot run. ./scripts/check.sh will FAIL, by design.\n' "$gaps"
  printf 'Fix the BLOCKED lines above and re-run. A check that cannot run must never report a pass.\n'
  exit 1
fi

printf '\n\033[32mbootstrap: READY\033[0m — every ./scripts/check.sh step can run here.\n'
exit 0
