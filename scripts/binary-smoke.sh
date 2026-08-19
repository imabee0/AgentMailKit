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
  # This workstation publishes Postgres as docker `amk-dev-postgres` and has no host client.
  # `--network host` keeps the same 127.0.0.1:55432 DSN the rest of the gate uses.
  if command -v docker >/dev/null 2>&1 \
     && docker image inspect postgres:17-alpine >/dev/null 2>&1; then
    local w
    w=$(mktemp "${TMPDIR:-/tmp}/amk-psql.XXXXXX")
    cat >"$w" <<'WRAP'
#!/bin/bash
exec docker run --rm --network host --entrypoint psql postgres:17-alpine "$@"
WRAP
    chmod +x "$w"
    echo "$w"
    return 0
  fi
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
  for pid in ${API_PID:-} ${SMTPD_PID:-} ${SINK_PID:-} ${LORIS_PID:-} ${TLSD_PID:-}; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  "$PSQL" "$MAINT" -qc "DROP DATABASE IF EXISTS \"$DB\" WITH (FORCE)" >/dev/null 2>&1
  # The work directory holds a private key. Removing it is part of the test, not tidiness.
  rm -rf "$WORK"
  case "$PSQL" in "${TMPDIR:-/tmp}/amk-psql."*) rm -f "$PSQL" ;; esac
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

step "throwaway blob root and master key"
# Both are the operator-facing shape: a directory of bytes and a secret from the environment. The
# key is generated per run and never printed -- `openssl rand` rather than a literal, so a copy of
# this script cannot become a shared secret in someone's deployment.
BLOBS="$WORK/blobs"; mkdir -p "$BLOBS"; chmod 700 "$BLOBS"
MASTER_KEY=$(openssl rand -hex 32) || { echo "openssl rand failed"; exit 1; }
ok "blob root $BLOBS, 64-hex master key (not printed)"

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

# The blob configuration has the same shape of trap, and each of these three was a plausible
# silent-degradation: a blob root with no key mints nothing (a 500 on the first download rather
# than a refusal at boot), an unwritable or absent root drops raw bytes for every message received
# until someone notices, and a short key produces a signature anyone can forge while HMAC raises
# no objection at all.
refuses() { # $1=label  $2=variable the message must name  $3..=env assignments
  local label="$1" want="$2"; shift 2
  if env "$@" AMK_DKIM_KEYS="$KEYS" AMK_BIND="$HTTP" timeout 10 "$AMKD" --role api \
       >"$WORK/refuse.log" 2>&1; then
    bad "amkd started $label -- it must refuse"
  elif grep -q "$want" "$WORK/refuse.log"; then
    ok "refused $label, naming $want"
  else
    bad "did not refuse $label naming $want: $(head -1 "$WORK/refuse.log")"
  fi
}
refuses "with a blob root and no master key" AMK_MASTER_KEY "AMK_BLOB_ROOT=$BLOBS"
refuses "with a blob root that does not exist" AMK_BLOB_ROOT \
  "AMK_BLOB_ROOT=$WORK/no-such-dir" "AMK_MASTER_KEY=$MASTER_KEY"
refuses "with a forgeably short master key" AMK_MASTER_KEY \
  "AMK_BLOB_ROOT=$BLOBS" "AMK_MASTER_KEY=secret"

# ---------------------------------------------------------------------------------------------
step "start the SMTP sink, then amkd --role api pointed at it"
python3 scripts/smtp-sink.py --port "$SINK" --outdir "$WORK/sink" \
  --cert "$WORK/sink.crt" --key "$WORK/sink.key" >"$WORK/sink.log" 2>&1 &
SINK_PID=$!
for _ in $(seq 1 40); do timeout 1 bash -c "(exec 3<>/dev/tcp/127.0.0.1/$SINK)" 2>/dev/null && break; sleep 0.25; done

AMK_DKIM_KEYS="$KEYS" AMK_SMTP_SMARTHOST="127.0.0.1:${SINK}" AMK_BIND="$HTTP" \
  AMK_BLOB_ROOT="$BLOBS" AMK_MASTER_KEY="$MASTER_KEY" AMK_PUBLIC_BASE_URL="http://${HTTP}" \
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
AMK_BIND="$SMTPD" AMK_BLOB_ROOT="$BLOBS" AMK_MASTER_KEY="$MASTER_KEY" \
  "$AMKD" --role smtpd >"$WORK/smtpd.log" 2>&1 &
