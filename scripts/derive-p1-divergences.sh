#!/usr/bin/env bash
# Scope derivation for .claude/contracts/amk-p1-divergences.md.
#
# The four divergences come from an executed conformance run (fixture 25), not from reading. This
# script enumerates, from the code, every site each one touches — a reviewer re-runs it rather
# than reading the contract's list.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "== 1. the Organization fields amk-types now emits, and where a value must come from =="
awk '/pub struct Organization \{/,/^}/' crates/amk-types/src/pod.rs \
  | grep -oE '^\s+pub [a-z_]+' | awk '{print "  field: " $2}'
echo "  --- the only constructor outside amk-types ---"
grep -rn "Ok(Organization {" crates/amk-store/src/*.rs | sed 's|crates/amk-store/src/|  |'
echo "  --- the organizations table as it stands ---"
grep -vE '^\s*--|^\s*$' crates/amk-store/migrations/0001_organizations.sql | sed 's/^/  /'

echo
echo "== 2. every place the error envelope is built (the 'fix' field lands in all of them) =="
grep -rn "docs\b" crates/amk-types/src/error.rs | grep -vE '^\s*//' | sed 's|crates/amk-types/src/|  |' | head -20
echo "  --- and the http layer's rendering of it ---"
grep -rn "ErrorEnvelope\|fn status\|IntoResponse" crates/amk-http/src/error.rs \
  | sed 's|crates/amk-http/src/|  |'

echo
echo "== 3. every typed path extractor that can reject before reaching a handler =="
grep -rnE "Path<[^>]+>" crates/amk-http/src/handlers/*.rs crates/amk-http/src/*.rs \
  | sed 's|crates/amk-http/src/|  |'
echo "  --- the router's existing method/fallback handling, which this must match ---"
grep -n "fallback\|method_not_allowed\|not_found_fallback" crates/amk-http/src/lib.rs | sed 's/^/  /'

echo
echo "== 4. every place an ApiKey response is built (pod_id must appear on inbox-scoped keys) =="
grep -rn "ApiKey {" crates/amk-store/src/api_keys.rs | sed 's|crates/amk-store/src/|  |'
echo "  --- the api_keys scope columns and their CHECK ---"
grep -nE "pod_id|inbox_id|CHECK" crates/amk-store/migrations/0007_api_keys.sql | sed 's/^/  /'
echo "  --- the wire type's scope fields ---"
awk '/pub struct ApiKey \{/,/^}/' crates/amk-types/src/api_key.rs | grep -E 'pub |serde' | sed 's/^/  /'

echo
echo "== 5. tests that pin any behaviour these four changes alter =="
grep -rln "organizations\|Organization" crates/amk-store/tests/*.rs crates/amk-http/tests/*.rs 2>/dev/null | sed 's/^/  /'
grep -rn "not_found\|405\|fallback" crates/amk-http/tests/not_found.rs 2>/dev/null | head -8 | sed 's|crates/amk-http/tests/|  |'
