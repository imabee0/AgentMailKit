#!/usr/bin/env bash
# Shape-provenance gate. Runs in CI and from the Stop hook.
#
# Enforces the plan's hard rule: every wire type, storage model and identifier shape derives
# from AgentMail's artifacts — never from Stalwart or JMAP. Stalwart is sanctioned in exactly
# two roles: a migration SOURCE (amk-import, P6) and a vendor of standalone MIT crates consumed
# as libraries inside amk-ingest / amk-outbound.
#
# Three checks:
#   1. dependency direction — amk-types/core/store must not reach amk-import
#   2. naming              — no Stalwart/JMAP concepts in those three crates
#   3. boundary types      — no mail_parser::/mail_auth::/mail_send::/smtp_proto:: in their
#                            public signatures or re-exports (the likeliest accidental leak,
#                            because those types are ergonomic and right there)
#
# Structural leakage is what this catches. SEMANTIC leakage — a correctly-named field that
# behaves differently — is caught only by the dual-target conformance diff. Both are required.
set -uo pipefail
cd "$(dirname "$0")/.."

# amk-http joined on 2026-08-16, at its merge. The plan's boundary-type rule names the three
# shape-DEFINING crates, deliberately — but amk-http has no legitimate use for a stalwart-labs
# type either (those belong in amk-ingest/amk-outbound, P2), so extending the set closes a path
# rather than asserting a new rule. Added AFTER the merge, never mid-dispatch: changing the gate
# while a worktree is live gives two different verdicts for the same tree, which is the same
# hazard as editing a contract in place.
PROTECTED=(amk-types amk-core amk-store amk-http)
fail=0
note() { printf '  %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*"; fail=1; }

echo "== 1. dependency direction =="
if command -v cargo >/dev/null 2>&1; then
  meta=$(cargo metadata --format-version 1 --no-deps 2>/dev/null)
  for c in "${PROTECTED[@]}"; do
    deps=$(printf '%s' "$meta" | python3 -c "
import json,sys
m=json.load(sys.stdin)
for p in m['packages']:
    if p['name']=='$c':
        print(' '.join(sorted(d['name'] for d in p['dependencies'])))
" 2>/dev/null)
    case " $deps " in
      *" amk-import "*) bad "$c depends on amk-import (translation boundary must not invert)" ;;
      *) note "$c deps: ${deps:-<none>}" ;;
    esac
  done
else
  note "cargo not found; skipping"
fi

echo "== 2. naming (Stalwart/JMAP concepts) =="
# Case-insensitive SUBSTRING match, deliberately not \b-anchored: a word boundary would miss
# the concept embedded in a CamelCase identifier (`JmapMailboxRole`, `RocksDbKey`), which is
# exactly how such a shape would arrive. These tokens are distinctive enough that substring
# matching does not produce false positives. Comment lines are exempt: notes explaining why a
# shape differs FROM Stalwart are documentation we want to keep.
NAMES='(jmap|sieve|rocksdb|mailbox_?role)'
for c in "${PROTECTED[@]}"; do
  d="crates/$c/src"
  [ -d "$d" ] || { note "$c: no src yet"; continue; }
  hits=$(grep -rniE "$NAMES" "$d" | grep -vE '^\s*[^:]+:[0-9]+:\s*(//|///|//!|\*)' || true)
  if [ -n "$hits" ]; then
    bad "$c contains Stalwart/JMAP naming in code (comments contrasting with Stalwart are allowed):"
    printf '%s\n' "$hits" | sed 's/^/    /'
  else
    note "$c: clean"
  fi
done

echo "== 3. stalwart-labs crate types in public API =="
BOUNDARY='(mail_parser|mail_auth|mail_send|smtp_proto)::'
for c in "${PROTECTED[@]}"; do
  d="crates/$c/src"
  [ -d "$d" ] || continue
  # any `pub` item, `pub use`, or `impl` signature mentioning a boundary crate type
  hits=$(grep -rnE "(pub (fn|struct|enum|type|const|use|mod)|impl).*$BOUNDARY" "$d" || true)
  if [ -n "$hits" ]; then
    bad "$c exposes a stalwart-labs type in its public API (convert at the ingest/outbound boundary):"
    printf '%s\n' "$hits" | sed 's/^/    /'
  else
    note "$c: clean"
  fi
  # and they must not even be dependencies of these crates
  if [ -f "crates/$c/Cargo.toml" ] && grep -qE '^\s*(mail-parser|mail-auth|mail-send|smtp-proto)\b' "crates/$c/Cargo.toml"; then
    bad "$c depends on a stalwart-labs crate; those belong to amk-ingest/amk-outbound only"
  fi
done

# ---------------------------------------------------------------------------------------------
echo
echo "== 4. stalwart-labs types in the BOUNDARY crates' own public API =="
# Sections 1-3 keep these crates out of amk-types/core/store/http entirely. This section is the
# other half of the same rule, and it did not exist until amk-outbound was written: the boundary
# crates MAY depend on mail-send/mail-builder/mail-auth/mail-parser/smtp-proto — that is their
# sanctioned role — but they must CONVERT at their own edge, so none of those types may appear in
# a public signature or re-export there either.
#
# Added because `crates/amk-outbound/src/lib.rs` and its dispatch contract both asserted that this
# script checked exactly that, and it did not. A doc claiming a guarantee no check enforces is the
# prompt-versus-hook distinction the plan is built on, pointed the wrong way.
BOUNDARY_CRATES=(amk-outbound amk-ingest)
for c in "${BOUNDARY_CRATES[@]}"; do
  d="crates/$c/src"
  if [ ! -d "$d" ]; then
    note "$c: not written yet"
    continue
  fi
  hits=$(grep -rnE "(pub (fn|struct|enum|type|const|use|mod)|impl).*$BOUNDARY" "$d" || true)
  if [ -n "$hits" ]; then
    bad "$c leaks a stalwart-labs type through its own public API (convert at this crate's edge):"
    printf '%s\n' "$hits" | sed 's/^/    /'
  else
    note "$c: clean"
  fi
done

echo
if [ "$fail" -eq 0 ]; then
  echo "shape-provenance: PASS"
else
  echo "shape-provenance: FAIL"
fi
exit "$fail"
