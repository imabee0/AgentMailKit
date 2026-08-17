#!/usr/bin/env bash
# Every extractor in amk-http that can REJECT before its handler runs — the complete class, not
# one member of it.
#
# WHY THIS EXISTS. `scripts/derive-p1-divergences.sh` asked "which typed PATH extractors can
# reject?", enumerated 17 `Path<Uuid>` sites, and the resulting contract closed all 17. It never
# asked about the body, and axum's `Json<T>` rejects exactly the same way: a plain-text body, a
# status the error catalog does not contain, and — for a deserialization failure rather than a
# syntax failure — serde's own message naming our internal Rust types. The P1 schemathesis run
# found it on the first fuzzed body: a 422 `text/plain` reading "data did not match any variant of
# untagged enum MetadataValue".
#
# The lesson the plan already records is that a contract's scope is derived, never recalled. This
# generalises it: the derivation must enumerate the CLASS, not the instance that prompted it.
# Every axum extractor with a `Rejection` type is a way out of the JSON error contract.
set -uo pipefail
cd "$(dirname "$0")/.."

H=crates/amk-http/src

echo "=== 1. request BODY extractors — the class the divergences contract never asked about ==="
echo "--- \$ grep -rn 'Json([a-z_]*): Json<' $H"
grep -rn 'Json([a-z_]*): Json<' "$H" || echo "  (none)"
echo "  -> each rejects with axum::extract::rejection::JsonRejection unless wrapped"

echo
echo "=== 2. QUERY extractors — same class, same escape ==="
echo "--- \$ grep -rn 'Query([a-z_]*): Query<' $H"
grep -rn 'Query([a-z_]*): Query<' "$H" || echo "  (none)"
echo "  -> each rejects with axum::extract::rejection::QueryRejection unless wrapped"

echo
echo "=== 3. PATH extractors — closed by the divergences contract; listed so the set stays whole ==="
echo "--- \$ grep -rn 'Path([a-z_]*): Path<\|PathPodId\|PathPodIdString' $H"
grep -rn 'Path([a-z_]*): Path<\|: PathPodId\b\|: PathPodIdString\b' "$H" || echo "  (none)"

echo
echo "=== 4. which of the above are already wrapped (Rejection = AppError) ==="
echo "--- \$ grep -rn 'type Rejection' $H"
grep -rn 'type Rejection' "$H" || echo "  (none)"

echo
echo "=== 5. anything else implementing FromRequest/FromRequestParts in this crate ==="
echo "--- \$ grep -rn 'impl FromRequest' $H"
grep -rn 'impl FromRequest' "$H" || echo "  (none)"

echo
echo "=== 6. the error catalog's statuses — what an extractor is allowed to produce ==="
echo "--- \$ grep -n 'StatusCode::\|=> 4[0-9][0-9]\|=> 5[0-9][0-9]' crates/amk-types/src/error.rs"
grep -n 'fn status' -A 40 crates/amk-types/src/error.rs | grep -oE '=> [0-9]{3}' | sort -u | tr '\n' ' '
echo
echo "  -> any status outside this set, emitted by an extractor, is a shape the API does not have"