SMTPD_PID=$!
for _ in $(seq 1 60); do timeout 1 bash -c "(exec 3<>/dev/tcp/${SMTPD/:/ })" 2>/dev/null && break; sleep 0.25; done

BEFORE=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')

INBOX="$INBOX" MID="$INBOUND_MID" python3 - "$SMTPD" <<'P' || bad "SMTP injection failed"
import os, smtplib, sys
host, port = sys.argv[1].split(":")
inbox = os.environ["INBOX"]
# Multipart with a base64 attachment, so Gate 7 can assert the attachment pipeline end to end:
# the stored body must be the DECODED bytes, not the wire form. Gate 3 itself keys only on the
# Message-ID and labels, so the extra part changes nothing it asserts.
msg = (f"From: sender@outside.test\r\nTo: {inbox}\r\n"
       "Subject: inbound binary smoke\r\n"
       f"Message-ID: {os.environ['MID']}\r\n"
       # Required: amk-ingest answers 554 5.0.0 Missing Content-Type without it. A bare
       # smtplib.sendmail omits it, which is why the first run of this gate failed here.
       "Content-Type: multipart/mixed; boundary=smk\r\n"
       "MIME-Version: 1.0\r\n\r\n"
       "--smk\r\nContent-Type: text/plain; charset=utf-8\r\n\r\ninbound body\r\n"
       "--smk\r\nContent-Type: application/pdf\r\n"
       'Content-Disposition: attachment; filename="smoke.pdf"\r\n'
       "Content-Transfer-Encoding: base64\r\n\r\n"
       "JVBERi0xLjQgc21va2UgYXR0YWNobWVudA==\r\n"   # decodes to: %PDF-1.4 smoke attachment
       "--smk--\r\n")
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

# Structured, and never key material. The keyring line names selector and domain only.
if grep -q '"DKIM keyring loaded"' "$WORK/api.log" || grep -q "DKIM keyring loaded" "$WORK/api.log"; then
  ok "startup logged the keyring load"
else
  bad "no structured keyring-load event in the log"
fi

# ---------------------------------------------------------------------------------------------
step "GATE 5 -- smtpd survives a slow-loris"
# The accept loop used to spawn a task per connection with no cap, and serve_session has no
# deadline of its own: after the 250ms greet-pause, read_line awaits the next byte forever. A few
# hundred sockets trickling one byte a minute cost an attacker nothing and pinned a task each.
#
# Its own smtpd on its own port, with tiny limits, so the numbers are checkable in seconds rather
# than inferred from the production defaults (256 / 600s).
LORIS_BIND=127.0.0.1:8226
AMK_BIND="$LORIS_BIND" AMK_SMTP_MAX_CONNECTIONS=5 AMK_SMTP_SESSION_TIMEOUT=3 \
  "$AMKD" --role smtpd >"$WORK/loris.log" 2>&1 &
LORIS_PID=$!
for _ in $(seq 1 60); do timeout 1 bash -c "(exec 3<>/dev/tcp/${LORIS_BIND/:/ })" 2>/dev/null && break; sleep 0.25; done

if python3 - "$LORIS_BIND" <<'P'
import socket, sys, time
host, port = sys.argv[1].split(":"); port = int(port)
banners, held = [], []
for _ in range(12):
    try:
        s = socket.create_connection((host, port), timeout=5); s.settimeout(5)
        b = s.recv(256).decode(errors="replace").strip()
        banners.append(b.split()[0] if b else "(none)")
        held.append(s)                      # never speak again: this is the attack
    except Exception as e:
        banners.append(f"ERR:{type(e).__name__}")
served  = banners.count("220")
deferred = banners.count("421")
assert served == 5, f"cap not enforced: {served} served against a cap of 5 ({banners})"
# 421 rather than a dropped socket matters: RFC 5321 s3.8, and every real MTA retries on it, so
# mail over the cap is DEFERRED, not lost. A closed connection with no banner is indistinguishable
# from a broken server and some senders will bounce.
assert deferred == 7, f"expected 7 deferrals, got {deferred} ({banners})"
print("  ok    cap held: 5 served, 7 answered 421 (retryable, not dropped)")
time.sleep(5)                                # outlast the 3s deadline
s = socket.create_connection((host, port), timeout=5); s.settimeout(5)
assert s.recv(256).decode(errors="replace").startswith("220"), \
    "permits were never released -- the session deadline is not firing"
