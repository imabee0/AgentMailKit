#!/bin/bash
# SessionStart hook — provision a Claude Code on the web sandbox so every check can actually run.
#
# Thin on purpose: scripts/bootstrap.sh is the single definition of "what this repo needs", tracked
# and runnable by hand. This file only decides WHEN to call it. Same pattern as scripts/verify.sh
# holding the step definitions while scripts/check.sh only sequences them.
#
# Synchronous, deliberately. Async would start the session sooner, but the first thing a session
# here does is run ./scripts/check.sh, and a race between that and the Postgres cluster coming up
# produces a NOT RUN for `test` — the exact gap this hook exists to close.
set -uo pipefail

# Local machines are already provisioned and should not have their dev database started from under
# them by an editor session.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}" || exit 0

# bootstrap.sh exits non-zero when it could not provision. This hook still exits 0, so a session
# always opens — a sandbox you cannot get into is worse than one you must fix. The failure is NOT
# swallowed: bootstrap prints which steps are BLOCKED, and ./scripts/check.sh will FAIL on exactly
# those, because scripts/verify.sh treats a missing dependency as a failure rather than a caveat.
# The gate is check.sh, not this hook; this only tries to make check.sh able to pass.
if ! ./scripts/bootstrap.sh; then
  echo
  echo "############################################################"
  echo "bootstrap FAILED — this sandbox is not fully provisioned."
  echo "./scripts/check.sh will FAIL on the BLOCKED steps above."
  echo "That is deliberate: a check that cannot run is never a pass."
  echo "############################################################"
fi
exit 0
