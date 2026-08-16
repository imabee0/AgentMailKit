#!/usr/bin/env bash
# Scope derivation for .claude/contracts/amk-bins.md — the `amk` and `amkd` binaries.
# A reviewer re-runs this rather than reading the contract's list.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "== 1. what amk-http exposes for a server to mount =="
# `^pub fn` alone misses constructors: an inherent `impl` indents its methods, so `AppState::new`
# — which `amk-cli` calls — was invisible to this section, and a review lens had to go read
# amk-http itself to confirm it was not an invented shape. Same blind spot section 2 had for
# `migration_status`, one section over and found later. Match indented `pub fn` too.
grep -n "^pub fn\|^pub async fn\|^pub struct\|^pub enum\|^    pub fn\|^    pub async fn" \
  crates/amk-http/src/lib.rs | sed 's/^/  lib.rs:/'
echo "  --- AppState fields ---"
awk '/pub struct AppState/,/^}/' crates/amk-http/src/lib.rs | sed 's/^/  /'
echo "  --- config surface ---"
awk '/pub struct .*Config/,/^}/' crates/amk-http/src/config.rs | sed 's/^/  /'

echo
echo "== 2. what amk-store exposes for init, migrate and doctor =="
# Enumerated, not recalled. The first version of this section grepped for `connect`,
# `connect_unmigrated` and `migrate!` by name, so it printed two comment lines out of the middle of
# `migration_status` and never its signature — while the contract instructed the implementer to
# CALL that function. A derivation that cannot show a function the contract names is the same
# defect as a hand-written scope, one layer down. Print the whole public surface of the three
# things the binaries touch and let the contract be checked against it.
echo "  --- crate re-exports (lib.rs) ---"
grep -n "^pub use\|^pub mod" crates/amk-store/src/lib.rs | sed 's/^/  lib.rs:/'
echo "  --- pool.rs public surface ---"
grep -n "^pub async fn\|^pub fn\|^pub struct\|^pub enum\|^    pub fn" crates/amk-store/src/pool.rs \
  | sed 's/^/  pool.rs:/'
echo "  --- every public fn in the modules init/doctor touch ---"
for f in organizations pods api_keys; do
  grep -n "^pub async fn\|^pub fn" "crates/amk-store/src/$f.rs" | sed "s|^|  $f.rs:|"
done

echo
echo "== 3. the New* structs those creates require =="
for f in organizations pods api_keys; do
  awk "/^pub struct New/,/^}/" "crates/amk-store/src/$f.rs" | sed "s|^|  $f.rs: |"
done

echo
echo "== 4. binaries that exist today =="
# Looked for `main.rs` only, which was right when the answer was "none" and wrong the moment one
# existed: cargo's `src/bin/<name>.rs` convention needs no `main.rs`, so the two binaries this
# section exists to track would have kept reporting as absent. Match both layouts.
if find crates \( -name main.rs -o -path '*/src/bin/*.rs' \) -print -quit | grep -q .; then
  find crates \( -name main.rs -o -path '*/src/bin/*.rs' \) | sort | sed 's/^/  /'
else
  echo "  (none — no main.rs and no src/bin/*.rs anywhere)"
fi
grep -c '\[\[bin\]\]' crates/*/Cargo.toml 2>/dev/null | sed 's/^/  /' || true

echo
echo "== 5. what the plan's P0 line requires of them =="
grep -n "amk init\|amkd\|--role" ~/.claude/plans/download-agents-mail-sdk-drifting-frog.md \
  | head -8 | sed 's/^/  /'
