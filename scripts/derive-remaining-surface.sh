#!/usr/bin/env bash
# What is left to build, grouped, derived from openapi.json and the router.
#
# `derive-implemented-paths.sh` answers "how many operations are mounted" and reconciles them
# against the spec. This answers the complementary question the work queue needs -- WHICH ones are
# not, and which resource each belongs to -- so `docs/execute-plan-v1.md` can name work items
# without transcribing a number that goes stale the moment a handler lands.
#
# The same rule as everywhere else here: one record. A DAG that says "7 webhook endpoints" is a
# second copy of a fact openapi.json already holds, and the two disagree the day somebody adds one.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to the repository root" >&2; exit 1; }

python3 - "${1:-summary}" <<'PY'
import collections, json, re, sys

mode = sys.argv[1]
spec = json.load(open("reference/openapi.json"))
mounted = set(re.findall(r'\.route\(\s*"([^"]+)"', open("crates/amk-http/src/lib.rs").read()))

METHODS = ("get", "post", "put", "patch", "delete")

def group_of(path: str) -> str:
    # /v0/<group>/... , or the segment after a {placeholder} when the first is the mount point.
    segs = [s for s in path.split("/") if s and not s.startswith("{")]
    return segs[1] if len(segs) > 1 else (segs[0] if segs else "?")

total, done = collections.Counter(), collections.Counter()
missing = collections.defaultdict(list)
for path, ops in spec.get("paths", {}).items():
    for method in ops:
        if method.lower() not in METHODS:
            continue
        g = group_of(path)
        total[g] += 1
        if path in mounted:
            done[g] += 1
        else:
            missing[g].append(f"{method.upper():<6} {path}")

if mode == "missing":
    for g in sorted(missing, key=lambda k: (-len(missing[k]), k)):
        print(f"== {g} ({len(missing[g])} remaining) ==")
        for op in sorted(missing[g]):
            print(f"   {op}")
    raise SystemExit(0)

print(f"{'group':<18}{'mounted':>9}{'total':>7}{'remaining':>11}")
for g in sorted(total, key=lambda k: (-(total[k] - done[k]), k)):
    print(f"{g:<18}{done[g]:>9}{total[g]:>7}{total[g] - done[g]:>11}")
print(f"\n{'TOTAL':<18}{sum(done.values()):>9}{sum(total.values()):>7}{sum(total.values()) - sum(done.values()):>11}")
PY
