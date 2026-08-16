#!/usr/bin/env bash
# P1 control-plane gate: stand up a throwaway candidate deployment, seed it with the resources the
# manifest's placeholders resolve against, and run the dual-target conformance diff.
#
# SECRETS: `amk init` prints the root key once. It is captured into a shell variable, passed to the
# harness through the environment, and never echoed, never written to a file, never committed.
set -uo pipefail
cd /home/imma/projects/AgentMailKit

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
# Seed a second pod and set `client_id` on what the placeholders resolve to.
api() { curl -fsS -X "$1" "http://${BIND}$2" -H "Authorization: Bearer $CAND_KEY" \
          -H 'Content-Type: application/json' ${3:+-d "$3"}; }
enc() { python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$1"; }

# A second pod, so `?limit=1` on /v0/pods has a next page as it does on the reference.
api POST /v0/pods '{"name":"p1gate second pod","client_id":"p1gate-pod-client"}' >/dev/null

# Seed into the pod the RESOLVER will pick, not whichever pod happens to be default — the two need
# not be the same, and when they were not, `{inbox_id}` resolved to nothing and three requests
# silently dropped out of the gate.
POD=$(api GET '/v0/pods?limit=1' | python3 -c 'import json,sys; print(json.load(sys.stdin)["pods"][0]["pod_id"])')

# The reference account holds a MIX: some inboxes carry `client_id`, some do not. Since optionals
# are omitted when absent, its element-shape SET has two members and a candidate whose inboxes all
# look alike can never match it. Seed both variants.
api POST "/v0/pods/$POD/inboxes" '{"username":"p1gate-a","client_id":"p1gate-inbox-a"}' \
  | python3 -c 'import json,sys; print("  inbox:", json.load(sys.stdin)["inbox_id"])'
api POST "/v0/pods/$POD/inboxes" '{"username":"p1gate-b"}' >/dev/null

# Keys at all three scopes, for the same reason: the org-mount listing returns every key, so its
# element-shape set spans org-scoped, pod-scoped and inbox-scoped.
INBOX=$(api GET "/v0/pods/$POD/inboxes?limit=1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["inboxes"][0]["inbox_id"])')
api POST "/v0/inboxes/$(enc "$INBOX")/api-keys" '{"name":"p1gate inbox key"}' >/dev/null
api POST "/v0/pods/$POD/api-keys" '{"name":"p1gate pod key"}' >/dev/null

for r in pods inboxes api-keys; do
  printf '  %-9s ' "$r"; api GET "/v0/$r" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])'
done

echo
echo "== dual-target conformance diff: api.agentmail.to vs localhost =="
AGENTMAIL_API_KEY='sdxd:agentmail' CAND_KEY="$CAND_KEY" sdxd run -- bash -c '
  REF_KEY="$AGENTMAIL_API_KEY" CAND_BASE="http://127.0.0.1:8111" CAND_KEY="$CAND_KEY" \
    python3 '"$KEYSETS"' '
echo "keysets exit: $?"