print("  ok    the deadline reclaimed the permits held by idle sessions")
P
then :; else bad "smtpd did not survive the slow-loris"; fi
grep -q "smtpd at capacity" "$WORK/loris.log" \
  && ok "capacity refusals are logged" || bad "capacity refusals were not logged"
grep -q "exceeded its deadline" "$WORK/loris.log" \
  && ok "deadline closures are logged" || bad "deadline closures were not logged"
kill "$LORIS_PID" 2>/dev/null; LORIS_PID=""

# ---------------------------------------------------------------------------------------------
step "GATE 5b -- the auth-failure path is rate limited"
# `amk-store::api_keys::authenticate` performs exactly one argon2id verify on EVERY path including
# misses -- the timing-oracle fix, and incidentally an expensive operation an unauthenticated
# caller gets to trigger at line rate. That is the CPU-exhaustion primitive the surcharge closes.
#
# Driven against the running binary because the first version of this limiter passed its unit
# tests and did nothing in production: the surcharge used `check`, which declines to deduct when
# the balance is short, so the charge silently stopped landing exactly when it mattered.
if python3 - <<'P'
import urllib.request, urllib.error
def hit(auth):
    r = urllib.request.Request("http://127.0.0.1:8123/v0/pods")
    r.add_header("Authorization", auth)
    try:
        return urllib.request.urlopen(r, timeout=5).status
    except urllib.error.HTTPError as e:
        return e.code
codes = [hit("Bearer am_smoke_wrong_key_000000000000") for _ in range(12)]
assert 429 in codes, f"auth failures were never throttled: {codes}"
first = codes.index(429)
assert first == 6, f"expected the 7th failure to throttle, got index {first}: {codes}"
print("  ok    six auth failures absorbed, the seventh throttled 429")
# A DIFFERENT credential must not be collateral damage -- many agents share one NAT address, so
# bucketing on the credential when one is presented is what keeps neighbours independent.
other = hit("Bearer am_smoke_other_key_1111111111")
assert other in (401, 403), f"a distinct credential was throttled by another's failures: {other}"
print("  ok    a distinct credential keeps its own bucket")
P
then :; else bad "the auth-failure rate limit did not behave"; fi

# And the valid key must be untouched by all of that.
api GET /v0/pods >/dev/null 2>&1 \
  && ok "the valid key is unaffected by another subject's throttling" \
  || bad "the valid key became collateral damage"
n=$(curl -sS "http://${HTTP}/metrics" 2>/dev/null | sed -n 's/^amk_throttled_total //p')
[ "${n:-0}" -ge 6 ] && ok "throttles are counted (amk_throttled_total=$n)" \
                    || bad "amk_throttled_total is ${n:-unset} after 6 throttled requests"

step "GATE 6 -- STARTTLS on inbound"
# Inbound mail used to be plaintext, unconditionally: `smtp.rs` answered 502 to STARTTLS and said
# so in its own doc. Opportunistic TLS is near-universal among senders, so every message from
# every peer crossed the wire in the clear.
#
# Opportunistic, not required -- a sender that does not offer STARTTLS is still accepted, because
# an MX that refuses plaintext refuses mail from everyone who has not implemented it, which is a
# delivery outage rather than a security posture.
TLS_BIND=127.0.0.1:8227
AMK_BIND="$TLS_BIND" AMK_SMTP_TLS_CERT="$WORK/sink.crt" AMK_SMTP_TLS_KEY="$WORK/sink.key" \
  "$AMKD" --role smtpd >"$WORK/tlsd.log" 2>&1 &
TLSD_PID=$!
for _ in $(seq 1 60); do timeout 1 bash -c "(exec 3<>/dev/tcp/${TLS_BIND/:/ })" 2>/dev/null && break; sleep 0.25; done

TLS_INBOX="tls-$$@${DOMAIN}"
api POST "/v0/pods/$POD/inboxes" "{\"username\":\"tls-$$\"}" >/dev/null 2>&1

