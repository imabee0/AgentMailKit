#!/usr/bin/env bash
# P1 control-plane gate: stand up a throwaway candidate deployment, seed it with the resources the
# manifest's placeholders resolve against, and run the dual-target conformance diff.
#
# SECRETS: `amk init` prints the root key once. It is captured into a shell variable, passed to the
# harness through the environment, and never echoed, never written to a file, never committed.
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

DB=amk_p1gate
CTR=amk-dev-postgres
PORT=55432
BIND=127.0.0.1:8111

cleanup() {
  [ -n "${AMKD_PID:-}" ] && kill "$AMKD_PID" 2>/dev/null
  wait "${AMKD_PID:-}" 2>/dev/null
  docker exec "$CTR" psql -U amk -d postgres -qc \
    "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
  echo "teardown: amkd stopped, database $DB dropped"
}
trap cleanup EXIT

echo "== build =="
cargo build -p amk-cli --bins 2>&1 | tail -2

echo "== throwaway database =="
docker exec "$CTR" psql -U amk -d postgres -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
docker exec "$CTR" psql -U amk -d postgres -qc "CREATE DATABASE \"$DB\"" || exit 1

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

echo "== operator configuration (direct UPDATE — no endpoint sets these; that is the honest state
     the divergence-1 contract itself names) =="
# The reference organization carries values for these eight columns; ours has none until an
# operator sets one directly. Shape comparison never looks at the VALUE, only at whether the key
# is present, so any non-null value here proves hydration end-to-end. `billing_plan_id` and
# `clerk_organization_id` are deliberately NOT here — excluded by decision (dispatch contract,
# divergence 1), pinned by a test in amk-types, and the one expected residual diff on this
# endpoint that this gate cannot and must not close.
docker exec "$CTR" psql -U amk -d "$DB" -qc \
  "UPDATE organizations SET inbox_limit=1000, domain_limit=10, daily_send_limit=5000, \
     five_minute_send_limit=100, first_day_recipient_limit=200, \
     first_week_recipient_limit=1000, tracking_allowed=true, \
     authentication_id='p1gate-auth', authentication_type='p1gate-type'" >/dev/null

echo "== serve =="
./target/debug/amkd --role api &
AMKD_PID=$!
for _ in $(seq 1 40); do
  curl -fsS -o /dev/null "http://${BIND}/v0/auth/me" -H "Authorization: Bearer $CAND_KEY" && break
  sleep 0.25
done

echo "== seed the candidate to match the reference's STATE, not just its schema =="
# Optionals are omitted when absent, so a candidate whose resources simply never set `client_id`
# reports it "missing" against a reference whose resources do. That is a data difference wearing a
# shape difference's clothes, and it hid the real diffs in the first run. Same for
# `next_page_token`: it is absent on the last page, so a single-pod candidate can never emit one.
api() { curl -fsS -X "$1" "http://${BIND}$2" -H "Authorization: Bearer $CAND_KEY" \
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

echo
echo "== P1 gate, second half: the unmodified official Python SDK against the same server =="
# The diff proves our responses have the reference's SHAPE. This proves the official client can
# actually USE them — deserialize into its own typed models, page, round-trip a full CRUD cycle.
# A response can be structurally identical and still break a typed client (a pydantic validator, a
# required field the model insists on), so neither half subsumes the other.
if [ ! -x .venv-gate/bin/python ]; then
  python3 -m venv .venv-gate
  .venv-gate/bin/pip install -q -r conformance/requirements-gate.txt
fi
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

# All three must pass. Reporting only the diff's verdict would let a broken client ship behind a
# clean diff, which is the exact gap the SDK halves exist to close.
[ "$GATE_EXIT" -eq 0 ] && [ "$SMOKE_EXIT" -eq 0 ] && [ "$NODE_EXIT" -eq 0 ]
exit $?
