#!/usr/bin/env bash
# Provision a working environment for this repository. Idempotent; safe to re-run.
#
#   ./scripts/bootstrap.sh          provision everything, then report what is runnable
#   ./scripts/bootstrap.sh --check  report only, change nothing
#
# WHY THIS IS TRACKED IN THE REPOSITORY. Most work on this project happens in an ephemeral sandbox
# that starts with the repo and whatever the base image happens to carry. Anything a check needs in
# order to run must therefore be *in the repo* — a script that installs it — rather than assumed to
# be lying around. Otherwise `./scripts/check.sh` reports NOT RUN for half its steps, and a step
# that is chronically NOT RUN is a step nobody is really running.
#
# NOT RUN (scripts/verify.sh) makes a missing prerequisite visible. This script makes it rare.
# The two are complements: this closes the gap, that one proves the gap is closed.
#
# Deliberately NOT used by CI. GitHub Actions provisions through the mechanisms its runners are
# built for — `services:` containers, setup-python, setup-node, a prebuilt cargo-deny — which are
# faster and cacheable there. This script is for humans and sandboxes.
set -uo pipefail
cd "$(dirname "$0")/.."

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

note() { printf '\033[1m==\033[0m %s\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }
skip() { printf '   already present: %s\n' "$1"; }

# ---------------------------------------------------------------- Rust
# rustup reads rust-toolchain.toml on its own, so simply invoking cargo installs and selects the
# pinned toolchain and its components. Nothing here names a version — one declaration, one place.
note "Rust toolchain (pinned by rust-toolchain.toml)"
if have rustup; then
  [ "$CHECK_ONLY" -eq 1 ] || rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install
  have cargo && cargo --version | sed 's/^/   /'
else
  printf '   \033[33mrustup MISSING\033[0m — install from https://rustup.rs\n'
fi

# ---------------------------------------------------------------- cargo-deny
# The one tool that is genuinely absent from a stock image, and therefore the step most likely to
# sit at NOT RUN forever. Installed from a prebuilt binary when possible; `cargo install` compiles
# a large tree and takes minutes.
note "cargo-deny (supply-chain gate)"
if have cargo-deny; then
  skip "$(cargo-deny --version 2>/dev/null)"
elif [ "$CHECK_ONLY" -eq 1 ]; then
  printf '   \033[33mMISSING\033[0m — `verify.sh audit` will report NOT RUN\n'
elif have cargo-binstall; then
  cargo binstall --no-confirm cargo-deny || printf '   \033[33minstall failed\033[0m\n'
else
  printf '   installing (compiles from source; several minutes, once per container)\n'
  cargo install --locked cargo-deny || printf '   \033[33minstall failed\033[0m\n'
fi

# ---------------------------------------------------------------- Postgres
# Debian hides the SERVER binaries under /usr/lib/postgresql/<v>/bin while putting only the client
# on PATH, so `psql` present does not imply `initdb` present. scripts/dev-db.sh already searches
# those directories; this puts them on PATH too, so the failure mode is not "initdb: not found".
note "Postgres"
PGBIN=$(ls -d /usr/lib/postgresql/*/bin /usr/pgsql-*/bin 2>/dev/null | sort -Vr | head -1)
if have initdb; then
  skip "initdb on PATH"
elif [ -n "$PGBIN" ]; then
  printf '   server binaries at %s (not on PATH)\n' "$PGBIN"
  export PATH="$PATH:$PGBIN"
  # Persist for the rest of a Claude Code session when the hook runs us.
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    printf 'export PATH="$PATH:%s"\n' "$PGBIN" >> "$CLAUDE_ENV_FILE"
  fi
else
  printf '   \033[33mno Postgres server binaries\033[0m — apt-get install postgresql-16 (or any 16+)\n'
fi

if [ "$CHECK_ONLY" -eq 0 ]; then
  if timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'; then
    skip "dev database already answering on 55432"
  else
    printf '   starting the dev cluster\n'
    ./scripts/dev-db.sh up >/dev/null 2>&1 \
      || printf '   \033[33mcould not start it\033[0m — `verify.sh test` will report NOT RUN\n'
  fi
fi

# ---------------------------------------------------------------- conformance harness
# Both virtualenvs and the Node SDK, all version-pinned. p1-gate.sh creates and syncs these itself,
# but doing it here means the FIRST gate run is not also the run that pays for the installs.
note "conformance harness (pinned SDKs and schemathesis)"
if [ "$CHECK_ONLY" -eq 1 ]; then
  [ -x .venv-gate/bin/python ]        && skip ".venv-gate"        || printf '   .venv-gate absent\n'
  [ -x .venv-schemathesis/bin/python ] && skip ".venv-schemathesis" || printf '   .venv-schemathesis absent\n'
  [ -d conformance/node_modules ]     && skip "conformance/node_modules" || printf '   node_modules absent\n'
elif have python3; then
  [ -x .venv-gate/bin/python ]         || python3 -m venv .venv-gate
  [ -x .venv-schemathesis/bin/python ] || python3 -m venv .venv-schemathesis
  # Sync every run rather than only on creation — a venv that exists but is stale is the bug that
  # made sdk_smoke.py fail with ModuleNotFoundError as though the SDK were broken (2026-08-18).
  .venv-gate/bin/pip install -q --disable-pip-version-check -r conformance/requirements-gate.txt \
    || printf '   \033[33mpip install failed (offline?)\033[0m\n'
  .venv-schemathesis/bin/pip install -q --disable-pip-version-check -r conformance/requirements-schemathesis.txt \
    || printf '   \033[33mpip install failed (offline?)\033[0m\n'
fi

if [ "$CHECK_ONLY" -eq 0 ] && have npm; then
  # `npm ci` rather than `npm install`: scripts/plan-ledger.sh's p1-gate-sdk-smoke asserts the
  # EXACT official SDK version in the gate transcript, so the lockfile is the contract and a
  # resolver free to drift would quietly break that obligation. Determinism beats warm-cache speed.
  ( cd conformance && npm ci --silent ) || printf '   \033[33mnpm ci failed (offline?)\033[0m\n'
fi

# ---------------------------------------------------------------- what can actually run now
note "what ./scripts/check.sh can run here"
report() {
  if eval "$2" >/dev/null 2>&1; then printf '   \033[32mready\033[0m    %s\n' "$1"
  else                               printf '   \033[33mNOT RUN\033[0m  %s — %s\n' "$1" "$3"; fi
}
report fmt        "command -v cargo"      "no cargo"
report clippy     "command -v cargo"      "no cargo"
report fixtures   "command -v cargo"      "no cargo"
report test       "timeout 2 bash -c '(exec 3<>/dev/tcp/127.0.0.1/55432) 2>/dev/null'" "no database on 55432"
report provenance "command -v cargo"      "no cargo"
report hooks      "true"                  ""
report audit      "command -v cargo-deny" "cargo-deny not installed"
report ledger     "true"                  ""
printf '\nRun \033[1m./scripts/check.sh\033[0m next. Anything still NOT RUN is covered by CI.\n'
