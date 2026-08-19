#!/usr/bin/env bash
# Does the COMPOSED BINARY work, configured the way an operator would configure it?
#
# WHY THIS EXISTS
#
# On 2026-08-19 an audit found that `amkd --role api` could not send mail. `AppState::new` built
# an empty `Keyring` unconditionally and no environment variable existed to inject a DKIM key, so
# every deployed send answered `NoSigningKey`. The crate compiled. All 697 tests passed. Three
# review lenses returned clean. `check.sh`, `shape-provenance.sh` and `plan-ledger.sh` were green.
#
# Every one of those gates tested a LIBRARY or a HANDLER. `amk-outbound` was exercised directly;
# `amk-http` was exercised through handlers that injected their own keyring. Nothing started the
# binary, read the environment, and watched bytes leave. This does.
#
# The rule it establishes: every crate that joins the workspace adds a line here. A capability
# that is not observed end to end through the shipped binary is not a capability, however many
# unit tests cover its parts.
#
# EXIT CODES
#   0  passed
#   1  FAILED -- a real defect
#   2  prerequisites unavailable (no Postgres). Never conflated with 0: `scripts/check.sh` prints
#      a distinct degraded line for this, because "did not run" reported as "passed" is the exact
#      class of failure this script was written to end.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

PORT=55432
DB=amk_binary_smoke
HTTP=127.0.0.1:8123
SMTPD=127.0.0.1:8125
SINK=52525
DOMAIN=smoke.test
SELECTOR=s20260819

fail=0
step()  { printf '\n== %s ==\n' "$1"; }
ok()    { printf '  ok    %s\n' "$1"; }
bad()   { printf '  FAIL  %s\n' "$1"; fail=1; }

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

if ! timeout 1 bash -c "(exec 3<>/dev/tcp/127.0.0.1/$PORT)" 2>/dev/null; then
  echo "binary-smoke: NOT RUN -- no Postgres on 127.0.0.1:$PORT (run ./scripts/dev-db.sh up)"
  exit 2