if TLS_INBOX="$TLS_INBOX" python3 - "$TLS_BIND" <<'P'
import os, smtplib, ssl, sys
host, port = sys.argv[1].split(":"); port = int(port)
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
s = smtplib.SMTP(host, port, timeout=15)
s.ehlo("smoke.probe")
assert s.has_extn("starttls"), "STARTTLS not advertised with a certificate configured"
print("  ok    STARTTLS advertised in the clear")
s.starttls(context=ctx)
print("  ok    handshake completed")
s.ehlo("smoke.probe")
# RFC 3207 s4.2: the extension must NOT reappear once the channel is encrypted.
assert not s.has_extn("starttls"), "STARTTLS re-advertised after the upgrade (RFC 3207 s4.2)"
print("  ok    not re-advertised after the upgrade")
code, _ = s.docmd("STARTTLS")
assert 500 <= code < 600, f"a second STARTTLS must be refused, got {code}"
print(f"  ok    a second STARTTLS is refused ({code})")
inbox = os.environ["TLS_INBOX"]
s.sendmail("sender@outside.test", [inbox],
    f"From: sender@outside.test\r\nTo: {inbox}\r\nSubject: over tls\r\n"
    "Message-ID: <smoke-tls-1@outside.test>\r\n"
    "Content-Type: text/plain; charset=utf-8\r\nMIME-Version: 1.0\r\n\r\nencrypted body\r\n")
s.quit()
print("  ok    a message was delivered over the encrypted channel")
P
then :; else bad "STARTTLS did not work end to end"; fi

# It must have LANDED, not merely been accepted -- reachable by id, like Gate 3.
if api GET "/v0/inboxes/$(enc "$TLS_INBOX")/messages/$(enc '<smoke-tls-1@outside.test>')" >/dev/null 2>&1; then
  ok "the TLS-delivered message is stored and reachable by id"
else
  bad "the TLS-delivered message never reached storage"
  sed -n '1,15p' "$WORK/tlsd.log"
fi
kill "$TLSD_PID" 2>/dev/null; TLSD_PID=""

# And the negative side: with NO certificate configured, behaviour is exactly what it was before
# TLS existed. The main smtpd from Gate 3 is still running without one.
python3 - "$SMTPD" <<'P' || bad "the no-certificate path changed behaviour"
import smtplib, sys
host, port = sys.argv[1].split(":")
s = smtplib.SMTP(host, int(port), timeout=10)
s.ehlo("smoke.probe")
assert not s.has_extn("starttls"), "STARTTLS advertised with no certificate configured"
s.quit()
print("  ok    no certificate configured -> STARTTLS not advertised (unchanged)")
P

# ---------------------------------------------------------------------------------------------
step "GATE 7 -- raw MIME leaves by a signed URL, and ONLY by a signed URL"
# Two processes and a filesystem have to agree here: `amkd --role smtpd` wrote the original bytes
# under AMK_BLOB_ROOT during Gate 3, and `amkd --role api` -- a separate process, started from a
# separate environment -- has to find them, mint a token over them and serve them to a caller with
# no credential at all. Nothing in the unit suite crosses that boundary; both sides construct
# their own store.
#
# The positive half is easy to pass by accident, so the negatives carry the weight. This endpoint
# hands out mail to an UNAUTHENTICATED request, which makes the token the entire access control:
# if a tampered, absent, or replayed-onto-another-object token is ever honoured, every message in
# the deployment is readable by anyone who can guess a 64-hex id.

RAW=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}/raw") \
  || bad "GET .../raw failed for a message that get-by-id serves"

