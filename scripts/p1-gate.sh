#!/usr/bin/env bash
# P1 control-plane gate: stand up a throwaway candidate deployment, seed it with the resources the
# manifest's placeholders resolve against, and run the dual-target conformance diff.
#
# SECRETS: `amk init` prints the root key once. It is captured into a shell variable, printed back
# only as `<redacted>`, and never committed. It reaches each tool by environment variable, or — for
# curl, which has no environment channel for a header — through a 0600 temp file removed by the
# exit trap. It is never passed as a command-line argument: see the CURLRC block below for what
# that cost when it was.
set -uo pipefail
# Portable, matching scripts/check.sh's own convention — NOT a hardcoded path to the primary
# checkout. A hardcoded `/home/imma/projects/AgentMailKit` here silently built and served whatever
# was checked out THERE, never the tree this script itself lives in: run from inside a dispatch
# worktree (exactly the "run it early too" use this script's own contract calls for), it gated
# unmodified `main` against the live reference and reported this dispatch's own fixes as still
# missing — found by cross-checking a "still failing" gate result against a manual curl of the
# freshly built binary, which passed. `dirname "$0"` is this script's own directory regardless of
# caller cwd, so `.. ` from `scripts/` is always the repository root THIS script is part of.
cd "$(dirname "$0")/.."

# LANE SPLIT. Every conjunct below runs against OUR server and needs no credential except the root
# key this script mints itself — except one: the dual-target conformance diff, which calls
# api.agentmail.to and therefore needs a live third-party API key via `sdxd`.
#
# `--lane-l` runs everything except that diff. It exists so continuous integration can run the
# whole uncredentialed gate on every pull request WITHOUT a reference credential ever being present
# in a runner. A pull request from a fork can read any secret its workflow is given; the correct
# number of places a live AgentMail key can appear is zero, and Lane R stays a deliberate,
# operator-run step on a trusted machine.
#
# A Lane L run is NOT the P1 gate and must never be recorded as one. It prints its own lane, and
# `scripts/plan-ledger.sh`'s `p1-gate-conformance` reads fixture 25 for the diff's own summary
# line, which no Lane L run can produce.
LANE_L_ONLY=0
[ "${1:-}" = "--lane-l" ] && LANE_L_ONLY=1

DB=amk_p1gate
PORT=55432
BIND=127.0.0.1:8111

# Talk to the dev cluster over TCP, not `docker exec`. `scripts/dev-db.sh` no longer runs Postgres
# in a container (its own header carries why), so shelling into one made this whole gate
# unrunnable wherever there is no Docker daemon — and unlike a skipped unit test, that failure
# looked like "the gate is workstation-only" rather than "the gate has a container dependency it
# never needed". `psql` reaches the same cluster from anywhere, container or not.
MAINT_DSN="postgres://amk:amk-dev-local@127.0.0.1:${PORT}/postgres"
find_psql() {
  command -v psql 2>/dev/null && return 0
  local d
  for d in $(ls -d /usr/lib/postgresql/*/bin /usr/pgsql-*/bin \
                   /opt/homebrew/opt/postgresql*/bin /usr/local/opt/postgresql*/bin 2>/dev/null \
             | sort -Vr); do
    [ -x "$d/psql" ] && { echo "$d/psql"; return 0; }
  done
  return 1
}
PSQL="$(find_psql)" || { echo "FATAL: no psql client found; run ./scripts/dev-db.sh up" >&2; exit 1; }

cleanup() {
  [ -n "${AMKD_PID:-}" ] && kill "$AMKD_PID" 2>/dev/null
  rm -f "${CURLRC:-}"
  wait "${AMKD_PID:-}" 2>/dev/null
  "$PSQL" "$MAINT_DSN" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
  echo "teardown: amkd stopped, database $DB dropped"
}
trap cleanup EXIT

echo "== build =="
cargo build -p amk-cli --bins 2>&1 | tail -2

echo "== throwaway database =="
"$PSQL" "$MAINT_DSN" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
"$PSQL" "$MAINT_DSN" -qc "CREATE DATABASE \"$DB\"" || {
  echo "FATAL: cannot reach the dev cluster at 127.0.0.1:${PORT} — run ./scripts/dev-db.sh up" >&2
  exit 1; }

