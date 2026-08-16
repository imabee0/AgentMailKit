#!/usr/bin/env bash
# Scope derivation for .claude/contracts/amk-bins.md — the `amk` and `amkd` binaries.
# A reviewer re-runs this rather than reading the contract's list.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "== 1. what amk-http exposes for a server to mount =="
grep -n "^pub fn\|^pub async fn\|^pub struct\|^pub enum" crates/amk-http/src/lib.rs \
  | sed 's/^/  lib.rs:/'
echo "  --- AppState fields ---"
awk '/pub struct AppState/,/^}/' crates/amk-http/src/lib.rs | sed 's/^/  /'
echo "  --- config surface ---"
awk '/pub struct .*Config/,/^}/' crates/amk-http/src/config.rs | sed 's/^/  /'

echo
echo "== 2. what amk-store exposes for init and migrate =="
grep -n "^pub async fn connect\|^pub async fn connect_unmigrated\|migrate!" crates/amk-store/src/pool.rs \
  | sed 's/^/  pool.rs:/'
for f in organizations pods api_keys; do
  awk "/^pub async fn create\(/,/-> Result</" "crates/amk-store/src/$f.rs" \
    | sed "s|^|  $f.rs: |"
done

echo
echo "== 3. the New* structs those creates require =="
for f in organizations pods api_keys; do
  awk "/^pub struct New/,/^}/" "crates/amk-store/src/$f.rs" | sed "s|^|  $f.rs: |"
done

echo
echo "== 4. binaries that exist today =="
if find crates -name main.rs -print -quit | grep -q .; then
  find crates -name main.rs | sed 's/^/  /'
else
  echo "  (none — no main.rs anywhere)"
fi
grep -c '\[\[bin\]\]' crates/*/Cargo.toml 2>/dev/null | sed 's/^/  /' || true

echo
echo "== 5. what the plan's P0 line requires of them =="
grep -n "amk init\|amkd\|--role" ~/.claude/plans/download-agents-mail-sdk-drifting-frog.md \
  | head -8 | sed 's/^/  /'
