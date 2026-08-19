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
cd "$(dirname "$0")/.."

if [ "$#" -gt 0 ]; then changed=$(printf '%s\n' "$@"); else changed=$(cat); fi
changed=$(printf '%s\n' "$changed" | sed '/^[[:space:]]*$/d')

# Nothing changed -> nothing to test. Distinct from "everything changed".
[ -z "$changed" ] && exit 0

meta=$(cargo metadata --format-version 1 --no-deps 2>/dev/null) || {
  echo "ALL"   # cargo unavailable: cannot compute a closure, so do not pretend to have one.
  exit 0
}

printf '%s' "$meta" | CHANGED="$changed" python3 -c '
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

# A change to any of these invalidates the whole graph: the lockfile and workspace manifest move
# every dependency, the toolchain changes every lint verdict, and the CI definition changes what
# "tested" even means. The gate scripts are listed because they assert workspace-wide properties.
GLOBAL = (
    "Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "rustfmt.toml", "deny.toml",
    ".github/", "scripts/", "conformance/", "reference/", "Dockerfile",
)

seeds, widen = set(), False
for line in os.environ["CHANGED"].splitlines():
    line = line.strip()
    if not line:
        continue
    if line.startswith(GLOBAL) or line in GLOBAL:
        widen = True
        break
    if line.startswith("docs/") or line.startswith(".claude/") or line.startswith(".grok/") \
       or line in ("README.md", "CLAUDE.md", "AGENTS.md", ".gitignore"):
        continue                      # documentation and harness prose reach no crate
    owner = None
    for name, d in dirs.items():
        if line.startswith(d + "/") and (owner is None or len(d) > len(dirs[owner])):
            owner = name              # longest matching directory wins
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
'