if [ -n "${RAW:-}" ]; then
  # Parsed into shell variables in one step, but the eval is guarded: `eval "$(cmd)"` succeeds
  # even when `cmd` fails, so the exit status has to be taken from the substitution itself before
  # anything is evaluated.
  if VARS=$(printf '%s' "$RAW" | python3 -c '
import json, re, sys, shlex
r = json.load(sys.stdin)
url = r["download_url"]
# The URL must be absolute and built from AMK_PUBLIC_BASE_URL, not from the bind address: a
# deployment behind a proxy hands out the proxy URL, and getting this wrong yields a link that
# only works from inside the cluster.
assert url.startswith("http://127.0.0.1:8123/v0/blobs/"), f"unexpected download_url {url}"
assert r["size"] > 0, "size is not positive"
# Timestamps are wire-exact: RFC 3339, exactly three fractional digits, Z (CLAUDE.md).
assert re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z", r["expires_at"]), r["expires_at"]
blob = url.split("/v0/blobs/", 1)[1].split("?", 1)[0]
assert re.fullmatch(r"[0-9a-f]{64}", blob), f"blob id is not 64 lowercase hex: {blob}"
print("RAW_URL=" + shlex.quote(url))
print("RAW_BLOB=" + shlex.quote(blob))
print("RAW_SIZE=" + shlex.quote(str(r["size"])))
'); then eval "$VARS"; else bad "the raw response is not the documented shape"; fi
fi

if [ -n "${RAW_URL:-}" ]; then
  ok "raw minted a signed URL over blob ${RAW_BLOB:0:12}… (${RAW_SIZE} bytes, expiring)"

  # --- the positive: no credential, and the ORIGINAL bytes come back --------------------------
  code=$(curl -sS -o "$WORK/raw.eml" -D "$WORK/raw.hdr" -w '%{http_code}' "$RAW_URL" 2>/dev/null)
  if [ "$code" != 200 ]; then
    bad "the signed URL answered $code without a credential"
  else
    ok "200 with no Authorization header -- the token is the authorisation"
    # Keyed on the injected Message-ID, for the reason recorded above Gate 3: an assertion that
    # does not identify WHICH object it got is not an assertion.
    grep -qF "$INBOUND_MID" "$WORK/raw.eml" \
      && ok "the bytes are the injected message, not some other row's raw" \
      || bad "the served blob does not contain $INBOUND_MID"
    served=$(wc -c <"$WORK/raw.eml" | tr -d ' ')
    [ "$served" = "$RAW_SIZE" ] \
      && ok "byte count matches the advertised size" \
      || bad "served $served bytes, advertised $RAW_SIZE"
    grep -qi '^content-type: *application/octet-stream' "$WORK/raw.hdr" \
      && ok "served as application/octet-stream (a browser will not render it)" \
      || bad "wrong content type: $(grep -i '^content-type' "$WORK/raw.hdr" | head -1)"
    # The URL IS a bearer token. A shared proxy caching it would hand the message to the next
    # caller who asks for the same path.
    grep -qi '^cache-control: *private, *no-store' "$WORK/raw.hdr" \
      && ok "Cache-Control: private, no-store" \
      || bad "missing no-store: $(grep -i '^cache-control' "$WORK/raw.hdr" | head -1)"
  fi

  # --- content addressing, on disk ------------------------------------------------------------
  # The id is not a name the server chose; it is the SHA-256 of the bytes. Recomputing it here is
  # what makes "content-addressed" a checked property rather than a comment in the source.
  sum=$(sha256sum "$WORK/raw.eml" 2>/dev/null | cut -d' ' -f1)
  [ "$sum" = "$RAW_BLOB" ] \
    && ok "blob id is the SHA-256 of the served bytes" \
    || bad "blob id $RAW_BLOB is not the digest of what was served ($sum)"
  shard="$BLOBS/${RAW_BLOB:0:2}/${RAW_BLOB:2:2}/$RAW_BLOB"
  [ -f "$shard" ] \
    && ok "on disk, sharded: ${RAW_BLOB:0:2}/${RAW_BLOB:2:2}/…" \
    || bad "no object at $shard -- smtpd and the api role disagree about the blob root"

fi

# --- the stored raw is the SIGNED raw ----------------------------------------------------------
# Gate 2 proved the bytes on the wire carry DKIM-Signature. This proves the bytes we KEPT are the
# same ones -- if the raw were captured before signing, a recipient's complaint could never be
# reconciled against what we can show we sent.
if [ -n "${SEND:-}" ]; then
  SENT_MID=$(printf '%s' "$SEND" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message_id"])')
  surl=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages/$(enc "$SENT_MID")/raw" \
         | python3 -c 'import json,sys; print(json.load(sys.stdin)["download_url"])' 2>/dev/null)
  if [ -n "$surl" ] && curl -fsS "$surl" -o "$WORK/sent.eml" 2>/dev/null; then
    grep -qi '^DKIM-Signature:' "$WORK/sent.eml" \
      && ok "the retained raw of a sent message carries its DKIM-Signature" \
      || bad "the stored raw is unsigned -- it was captured before signing"
    # Kept for the replay negative below: a SECOND blob that genuinely exists on disk.
    OTHER_BLOB=${surl#*/v0/blobs/}; OTHER_BLOB=${OTHER_BLOB%%\?*}
  else
    bad "the raw of a message this binary sent is not retrievable"
  fi
fi


if [ -n "${RAW_URL:-}" ]; then
  # The MINTING endpoint stays authenticated -- only the fetch is credential-free. Asserted HERE,
  # before the refusals below: each of those is charged the auth-failure surcharge, and once the
  # anonymous bucket is in debt this answers 429 -- still a refusal, but not the one under test.
  c=$(curl -sS -o /dev/null -w '%{http_code}' \
        "http://${HTTP}/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}/raw" 2>/dev/null)
  case "$c" in
    401|403) ok "minting a URL still requires a credential ($c)" ;;
    *)       bad "GET .../raw answered $c with no credential -- anyone can mint download URLs" ;;
  esac

  # --- the negatives, each an independent way in ----------------------------------------------
  # Each refusal below is a 403 and therefore carries the 20x auth-failure surcharge, and five of
  # them exhaust the anonymous bucket for this address -- so the sixth would read 429 and tell us
  # nothing about the sixth way in. Giving each probe its own Bearer puts it on its own bucket.
  # The header is IGNORED by this endpoint (the query-string token is the entire authorisation),
  # so the code path under test is identical; only which bucket pays for it differs. The
  # anonymous path gets its own assertion immediately after.
  DENY_N=0
  deny() { # $1=label  $2=url
    DENY_N=$((DENY_N + 1))
    local c; c=$(curl -sS -o /dev/null -w '%{http_code}' \
                   -H "authorization: Bearer smoke-bucket-$DENY_N" "$2" 2>/dev/null)
    [ "$c" = 403 ] && ok "$1 -> 403" || bad "$1 -> $c (must be an indistinguishable 403)"
  }
  TOKEN=${RAW_URL#*\?token=}
  BASE=${RAW_URL%\?*}
  # Flip the last character of the MAC. Every other byte of the request is untouched, so a pass
  # here means the signature is not being checked at all.
  last=${TOKEN: -1}; [ "$last" = "A" ] && flip=B || flip=A
  deny "a token with one character changed" "${BASE}?token=${TOKEN%?}${flip}"
  deny "no token at all"                    "${BASE}"
  deny "an empty token"                     "${BASE}?token="
  # The MAC covers the blob id, so a token minted for one object must not open another. Without
  # that binding, one legitimate download URL is a key to the whole store.
  # The replay target must be a blob that REALLY EXISTS. Falsifying this gate against a build
  # with `download::verify` stubbed out showed why: pointed at an invented id, this assertion
  # still passed -- on the 403 the missing object produces, not on the signature. An assertion
  # that passes for the wrong reason is the same false green the Gate 3 note records.
  if [ -n "${OTHER_BLOB:-}" ] && [ "$OTHER_BLOB" != "$RAW_BLOB" ]; then
    deny "a valid token replayed against a different blob that exists" \
         "http://${HTTP}/v0/blobs/${OTHER_BLOB}?token=${TOKEN}"
  else
    bad "no second blob to replay against -- the binding assertion did not run"
  fi
  deny "a token for a blob that does not exist" \
       "http://${HTTP}/v0/blobs/$(printf 'absent' | sha256sum | cut -d' ' -f1)?token=${TOKEN}"
  # Path traversal cannot survive BlobId::parse, but the assertion belongs where an attacker would
  # aim rather than only in the unit test for the parser.
  deny "a traversal in the blob id" "http://${HTTP}/v0/blobs/..%2f..%2fetc%2fpasswd?token=${TOKEN}"

fi

  # --- what those six refusals did to the bucket, and what they must NOT have done -------------
  # Discovered here, not designed: the six refusals above each carry the 20x auth-failure
  # surcharge, so by this line the anonymous bucket for 127.0.0.1 is in debt. That is correct --
  # forging download tokens IS credential guessing and should get expensive. What was NOT correct
  # is what happened next on the first run of this gate: `/health` answered 429, which in a
  # cluster is a liveness failure and a pod restart. A burst of bad tokens must never be able to
  # restart the API server.
  throttled_at=""
  for n in $(seq 1 12); do
    c=$(curl -sS -o /dev/null -w '%{http_code}' "http://${HTTP}/v0/blobs/${RAW_BLOB}?token=" 2>/dev/null)
    if [ "$c" = 429 ]; then throttled_at=$n; break; fi
    [ "$c" = 403 ] || bad "an anonymous forged token answered $c, expected 403 or 429"
  done
  [ -n "$throttled_at" ] \
    && ok "forging tokens anonymously throttles the caller (429 at attempt $throttled_at)" \
    || bad "12 forged tokens from one address were never throttled -- guessing is free"
  for probe in /health /ready /metrics; do
    c=$(curl -sS -o /dev/null -w '%{http_code}' "http://${HTTP}${probe}" 2>/dev/null)
    [ "$c" = 200 ] \
      && ok "$probe answers 200 while application traffic is throttled" \
      || bad "$probe answered $c -- infrastructure probes share a bucket with application traffic"
  done

# --- the attachment rides the same pipeline ----------------------------------------------------
# Gate 3's injected message carried one attachment; this is `get-attachment` end to end across
# the same process boundary as the raw form above: smtpd decoded and stored the body, and the api
# role has to describe it, sign for it, and serve it to a caller with no credential.
ATT=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}" \
      | python3 -c 'import json,sys; a=(json.load(sys.stdin).get("attachments") or []); print(a[0]["attachment_id"] if a else "")')
if [ -z "$ATT" ]; then
  bad "the injected message lists no attachments -- ingest dropped the metadata"
else
  ok "message describes its attachment ($ATT)"
  ARESP=$(api GET "/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}/attachments/${ATT}") \
    || bad "GET .../attachments/{id} failed for a described attachment"
  if [ -n "${ARESP:-}" ]; then
    AURL=$(printf '%s' "$ARESP" | python3 -c '
import json, sys
r = json.load(sys.stdin)
assert r["filename"] == "smoke.pdf", r
assert r["content_type"] == "application/pdf", r
# The DECODED length -- 25 bytes -- not the 36 the base64 wire form occupies. Getting the wire
# length here means the store kept the encoded body, and every downloaded file would be garbage.
assert r["size"] == 25, ("size is not the DECODED length (25)", r["size"])
print(r["download_url"])
') || { bad "the attachment response is not the documented shape"; AURL=""; }
    if [ -n "$AURL" ]; then
      # The anonymous IP bucket is already in debt from the forged-token loop above.
      # A bare GET would 429 and this assertion would read as "empty body". The blob
      # endpoint still authorises on the query token; the Bearer only picks a fresh bucket.
      acode=$(curl -sS -o "$WORK/att.bin" -w '%{http_code}' \
                -H "authorization: Bearer smoke-bucket-att-dl" "$AURL" 2>/dev/null)
      got=$(cat "$WORK/att.bin" 2>/dev/null || true)
      if [ "$acode" = 200 ] && [ "$got" = "%PDF-1.4 smoke attachment" ]; then
        ok "the download returns the DECODED attachment body, credential-free"
      else
        bad "downloaded body is wrong: http $acode '$got'"
      fi
    fi
    # And the miss: an invented id on the same message is the flat 404.
    c=$(curl -sS -o /dev/null -w '%{http_code}' -K "$CURLRC" \
          "http://${HTTP}/v0/inboxes/$(enc "$INBOX")/messages/${MID_ENC}/attachments/00000000000000000000000000000000" 2>/dev/null)
    [ "$c" = 404 ] \
      && ok "an invented attachment id is a 404, not an oracle" \
      || bad "an invented attachment id answered $c"
  fi
fi

# --- and the honest degradation ----------------------------------------------------------------
# Gate 6's TLS smtpd ran with NO blob root, which is a supported deployment. Its message must give
# a 404 rather than a 500 or an empty 200: absent raw degrades to not-found, never to a lie.
if [ -n "${TLS_INBOX:-}" ]; then
  c=$(curl -sS -o /dev/null -w '%{http_code}' -K "$CURLRC" \
        "http://${HTTP}/v0/inboxes/$(enc "$TLS_INBOX")/messages/$(enc '<smoke-tls-1@outside.test>')/raw" 2>/dev/null)
  [ "$c" = 404 ] \
    && ok "a message stored with no blob root gives 404 on /raw, not a broken link" \
    || bad "/raw answered $c for a message that has no raw"
fi

# ---------------------------------------------------------------------------------------------
step "GATE 8 -- /health and /ready diverge when the database dies"
# LAST, and destructive: it drops this run's database, so nothing after it can use one. That
# ordering is the bug this gate already caught once -- it used to sit inside Gate 4 and left the
# smtpd in Gate 5 with no database to connect to.
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


printf '\n'
if [ "$fail" -eq 0 ]; then echo "binary-smoke: PASS"; else echo "binary-smoke: FAIL"; fi
exit "$fail"
