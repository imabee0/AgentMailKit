#!/bin/bash
# ITEM A10 case (g) — THREADING TIME-WINDOW BISECT  (LONG-TAIL, closure horizon T+30d)
#
# STATUS: ready-to-run, NOT launched. The orchestrator decides whether to start it.
# Non-blocking: it runs detached for ~30 days. Nothing else in P-1 waits on it.
#
# PURPOSE: hold subject + correspondent + (no References/In-Reply-To) CONSTANT and
# vary only ELAPSED TIME since the first message, to find whether AgentMail expires
# a thread — i.e. after how long an identical-subject/same-sender message starts a
# NEW thread_id instead of joining the original. This is the one A10 dimension the
# synchronous matrix cannot cover (it needs multi-day gaps).
#
# METHOD: send one message at each offset from T0, then ~60s later read its
# thread_id from the AgentMail API (matched by our Message-ID), and append to LOG.
# All messages share Subject "AMKwindowG <RUN>" and From window@probe.test.
#
# RUN (detached) from /home/imma/projects/AgentMailKit (holds the sdxd grant):
#   nohup ./reference/fixtures/16-threading-matrix/g-window-bisect.sh \
#       > reference/fixtures/16-threading-matrix/g-window-bisect.out 2>&1 &
#
# REQUIREMENTS: ssh BatchMode to root@144.217.66.212 (port 25 egress open there);
#   `sdxd run` grant present in CWD for kv/agentmail. Never echoes the API key.

set -u
OVH="root@144.217.66.212"
MX="inbound-smtp.us-east-1.amazonaws.com"
EHLO="server1.appsynergy.io"
RCPT="amk-probe@agentmail.to"
RUN="$(openssl rand -hex 4)"
SUBJECT="AMKwindowG ${RUN}"
HEADERFROM="Window Probe <window@probe.test>"
MAILFROM="window@probe.test"
DIR="$(cd "$(dirname "$0")" && pwd)"
LOG="${DIR}/g-window-bisect.log"
INGEST_WAIT=60

# offsets from T0, in seconds: 5m, 1h, 6h, 24h, 3d, 7d, 14d, 30d
OFFSETS=(300 3600 21600 86400 259200 604800 1209600 2592000)
LABELS=(T+5m T+1h T+6h T+24h T+3d T+7d T+14d T+30d)

T0=$(date +%s)
echo "# A10(g) window bisect  RUN=${RUN}  T0=$(date -u -d @${T0} +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$LOG"
echo "# subject=${SUBJECT}  from=${MAILFROM}  rcpt=${RCPT}  (no In-Reply-To/References)" | tee -a "$LOG"
echo "# label | offset_s | sent_utc | message_id | thread_id" | tee -a "$LOG"

send_one() {  # $1=label  $2=message_id
  local label="$1" mid="$2"
  ssh -o BatchMode=yes -o ConnectTimeout=15 "$OVH" \
    "MX='$MX' EHLO='$EHLO' MAILFROM='$MAILFROM' RCPT='$RCPT' \
     HFROM='$HEADERFROM' SUBJ='$SUBJECT' MID='$mid' python3 - <<'PY'
import os, smtplib
from email.utils import formatdate
L=['From: '+os.environ['HFROM'],'To: '+os.environ['RCPT'],
   'Subject: '+os.environ['SUBJ'],'Date: '+formatdate(localtime=True),
   'Message-ID: '+os.environ['MID'],'MIME-Version: 1.0',
   'Content-Type: text/plain; charset=utf-8','','A10(g) window bisect send']
s=smtplib.SMTP(os.environ['MX'],25,timeout=30); s.ehlo(os.environ['EHLO'])
s.mail(os.environ['MAILFROM']); s.rcpt(os.environ['RCPT'])
c,r=s.data('\r\n'.join(L)); s.quit()
print('SMTP',c,r.decode('utf-8','replace'))
PY"
}

thread_for() {  # $1=message_id  -> prints thread_id or NOT_FOUND
  local mid="$1"
  AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c '
    curl -s -H "Authorization: Bearer $AGENTMAIL_API_KEY" \
      "https://api.agentmail.to/v0/inboxes/amk-probe@agentmail.to/messages?limit=100"' \
  | MID="$mid" python3 -c '
import sys,os,json
mid=os.environ["MID"]
d=json.load(sys.stdin)
for m in d.get("messages",[]):
    if m.get("message_id")==mid:
        print(m.get("thread_id")); break
else:
    print("NOT_FOUND")'
}

for i in "${!OFFSETS[@]}"; do
  target=$(( T0 + ${OFFSETS[$i]} ))
  now=$(date +%s)
  gap=$(( target - now ))
  [ "$gap" -gt 0 ] && sleep "$gap"
  label="${LABELS[$i]}"
  mid="<g-${label//[+]/}-${RUN}@probe.test>"
  send_one "$label" "$mid"
  sleep "$INGEST_WAIT"
  tid="$(thread_for "$mid")"
  echo "${label} | ${OFFSETS[$i]} | $(date -u +%Y-%m-%dT%H:%M:%SZ) | ${mid} | ${tid}" | tee -a "$LOG"
done
echo "# A10(g) window bisect COMPLETE  RUN=${RUN}" | tee -a "$LOG"
