#!/usr/bin/env python3
"""Which operations does amk-http actually mount, and does openapi.json describe exactly those?

P1's gate says "schemathesis over implemented paths", and the whole question is which paths those
are. A hand-written list is the shape this project has been bitten by repeatedly: right when
written, silently wrong the moment a route moves. So the set is DERIVED from `router()` on every
run, reconciled against the spec, and the run fails if the two disagree — an operation we mount
that the spec does not describe cannot be schema-fuzzed at all, and a method the spec describes on
a mounted path but we do not serve would be fuzzed against a route that answers 404 by design.

One module owns the parse. `scripts/derive-implemented-paths.sh` prints it for contract scoping and
`scripts/p1-gate.sh` consumes `--include-args` to build the schemathesis command line; neither
keeps its own copy of the list.

  python3 schemathesis_scope.py --list           # human-readable enumeration + reconciliation
  python3 schemathesis_scope.py --include-args   # `--include-path <p>` args, one per line
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROUTER = ROOT / "crates" / "amk-http" / "src" / "lib.rs"
SPEC = ROOT / "reference" / "openapi.json"

VERBS = ("get", "post", "patch", "put", "delete")


def mounted():
    """Enumerate (METHOD, path) from the `.route(...)` calls in `router()`.

    The method chain may wrap across lines (`get(..).patch(..).delete(..)` is formatted onto three),
    so the call is closed by counting parens rather than by scanning to end-of-line.
    """
    src = ROUTER.read_text()
    body = src.split("pub fn router(", 1)[1].split(".fallback(", 1)[0]
    ops = []
    for m in re.finditer(r'\.route\(\s*"([^"]+)"\s*,', body):
        path, i, depth = m.group(1), m.end(), 1
        while depth:
            depth += {"(": 1, ")": -1}.get(body[i], 0)
            i += 1
        for verb in re.findall(r"\b(%s)\s*\(" % "|".join(VERBS), body[m.end():i]):
            ops.append((verb.upper(), path))
    return sorted(set(ops), key=lambda o: (o[1], o[0]))


def spec_ops(spec):
    return {(m.upper(), p) for p, e in spec["paths"].items() for m in e if m in VERBS}


def reconcile(ops, spec):
    """Return (unspecified, unmounted_on_mounted_paths). Either being non-empty fails the gate."""
    described = spec_ops(spec)
    unspecified = [o for o in ops if o not in described]
    paths = {p for _, p in ops}
    unmounted = sorted(o for o in described if o[1] in paths and o not in ops)
    return unspecified, unmounted


def main():
    ops = mounted()
    spec = json.loads(SPEC.read_text())
    unspecified, unmounted = reconcile(ops, spec)

    if "--include-args" in sys.argv:
        # Paths only: the reconciliation above has already proven method sets agree per path, so a
        # path filter cannot pull in an operation we do not serve.
        for path in sorted({p for _, p in ops}):
            print("--include-path")
            print(path)
        return 1 if (unspecified or unmounted) else 0

    print("=== 1. operations mounted by amk-http::router(), read from the source ===")
    print(f"--- derived from {ROUTER.relative_to(ROOT)} by parsing every .route() call")
    for verb, path in ops:
        op = spec["paths"].get(path, {}).get(verb.lower(), {})
        print(f"  {verb:7} {path:45} operationId={op.get('operationId', '<not in spec>')}")
    print(f"  -> {len(ops)} operations on {len({p for _, p in ops})} path templates")

    print()
    print("=== 2. reconciliation against reference/openapi.json ===")
    if unspecified:
        print("  MOUNTED BUT NOT DESCRIBED BY THE SPEC — cannot be schema-fuzzed:")
        for verb, path in unspecified:
            print(f"    {verb:7} {path}")
    if unmounted:
        print("  DESCRIBED ON A MOUNTED PATH BUT NOT SERVED — a path filter would fuzz it:")
        for verb, path in unmounted:
            print(f"    {verb:7} {path}")
    if not unspecified and not unmounted:
        print(f"  clean: all {len(ops)} mounted operations are described, and the spec describes")
        print("  no additional method on any mounted path — so filtering by PATH alone selects")
        print("  exactly the implemented set, with no forked copy of the schema to drift.")
    total = len(spec_ops(spec))
    print(f"  spec describes {total} operations; this phase mounts {len(ops)}"
          f" ({total - len(ops)} out of scope for P1)")

    print()
    print("=== 3. the path filter this produces ===")
    for path in sorted({p for _, p in ops}):
        print(f"  --include-path {path}")
    return 1 if (unspecified or unmounted) else 0


if __name__ == "__main__":
    sys.exit(main())