export AMK_DATABASE_URL="postgres://amk:amk-dev-local@127.0.0.1:${PORT}/${DB}"
# The reference account serves agentmail.to; ours serves a domain we actually own. The diff
# compares SHAPES, never values, so the domains differing is exactly what it must tolerate.
export AMK_PRIMARY_DOMAIN=appsynergy.io
export AMK_PRODUCT_NAME=AgentMailKit
export AMK_BIND="$BIND"

echo "== migrate =="
./target/debug/amk migrate || exit 1

echo "== init (root key captured, never printed) =="
INIT_OUT=$(./target/debug/amk init) || exit 1
CAND_KEY=$(printf '%s\n' "$INIT_OUT" | sed -n 's/.*root api key: *//p' | tr -d ' ')
printf '%s\n' "$INIT_OUT" | sed -E 's/(root api key: *).*/\1<redacted>/'
[ -n "$CAND_KEY" ] || { echo "FATAL: could not capture the root key"; exit 1; }

# ARGV IS NOT A PRIVATE CHANNEL. `curl -H "Authorization: Bearer $CAND_KEY"` writes the key into
# /proc/<pid>/cmdline, which is world-readable for as long as the process runs — no privilege
# needed. Found the hard way: a `pgrep -af` run against this script's own schemathesis invocation
# printed a live key straight into a transcript. Every request below presents the credential
# through this 0600 config file instead, and schemathesis takes it from the environment via
# conformance/schemathesis_checks.py's before_call hook. Environment variables are the right
# channel here (/proc/<pid>/environ is owner-only, argv is not), which is how every other secret
# in this script already travels.
CURLRC=$(umask 077; mktemp "${TMPDIR:-/tmp}/p1gate-curlrc.XXXXXX") || exit 1
printf 'header = "Authorization: Bearer %s"\n' "$CAND_KEY" > "$CURLRC"

echo "== operator configuration (direct UPDATE — no endpoint sets these; that is the honest state
     the divergence-1 contract itself names) =="
# The reference organization carries values for these eight columns; ours has none until an
# operator sets one directly. Shape comparison never looks at the VALUE, only at whether the key
# is present, so any non-null value here proves hydration end-to-end. `billing_plan_id` and
# `clerk_organization_id` are deliberately NOT here — excluded by decision (dispatch contract,
# divergence 1), pinned by a test in amk-types, and the one expected residual diff on this
# endpoint that this gate cannot and must not close.
"$PSQL" "$AMK_DATABASE_URL" -qc \
  "UPDATE organizations SET inbox_limit=1000, domain_limit=10, daily_send_limit=5000, \
     five_minute_send_limit=100, first_day_recipient_limit=200, \
     first_week_recipient_limit=1000, tracking_allowed=true, \
     authentication_id='p1gate-auth', authentication_type='p1gate-type'" >/dev/null

echo "== serve =="
./target/debug/amkd --role api &
AMKD_PID=$!
for _ in $(seq 1 40); do
  curl -fsS -o /dev/null -K "$CURLRC" "http://${BIND}/v0/auth/me" && break
  sleep 0.25
done

echo "== seed the candidate to match the reference's STATE, not just its schema =="
# Optionals are omitted when absent, so a candidate whose resources simply never set `client_id`
# reports it "missing" against a reference whose resources do. That is a data difference wearing a
# shape difference's clothes, and it hid the real diffs in the first run. Same for
# `next_page_token`: it is absent on the last page, so a single-pod candidate can never emit one.
api() { curl -fsS -K "$CURLRC" -X "$1" "http://${BIND}$2" \
          -H 'Content-Type: application/json' ${3:+-d "$3"}; }