fi
PSQL="$(find_psql)" || { echo "binary-smoke: NOT RUN -- no psql client found"; exit 2; }
command -v openssl >/dev/null || { echo "binary-smoke: NOT RUN -- no openssl to mint a test key"; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/amk-smoke.XXXXXX") || exit 2
MAINT="postgres://amk:amk-dev-local@127.0.0.1:${PORT}/postgres"

cleanup() {
  for pid in ${API_PID:-} ${SMTPD_PID:-} ${SINK_PID:-}; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  "$PSQL" "$MAINT" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
  # The work directory holds a private key. Removing it is part of the test, not tidiness.
  rm -rf "$WORK"
  echo "teardown: processes stopped, database dropped, key material removed from $WORK"
}
trap cleanup EXIT

step "release binaries"
AMK=./target/release/amk
AMKD=./target/release/amkd
# CI builds once, uploads the artifact, and every downstream job downloads it -- so this gate must
# NOT rebuild there. Locally the build is the convenience; in CI rebuilding would break the
# build-once rule and, worse, mean the gate exercised a different binary from the one shipped.
if [ "${AMK_SMOKE_SKIP_BUILD:-0}" = "1" ]; then
  for b in "$AMK" "$AMKD"; do
    [ -x "$b" ] || { echo "AMK_SMOKE_SKIP_BUILD=1 but $b is missing or not executable"; exit 1; }
  done
  echo "  using the prebuilt binaries (AMK_SMOKE_SKIP_BUILD=1)"
else
  # Release, not debug: a gate that proves something about a binary nobody ships proves nothing.
  cargo build --release -p amk-cli --bins 2>&1 | tail -2 || exit 1
fi

step "mint a throwaway DKIM key"
KEYS="$WORK/keys"; mkdir -p "$KEYS"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform DER \
  -out "$KEYS/${SELECTOR}.${DOMAIN}.der" 2>/dev/null || { echo "openssl failed"; exit 1; }
chmod 600 "$KEYS/${SELECTOR}.${DOMAIN}.der"
ok "$SELECTOR._domainkey.$DOMAIN (2048-bit, DER, throwaway)"

# A self-signed cert for the sink. amk-outbound falls back to plaintext ONLY on port 25, so a
# smarthost anywhere else must complete a TLS handshake -- which means this gate exercises the
# real production delivery path (STARTTLS to a smarthost) rather than a plaintext shortcut no
# deployment uses. The sender sets allow_invalid_certs(), so self-signed is sufficient.
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=localhost" \
  -keyout "$WORK/sink.key" -out "$WORK/sink.crt" 2>/dev/null || { echo "openssl req failed"; exit 1; }
chmod 600 "$WORK/sink.key"

step "throwaway database"
"$PSQL" "$MAINT" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
"$PSQL" "$MAINT" -qc "CREATE DATABASE \"$DB\"" >/dev/null || exit 1
export AMK_DATABASE_URL="postgres://amk:amk-dev-local@127.0.0.1:${PORT}/${DB}"
export AMK_PRIMARY_DOMAIN="$DOMAIN"
export AMK_PRODUCT_NAME=AgentMailKit
"$AMK" migrate >/dev/null || exit 1

INIT=$("$AMK" init) || exit 1
KEY=$(printf '%s\n' "$INIT" | sed -n 's/.*root api key: *//p' | tr -d ' ')
[ -n "$KEY" ] || { echo "could not capture the root key"; exit 1; }
# argv is world-readable via /proc/<pid>/cmdline; the credential travels by 0600 file, exactly as
# scripts/p1-gate.sh does and for the same reason recorded there.
CURLRC=$(umask 077; mktemp "$WORK/curlrc.XXXXXX")
printf 'header = "Authorization: Bearer %s"\n' "$KEY" > "$CURLRC"
api() { curl -fsS -K "$CURLRC" -X "$1" "http://${HTTP}$2" \
          -H 'Content-Type: application/json' ${3:+-d "$3"}; }
enc() { python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$1"; }
ok "database migrated, organization initialised"

# ---------------------------------------------------------------------------------------------
step "GATE 1 -- a sending role REFUSES to start with an empty key directory"
# The failure this whole script exists for, asserted in the direction that would let it back in.
# An operator who set AMK_DKIM_KEYS asked for a deployment that sends; starting anyway with an
# empty keyring is the silent-success state, not a lenient default.
mkdir -p "$WORK/emptykeys"
if AMK_DKIM_KEYS="$WORK/emptykeys" AMK_BIND="$HTTP" timeout 20 "$AMKD" --role api >"$WORK/empty.log" 2>&1; then
  bad "amkd started with an empty key directory -- it must refuse"
else
  grep -q "AMK_DKIM_KEYS" "$WORK/empty.log" \
    && ok "refused, naming AMK_DKIM_KEYS" \
    || bad "refused but did not name the variable: $(head -1 "$WORK/empty.log")"
fi
if AMK_SMTP_SMARTHOST="not-a-host-port" AMK_BIND="$HTTP" timeout 20 "$AMKD" --role api >"$WORK/badsh.log" 2>&1; then
  bad "amkd started with a malformed AMK_SMTP_SMARTHOST -- it must refuse"
else
  grep -q "AMK_SMTP_SMARTHOST" "$WORK/badsh.log" \
    && ok "refused a malformed smarthost rather than falling back to direct-to-MX" \
    || bad "refused but did not name the variable: $(head -1 "$WORK/badsh.log")"
fi

# ---------------------------------------------------------------------------------------------
step "start the SMTP sink, then amkd --role api pointed at it"
python3 scripts/smtp-sink.py --port "$SINK" --outdir "$WORK/sink" \
  --cert "$WORK/sink.crt" --key "$WORK/sink.key" >"$WORK/sink.log" 2>&1 &
SINK_PID=$!
for _ in $(seq 1 40); do timeout 1 bash -c "(exec 3<>/dev/tcp/127.0.0.1/$SINK)" 2>/dev/null && break; sleep 0.25; done

AMK_DKIM_KEYS="$KEYS" AMK_SMTP_SMARTHOST="127.0.0.1:${SINK}" AMK_BIND="$HTTP" \
  "$AMKD" --role api >"$WORK/api.log" 2>&1 &
API_PID=$!
up=0
for _ in $(seq 1 60); do curl -fsS -o /dev/null -K "$CURLRC" "http://${HTTP}/v0/auth/me" 2>/dev/null && { up=1; break; }; sleep 0.25; done
[ "$up" = 1 ] || { bad "amkd --role api never became reachable"; sed -n '1,20p' "$WORK/api.log"; exit 1; }
ok "serving on $HTTP"
# The loader must announce the key WITHOUT printing it. Both halves are the assertion.
grep -q "DKIM keyring loaded" "$WORK/api.log" && ok "keyring load announced" || bad "no keyring-load line"
if grep -qE "BEGIN (RSA )?PRIVATE KEY|[A-Za-z0-9+/]{120,}" "$WORK/api.log"; then
  bad "the log appears to contain key material"
else
  ok "no key material in the log"
fi

# ---------------------------------------------------------------------------------------------
step "GATE 2 -- a send leaves the binary DKIM-signed, on the wire"
POD=$(api GET '/v0/pods' | python3 -c 'import json,sys; print(json.load(sys.stdin)["pods"][0]["pod_id"])')
INBOX=$(api POST "/v0/pods/$POD/inboxes" '{"username":"smoke"}' \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["inbox_id"])')
ok "inbox $INBOX"

SEND=$(api POST "/v0/inboxes/$(enc "$INBOX")/messages/send" \
  '{"to":["dest@elsewhere.test"],"subject":"binary smoke","text":"hello from the composed binary"}') \
  || {
    bad "POST .../messages/send failed -- this is the D1 regression"
    # The error envelope is the diagnosis and `curl -f` throws it away, so ask again without it.
    echo "  response: $(curl -sS -K "$CURLRC" -X POST \
      "http://${HTTP}/v0/inboxes/$(enc "$INBOX")/messages/send" -H 'Content-Type: application/json' \
      -d '{"to":["dest@elsewhere.test"],"subject":"binary smoke","text":"diagnostic retry"}' 2>&1 | head -c 400)"
    sed -n '1,20p' "$WORK/api.log"
  }

if [ -n "${SEND:-}" ]; then
  SENT_THREAD=$(printf '%s' "$SEND" | python3 -c 'import json,sys; print(json.load(sys.stdin)["thread_id"])')
  ok "send accepted, thread $SENT_THREAD"
  got=""
  for _ in $(seq 1 40); do
    got=$(ls "$WORK"/sink/*.eml 2>/dev/null | head -1); [ -n "$got" ] && break; sleep 0.25
  done
  if [ -z "$got" ]; then
    bad "nothing reached the smarthost -- the send was accepted but no bytes went out"
  else
    ok "the smarthost received $(basename "$got")"
    grep -qi '^DKIM-Signature:' "$got" \
      && ok "the wire bytes carry DKIM-Signature" \
      || bad "the message went out UNSIGNED -- exactly the D1 defect"
    grep -qi "d=${DOMAIN}" "$got" \
      && ok "signed for d=$DOMAIN" \
      || bad "DKIM-Signature does not name d=$DOMAIN"
    grep -qi "s=${SELECTOR}" "$got" \
      && ok "selector s=$SELECTOR" \
      || bad "DKIM-Signature does not carry the configured selector"
  fi
fi

# ---------------------------------------------------------------------------------------------
step "GATE 3 -- inbound SMTP lands, and lands with the RIGHT visibility"
# The first version of this gate asserted "a message is listed by the API" after injecting one,
# and PASSED -- on the outbound message Gate 2 had just sent into the same inbox. A false green,
# and exactly the class of defect this whole script exists to catch, so it is recorded here
# rather than quietly fixed: an assertion that does not identify WHICH row it found is not an
# assertion. Everything below keys on the injected Message-ID.
#
# Visibility is the real subject. Mail arriving with no SPF/DKIM pass is labelled
# `unauthenticated`, and restricted labels are EXCLUDED from list endpoints while remaining
# reachable by id (fixture 09b, CLAUDE.md contract facts). So the correct end-to-end assertion is
# a conjunction: reachable by id, and absent from the list. Asserting only the first would pass on
# a build that had lost the exclusion -- which is a data-disclosure bug, not a cosmetic one.
INBOUND_MID='<smoke-inbound-1@outside.test>'
AMK_BIND="$SMTPD" "$AMKD" --role smtpd >"$WORK/smtpd.log" 2>&1 &
SMTPD_PID=$!
for _ in $(seq 1 60); do timeout 1 bash -c "(exec 3<>/dev/tcp/${SMTPD/:/ })" 2>/dev/null && break; sleep 0.25; done

BEFORE=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')

INBOX="$INBOX" MID="$INBOUND_MID" python3 - "$SMTPD" <<'P' || bad "SMTP injection failed"
import os, smtplib, sys
host, port = sys.argv[1].split(":")
inbox = os.environ["INBOX"]
msg = (f"From: sender@outside.test\r\nTo: {inbox}\r\n"
       "Subject: inbound binary smoke\r\n"
       f"Message-ID: {os.environ['MID']}\r\n"
       # Required: amk-ingest answers 554 5.0.0 Missing Content-Type without it. A bare
       # smtplib.sendmail omits it, which is why the first run of this gate failed here.
       "Content-Type: text/plain; charset=utf-8\r\n"
       "MIME-Version: 1.0\r\n\r\ninbound body\r\n")
s = smtplib.SMTP(host, int(port), timeout=15)
s.sendmail("sender@outside.test", [inbox], msg)
s.quit()
print("  ok    injected over SMTP")
P

# `message_id` IS the angle-bracket Message-ID and must be percent-encoded in a path segment --
# `<`, `>` and `@` all (CLAUDE.md contract facts).
MID_ENC=$(enc "$INBOUND_MID")
body=""
for _ in $(seq 1 40); do
  body=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}" 2>/dev/null) && [ -n "$body" ] && break
  body=""; sleep 0.25
done
if [ -z "$body" ]; then
  bad "the injected message is not reachable by id -- inbound SMTP did not persist it"
  sed -n '1,20p' "$WORK/smtpd.log"
else
  ok "reachable by id"
  printf '%s' "$body" | python3 -c '
import json, sys
m = json.load(sys.stdin)
assert m.get("thread_id"), "no thread_id"
labels = m.get("labels") or []
print("  ok    thread_id " + m["thread_id"])
print("  ok    labels: " + ",".join(labels))
assert "sent" not in labels, "the inbound message is labelled sent -- this is the OUTBOUND row"
' || bad "the message reached by id is not the injected inbound one"
fi

AFTER=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')
LISTED=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages" \
  | python3 -c 'import json,sys; print(",".join(m["message_id"] for m in json.load(sys.stdin)["messages"]))')
case "$LISTED" in
  *"$INBOUND_MID"*)
    # Not automatically wrong -- it is right if the message authenticated. But nothing in this
    # harness can make SPF/DKIM pass for outside.test, so a listed row means the restricted-label
    # exclusion is not holding, which discloses hidden mail.
    bad "unauthenticated mail appears in the list endpoint (count $BEFORE -> $AFTER) -- restricted-label exclusion is not holding" ;;
  *)
    ok "excluded from the list endpoint while reachable by id (count stayed $BEFORE -> $AFTER)" ;;
esac

# ---------------------------------------------------------------------------------------------
step "GATE 4 -- the operational surface a cluster needs"
# k3s cannot health-check a server with no probe endpoint, and an incident cannot be answered by
# grepping unstructured stdout. These are unauthenticated by design (a probe that needs a
# credential is a probe the kubelet cannot use), so they are checked WITHOUT the curl config.

code=$(curl -sS -o "$WORK/health.out" -w '%{http_code}' "http://${HTTP}/health" 2>/dev/null)
[ "$code" = 200 ] && grep -q "^ok$" "$WORK/health.out" \
  && ok "/health 200 without a credential" \
  || bad "/health answered $code (body: $(head -c 60 "$WORK/health.out" 2>/dev/null))"

code=$(curl -sS -o "$WORK/ready.out" -w '%{http_code}' "http://${HTTP}/ready" 2>/dev/null)
[ "$code" = 200 ] && grep -q "^ready$" "$WORK/ready.out" \
  && ok "/ready 200 with the database up" \
  || bad "/ready answered $code with a live database"

code=$(curl -sS -o "$WORK/metrics.out" -w '%{http_code}' "http://${HTTP}/metrics" 2>/dev/null)
if [ "$code" != 200 ]; then
  bad "/metrics answered $code"
else
  ok "/metrics 200"
  # Every sample must carry HELP and TYPE, and every value must parse. A malformed exposition is
  # rejected by the scraper SILENTLY -- the dashboard simply stops updating, which is the failure
  # mode this assertion exists for.
  python3 - "$WORK/metrics.out" <<'P'
import sys
text = open(sys.argv[1]).read()
samples = [l for l in text.splitlines() if l and not l.startswith("#")]
assert samples, "no samples exported"
for line in samples:
    parts = line.split()
    assert len(parts) == 2, f"malformed sample: {line!r}"
    int(parts[1])
    assert f"# HELP {parts[0]} " in text, f"{parts[0]} has no HELP"
    assert f"# TYPE {parts[0]} counter" in text, f"{parts[0]} has no TYPE"
print(f"  ok    {len(samples)} counters, all with HELP and TYPE")
P
  # The request counter must have MOVED: Gate 2 drove real traffic through this server. A counter
  # stuck at zero means the middleware is mounted somewhere requests do not reach it.
  n=$(sed -n 's/^amk_http_requests_total //p' "$WORK/metrics.out")
  [ "${n:-0}" -gt 0 ] \
    && ok "amk_http_requests_total counted real traffic ($n)" \
    || bad "amk_http_requests_total is $n after Gate 2 drove requests -- the layer is not seeing them"
fi

# The request id is what correlates a caller's logs with ours; echoing it is the contract.
hdr=$(curl -sS -o /dev/null -D - -H 'x-request-id: smoke-correlation-1' "http://${HTTP}/health" 2>/dev/null | tr -d '\r' | sed -n 's/^[Xx]-[Rr]equest-[Ii]d: //p')
[ "$hdr" = "smoke-correlation-1" ] \
  && ok "x-request-id echoed verbatim" \
  || bad "x-request-id not echoed (got ${hdr:-none})"
gen=$(curl -sS -o /dev/null -D - "http://${HTTP}/health" 2>/dev/null | tr -d '\r' | sed -n 's/^[Xx]-[Rr]equest-[Ii]d: //p')
[ -n "$gen" ] && [ "$gen" != "smoke-correlation-1" ] \
  && ok "a request id is generated when none is supplied" \
  || bad "no request id generated for an unlabelled request"

# THE NEGATIVE CASE, and the one that matters. /health and /ready must diverge when the database
# dies: liveness stays up (restarting the pod does not fix Postgres -- it turns a degraded service
# into a crash-loop) while readiness fails, taking the instance out of rotation. Asserting only the
# happy path would pass on a /ready that returns 200 unconditionally, which is the same as having
# no readiness probe at all.
#
# Runs LAST, and destroys this run's throwaway database to do it -- nothing below needs it.
"$PSQL" "$MAINT" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
sleep 1
hc=$(curl -sS -o /dev/null -w '%{http_code}' "http://${HTTP}/health" 2>/dev/null)
rc=$(curl -sS -o /dev/null -w '%{http_code}' "http://${HTTP}/ready" 2>/dev/null)
[ "$hc" = 200 ] \
  && ok "/health stays 200 with the database gone (liveness must not crash-loop the pod)" \
  || bad "/health answered $hc with the database gone -- a failing liveness probe restarts the pod"
[ "$rc" = 503 ] \
  && ok "/ready falls to 503 with the database gone" \
  || bad "/ready answered $rc with the database gone -- it is not checking anything"
grep -q "readiness check failed" "$WORK/api.log" \
  && ok "the readiness failure is logged with its reason" \
  || bad "the readiness failure was not logged"
grep -q "amk-dev-local" "$WORK/api.log" \
  && bad "the log contains the database password" \
  || ok "the readiness failure did not leak the DSN"

# Structured, and never key material. The keyring line names selector and domain only.
if grep -q '"DKIM keyring loaded"' "$WORK/api.log" || grep -q "DKIM keyring loaded" "$WORK/api.log"; then
  ok "startup logged the keyring load"
else
  bad "no structured keyring-load event in the log"
fi

printf '\n'
if [ "$fail" -eq 0 ]; then echo "binary-smoke: PASS"; else echo "binary-smoke: FAIL"; fi
exit "$fail"
