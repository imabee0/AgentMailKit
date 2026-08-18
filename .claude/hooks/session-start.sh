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

# Never block a session from starting. A failed bootstrap must degrade to "some steps report NOT
# RUN", which is visible and recoverable, not to "the session will not open".
./scripts/bootstrap.sh || echo "bootstrap: incomplete — ./scripts/check.sh will report what is NOT RUN"
exit 0
