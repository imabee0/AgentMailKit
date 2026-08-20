#!/usr/bin/env bash
# Which workspace crates does a change actually reach?
#
# Prints `-p <crate>` flags for `cargo test`/`cargo clippy`, one per line, or the single token
# ALL when every crate is affected. Reads changed paths from argv, or from stdin when given none.
#
#   ./scripts/affected-crates.sh $(git diff --name-only origin/main...HEAD)
#   git diff --name-only origin/main...HEAD | ./scripts/affected-crates.sh
#
# WHY THIS IS NOT PATH FILTERING
#
# The obvious CI shortcut -- "file under crates/amk-ingest changed, so test amk-ingest" -- is
# wrong in this workspace and would produce a confidently green build on a broken tree. The crates
# form a dependency chain (types -> core -> store -> http -> ingest/outbound -> cli), so a change
# to `amk-types` reaches all seven. Selecting by directory would test one and report success.
#
# So the mapping is computed, not declared: `cargo metadata` gives the real graph, this script
# walks it in REVERSE from each changed crate, and the answer is the closure. There is no list to
# keep in sync, which is the point -- a hand-maintained affected-map is a second record of the
# dependency graph, and `docs/PLAN.md`'s ledger rule says the second record is the one that
# silently disagrees.
#
# FAIL OPEN, NOT CLOSED. Every unrecognised path widens the selection to ALL. A path this script
# has not been taught about is a path whose blast radius is unknown, and the safe answer to
# "unknown" in a gate is "run everything" -- never "run nothing".
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

if [ "$#" -gt 0 ]; then changed=$(printf '%s\n' "$@"); else changed=$(cat); fi
changed=$(printf '%s\n' "$changed" | sed '/^[[:space:]]*$/d')

# Nothing changed -> nothing to test. Distinct from "everything changed".
[ -z "$changed" ] && exit 0

meta=$(cargo metadata --format-version 1 --no-deps 2>/dev/null) || {
  echo "ALL"   # cargo unavailable: cannot compute a closure, so do not pretend to have one.
  exit 0
}

# Paths go through a file, not argv and not the environment: a PR that untracks a venv
# (thousands of paths) blew ARG_MAX via CHANGED=... python3 (`Argument list too long`).
changed_file=$(mktemp)
printf '%s\n' "$changed" >"$changed_file"
printf '%s' "$meta" | python3 -c '
import json, os, sys, pathlib

meta = json.load(sys.stdin)
root = pathlib.Path(meta["workspace_root"])
names = {p["name"] for p in meta["packages"]}

# crate name -> its directory, relative to the workspace root
dirs = {}
for p in meta["packages"]:
    dirs[p["name"]] = str(pathlib.Path(p["manifest_path"]).parent.relative_to(root))

# reverse edges: dependency -> dependents (workspace members only; externals are pinned and
# arrive through Cargo.lock, which is handled by the global-trigger list below)
rdeps = {n: set() for n in names}
for p in meta["packages"]:
    for d in p["dependencies"]:
        if d["name"] in names:
            rdeps[d["name"]].add(p["name"])

# There is deliberately NO list of "global" paths here.
#
# One was written first -- Cargo.lock, the workspace manifest, the toolchain pin, .github/,
# scripts/, reference/ -- and a mutation run proved it was dead code: deleting the whole list
# changed no test result, because every path on it lives outside a crate directory and therefore
# already widens through the unrecognised-path branch below. Two mechanisms producing one
# behaviour is the duplicate-record failure docs/PLAN.md line 516 exists to prevent: the two
# disagree eventually and the quiet one wins. The fail-open branch is the single record.
#
# What keeps it honest is the test suite, not a list: affected-crates.test.sh asserts that
# Cargo.lock, the toolchain pin, a gate script, a workflow, a fixture and the Dockerfile each
# widen to ALL. Adding an over-broad exemption below breaks those tests.

seeds, widen = set(), False
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    if line.startswith("docs/") or line.startswith(".claude/") or line.startswith(".grok/") \
       or line in ("README.md", "CLAUDE.md", "AGENTS.md", ".gitignore"):
        continue                      # documentation and harness prose reach no crate
    owner = None
    for name, d in dirs.items():
        # Longest matching directory wins. UNEXERCISED TODAY and deliberately kept: no workspace
        # member nests inside another, so the tiebreak never fires and a mutation run confirmed
        # that removing it kills no test. It fires the moment a nested member exists (say
        # crates/amk-http/bench), where parent and child both prefix-match and the shorter match
        # would attribute the nested files to the parent. Add a case to affected-crates.test.sh
        # on the commit that introduces such a member.
        if line.startswith(d + "/") and (owner is None or len(d) > len(dirs[owner])):
            owner = name
    if owner is None:
        widen = True                  # unrecognised path: unknown blast radius
        break
    seeds.add(owner)

if widen:
    print("ALL"); sys.exit(0)
if not seeds:
    sys.exit(0)                       # docs-only: no crate needs rebuilding

# transitive closure over reverse edges
affected, stack = set(seeds), list(seeds)
while stack:
    cur = stack.pop()
    for dependent in rdeps.get(cur, ()):
        if dependent not in affected:
            affected.add(dependent)
            stack.append(dependent)

if affected == names:
    print("ALL"); sys.exit(0)
for n in sorted(affected):
    print(f"-p {n}")
' "$changed_file"
rm -f "$changed_file"