enc() { python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$1"; }

# The default pod `amk init` created, captured before a second pod exists so this id is
# unambiguous (there is exactly one row to pick).
POD1=$(api GET '/v0/pods' | python3 -c 'import json,sys; print(json.load(sys.stdin)["pods"][0]["pod_id"])')

# A second pod: `?limit=1` on /v0/pods needs a next page as the reference has, and — since pods
# sort newest-first (fixture 22) — this is also the pod the manifest's own resolver will pick for
# `{pod_id}`, the same id `POD` below captures.
api POST /v0/pods '{"name":"p1gate second pod","client_id":"p1gate-pod-client"}' >/dev/null
POD=$(api GET '/v0/pods?limit=1' | python3 -c 'import json,sys; print(json.load(sys.stdin)["pods"][0]["pod_id"])')

# The reference account holds a MIX: some inboxes carry `client_id`, some do not — its element-
# shape SET has two members and a candidate whose inboxes all look alike can never match it. But
# the manifest's `{inbox_id}` resolver always picks the NEWEST (limit=1, newest-first), and on the
# reference that resolved inbox carries `client_id` — so the client_id-bearing one here must be
# both the newest overall AND live in $POD (the resolved pod), never POD1: put the two variants in
# different pods so $POD's own unlimited listing shows exactly ONE element shape (matching what
# the reference's resolved pod shows), while the org-wide listing still sees the mix.
api POST "/v0/pods/$POD1/inboxes" '{"username":"p1gate-a"}' >/dev/null
api POST "/v0/pods/$POD/inboxes" '{"username":"p1gate-b","client_id":"p1gate-inbox-b"}' \
  | python3 -c 'import json,sys; print("  inbox:", json.load(sys.stdin)["inbox_id"])'
INBOX=$(api GET "/v0/pods/$POD/inboxes?limit=1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["inboxes"][0]["inbox_id"])')

# One inbox-scoped key, carrying `permissions` — the reference's own inbox-scoped key does, so its
# element shape has that key present, not absent. It must NOT sit at $POD or at $INBOX: the
# reference's own pod- and inbox-mounted `/api-keys` listings, at whatever its resolver picks, come
# back empty (checked live), so a key seeded at the resolved pod/inbox is a shape the reference
# never shows at that mount. POD1's inbox is neither, and the org-mount listing spans every scope
# regardless of which pod or inbox holds the key.
INBOX1=$(api GET "/v0/pods/$POD1/inboxes?limit=1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["inboxes"][0]["inbox_id"])')
api POST "/v0/inboxes/$(enc "$INBOX1")/api-keys" \
  '{"name":"p1gate inbox key","permissions":{"message_read":true}}' >/dev/null

for r in pods inboxes api-keys; do
  printf '  %-9s ' "$r"; api GET "/v0/$r" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])'
done

echo
if [ "$LANE_L_ONLY" -eq 1 ]; then
  echo "== dual-target conformance diff: SKIPPED (Lane R) =="
  echo "   --lane-l was passed: this run holds no reference credential and did not call"
  echo "   api.agentmail.to. This run is NOT the P1 gate."
  GATE_EXIT=0
else
echo "== dual-target conformance diff: api.agentmail.to vs localhost =="
# The real gate: conformance/dual_target.py's own structural diff, not the ad hoc key-set probe
# that established the four divergences in the first place (that was a debugging aid run by hand;
# this is the thing p1-gate-conformance actually reads). Its own summary line ("N requests, M
# compared, X skipped, Y with structural diffs") is the ledger's pass/fail criterion verbatim.
AGENTMAIL_API_KEY='sdxd:agentmail' CAND_KEY="$CAND_KEY" sdxd run -- bash -c '
  REF_KEY="$AGENTMAIL_API_KEY" CAND_BASE="http://127.0.0.1:8111" CAND_KEY="$CAND_KEY" \
    python3 conformance/dual_target.py conformance/manifest.json'
GATE_EXIT=$?
echo "dual_target.py exit: $GATE_EXIT"
fi

echo
echo "== P1 gate, second half: the unmodified official Python SDK against the same server =="
# The diff proves our responses have the reference's SHAPE. This proves the official client can
# actually USE them — deserialize into its own typed models, page, round-trip a full CRUD cycle.
# A response can be structurally identical and still break a typed client (a pydantic validator, a
# required field the model insists on), so neither half subsumes the other.
# Create the venv if absent, then ALWAYS sync it to the pinned requirements. The guard used to be
# `if [ ! -x .venv-gate/bin/python ]` around both lines, which never repaired a venv that existed
# but was incomplete — a half-built venv, or one cached from an earlier pin set, was reused
# silently and the smoke failed with ModuleNotFoundError as if the SDK were broken. Observed
# exactly that on 2026-08-18. `pip install -r` is a no-op in about a second when already
# satisfied, so syncing unconditionally costs nothing and removes the whole failure class. This
# also makes a restored CI cache safe: a stale cache is corrected, never trusted.
[ -x .venv-gate/bin/python ] || python3 -m venv .venv-gate
.venv-gate/bin/pip install -q --disable-pip-version-check -r conformance/requirements-gate.txt
AMK_BASE="http://${BIND}" AMK_KEY="$CAND_KEY" .venv-gate/bin/python conformance/sdk_smoke.py
SMOKE_EXIT=$?
echo "sdk_smoke.py exit: $SMOKE_EXIT"

echo
echo "== P1 gate, third half: the unmodified official NODE SDK against the same server =="
# The plan's P1 gate says "Python+Node SDK smoke", and the two clients are generated from one spec
# but not from one codebase. What the Node client does differently, verified by probing its own
# serialization layer rather than assumed: it maps the wire's snake_case to camelCase (a field we
# misspell arrives as `undefined`, not as an error), it coerces RFC 3339 strings to real `Date`
# objects, and it routes an `am_eu_`-prefixed key to AgentMail's EU host — the one SDK behaviour
# that could take a client off our base URL entirely. What it does NOT do, contrary to the obvious
# guess: reject a response missing a field its model marks required. Every resource client parses
# with `skipValidation: true, unrecognizedObjectKeys: "passthrough"`, so a missing required field is
# accepted as `undefined` and our extra live fields (`organization_id`, `smtp_id`) pass through
# un-mapped. The typed-client-rejects-bad-shapes check the plan wants therefore has to be written as
# assertions here; it is not something the SDK performs for us.
[ -d conformance/node_modules ] || npm install --prefix conformance --silent
AMK_BASE="http://${BIND}" AMK_KEY="$CAND_KEY" node conformance/sdk_smoke.mjs
NODE_EXIT=$?
echo "sdk_smoke.mjs exit: $NODE_EXIT"

echo
# ---- seed the resources the by-id operations need -------------------------------------------
#
# Mounting the message/thread get-by-id operations made the schemathesis half WORSE before it made
# it better: the fuzzer generates random UUIDs and Message-IDs for `{thread_id}`/`{message_id}`,
# every one 404s, and hypothesis aborts the operation with `filter_too_much`. The 41-operation run
# reported 8 failed health checks and warned that 13 operations "repeatedly returned 404,
# preventing tests from reaching your API's core logic" — the exit code went red while the coverage
# silently fell. So seed one real row of each and bind the ids in conformance/schemathesis.toml.
#
# Threads and messages are seeded with SQL because no endpoint creates them yet: send is P2's
# outbound half and ingest is the daemon. This is the same direct-UPDATE honesty as the operator
# configuration above — stated, not hidden.
echo
echo "== seed the by-id fixtures =="
ST_INBOX=$(api POST /v0/inboxes '{"username":"stfixture"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["inbox_id"])')
ST_POD=$(api GET /v0/pods '' | python3 -c 'import sys,json;print(json.load(sys.stdin)["pods"][0]["pod_id"])')
ST_KEY_ID=$(api POST /v0/api-keys '{"name":"st fixture"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["api_key_id"])')
ST_THREAD=$(python3 -c 'import uuid;print(uuid.uuid4())')
ST_MESSAGE="<stfixture@appsynergy.io>"
"$PSQL" "$AMK_DATABASE_URL" -qc "
  INSERT INTO threads (thread_id, organization_id, pod_id, inbox_id, labels, \"timestamp\",
                       senders, recipients, subject, preview, last_message_id, message_count, size)
  SELECT '$ST_THREAD', organization_id, '$ST_POD', '$ST_INBOX', ARRAY['received'], now(),
         ARRAY['sender@example.test'], ARRAY['$ST_INBOX'], 'st fixture', 'preview',
         '$ST_MESSAGE', 1, 42
    FROM inboxes WHERE inbox_id = '$ST_INBOX';
  INSERT INTO messages (inbox_id, message_id, organization_id, pod_id, thread_id, labels,
                        \"timestamp\", from_address, to_addresses, size)
  SELECT '$ST_INBOX', '$ST_MESSAGE', organization_id, '$ST_POD', '$ST_THREAD', ARRAY['received'],
         now(), 'sender@example.test', ARRAY['$ST_INBOX'], 42
    FROM inboxes WHERE inbox_id = '$ST_INBOX';" >/dev/null || {
  echo "FATAL: could not seed the by-id fixtures" >&2; exit 1; }
export AMK_ST_INBOX_ID="$ST_INBOX" AMK_ST_POD_ID="$ST_POD" AMK_ST_THREAD_ID="$ST_THREAD"
export AMK_ST_MESSAGE_ID="$ST_MESSAGE" AMK_ST_API_KEY_ID="$ST_KEY_ID"
echo "  inbox=$ST_INBOX pod=$ST_POD thread=$ST_THREAD api_key=$ST_KEY_ID"

echo "== P1 gate, fourth part: schemathesis over the implemented paths =="
# The last clause of the plan's P1 gate. The two SDK smokes walk the paths a client is meant to
# walk; this walks the ones nobody would write down. Scope is DERIVED from router() and reconciled
# against openapi.json on every run (conformance/schemathesis_scope.py) rather than listed here —
# a hand-written path list is right when written and silently wrong the moment a route moves.
#
# CHECKS, and why this set. `status_code_conformance` is EXCLUDED, with cause: the schema is
# AgentMail's own openapi.json, which this project has caught being wrong against live captures
# four times, and it is 0-for-3 on DELETE statuses alone (fixtures 22 and 23 — it documents 200
# where the live API returns 204 and 202). Statuses already have a far better oracle: fixture 25
# diffs ours against the LIVE reference's for every request. What is kept is what the spec is good
# at — response shapes, content types, no 5xx — plus three checks of our own carrying the
# invariants no OpenAPI document can express (conformance/schemathesis_checks.py).
# Same reasoning as .venv-gate above: create if absent, sync every run.
[ -x .venv-schemathesis/bin/python ] || python3 -m venv .venv-schemathesis
.venv-schemathesis/bin/pip install -q --disable-pip-version-check -r conformance/requirements-schemathesis.txt
# `SCOPE_EXIT=$?` after a `mapfile < <(...)` reads MAPFILE's status, not the script's — which is
# always 0, so a router/spec disagreement would have been announced and then ignored. Capture the
# script's own exit through a command substitution, which propagates it.
INCLUDE_ARGS=$(python3 conformance/schemathesis_scope.py --include-args)
SCOPE_EXIT=$?
mapfile -t INCLUDE <<< "$INCLUDE_ARGS"
if [ "$SCOPE_EXIT" -ne 0 ]; then
  echo "FATAL: router() and openapi.json disagree — run scripts/derive-implemented-paths.sh"
fi
PYTHONPATH=. SCHEMATHESIS_HOOKS=conformance.schemathesis_checks AMK_KEY="$CAND_KEY" \
  .venv-schemathesis/bin/st --config-file conformance/schemathesis.toml \
    run reference/openapi.json \
    --url "http://${BIND}" \
    "${INCLUDE[@]}" \
    --checks not_a_server_error,content_type_conformance,response_schema_conformance,optionals_are_omitted_never_null,timestamps_are_wire_exact,error_shape_is_one_of_the_two \
    --mode all \
    --max-examples "${AMK_ST_EXAMPLES:-40}" \
    --seed 1 \
    --continue-on-failure \
    --output-sanitize true
ST_EXIT=$?
echo "schemathesis exit: $ST_EXIT"

echo
if [ "$LANE_L_ONLY" -eq 1 ]; then
  echo "== lane: L (local only) =="
  echo "   Ran: schemathesis, Python SDK smoke, Node SDK smoke, scope reconciliation."
  echo "   Did NOT run: the dual-target conformance diff (Lane R, needs the reference key)."
  echo "   This result cannot satisfy p1-gate-conformance."
else
  echo "== lane: L + R (full P1 gate) =="
fi

# All conjuncts must pass. Reporting only the diff's verdict would let a broken client ship behind
# a clean diff, which is the exact gap the SDK halves exist to close. Under --lane-l, GATE_EXIT is
# forced to 0 above because the diff did not run — the lane banner, not this conjunction, is what
# tells a reader which checks are actually behind this exit code.
[ "$GATE_EXIT" -eq 0 ] && [ "$SMOKE_EXIT" -eq 0 ] && [ "$NODE_EXIT" -eq 0 ] \
  && [ "$ST_EXIT" -eq 0 ] && [ "$SCOPE_EXIT" -eq 0 ]
exit $?
