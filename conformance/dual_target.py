#!/usr/bin/env python3
"""Dual-target conformance harness for AgentMailKit.

Issues each request in a manifest against TWO base URLs — the reference
(api.agentmail.to) and a candidate (our localhost server) — and structurally
diffs the responses: HTTP status, a selected set of headers, and the JSON body
SHAPE (key sets + value TYPES, recursively) — never the values, which legitimately
differ per resource. Prints a per-request PASS/DIFF report and exits non-zero if
any request diffs, so it can gate a phase in CI.

This is the real 1:1 check the plan relies on: naming lints catch structural
Stalwart leakage; only this diff catches SEMANTIC divergence (a correctly-named
field that behaves differently).

Usage:
  REF_BASE=https://api.agentmail.to  REF_KEY=<am_...> \
  CAND_BASE=http://localhost:8080     CAND_KEY=<amk_...> \
  python3 dual_target.py manifest.json [--only GET] [--self-test]

--self-test sets CAND_BASE=REF_BASE and CAND_KEY=REF_KEY: the reference API
diffed against itself must yield ZERO structural diffs. That validates the
comparator itself (used in P-1, before any local server exists).

Keys are read from the environment, never hard-coded, never printed.
"""
import json, os, sys, urllib.request, urllib.error, urllib.parse, argparse

SELECTED_HEADERS = ("content-type",)  # structural headers worth comparing; not date/request-id/etc.

def shape(v):
    """Reduce a JSON value to its structural signature: keys+types, not values."""
    if isinstance(v, dict):
        return {k: shape(v[k]) for k in sorted(v)}
    if isinstance(v, list):
        # element shapes, de-duplicated so [a,a,a] and [a] compare equal structurally
        seen = []
        for e in v:
            s = shape(e)
            if s not in seen:
                seen.append(s)
        return {"[]": seen}
    if v is None:
        return "null"       # nullable — reconciled leniently below
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, int):
        return "number"
    if isinstance(v, float):
        return "number"
    return "string"

def diff_shape(a, b, path="$"):
    """Yield human-readable structural differences between two shapes."""
    if a == b:
        return
    # null on one side only => nullable field; report as a soft note, not a hard diff
    if a == "null" or b == "null":
        yield f"  ~ {path}: nullable (ref={a} cand={b})"
        return
    if isinstance(a, dict) and isinstance(b, dict):
        if "[]" in a and "[]" in b:
            # compare the set of element shapes
            for i, es in enumerate(a["[]"]):
                if es not in b["[]"]:
                    yield f"  - {path}[]: element shape only in REF: {json.dumps(es)[:120]}"
            for i, es in enumerate(b["[]"]):
                if es not in a["[]"]:
                    yield f"  + {path}[]: element shape only in CAND: {json.dumps(es)[:120]}"
            return
        ak, bk = set(a), set(b)
        for k in sorted(ak - bk):
            yield f"  - {path}.{k}: present in REF, MISSING in CAND"
        for k in sorted(bk - ak):
            yield f"  + {path}.{k}: present in CAND, absent in REF (invented?)"
        for k in sorted(ak & bk):
            yield from diff_shape(a[k], b[k], f"{path}.{k}")
        return
    yield f"  ! {path}: type mismatch ref={a!r} cand={b!r}"

def call(base, key, method, path, body):
    url = base.rstrip("/") + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if key:
        req.add_header("Authorization", "Bearer " + key)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
            status, headers = r.status, dict((k.lower(), v) for k, v in r.headers.items())
    except urllib.error.HTTPError as e:
        raw = e.read()
        status, headers = e.code, dict((k.lower(), v) for k, v in e.headers.items())
    try:
        parsed = json.loads(raw) if raw else None
    except Exception:
        parsed = {"__nonjson__": raw[:80].decode("utf-8", "replace")}
    return status, headers, parsed

def dig(obj, dotted):
    """Follow a dotted path like 'pods.0.pod_id' through parsed JSON. None if absent."""
    cur = obj
    for part in dotted.split("."):
        if isinstance(cur, list):
            idx = int(part)
            if idx >= len(cur):
                return None
            cur = cur[idx]
        elif isinstance(cur, dict):
            if part not in cur:
                return None
            cur = cur[part]
        else:
            return None
    return cur

def resolve(base, key, spec, label):
    """Discover each placeholder's value ON THIS TARGET.

    The two sides of the diff hold different resources: the reference account's pod is not our
    pod, and `inbox_id` IS an email address, so it cannot even be guessed. Sending one target's
    ids to the other yields a 404-vs-200 diff that says nothing about conformance. So each
    placeholder is resolved per target by listing that target, and only the resulting SHAPES are
    compared — which is the whole point of the harness.

    Without this the manifest could only ever hold ids that exist on one side; the README said to
    "swap them by hand when running against localhost", which is not a gate anyone can re-run.

    Resolution is ORDERED and may depend on earlier placeholders: a `from` path is itself filled
    with what has been resolved so far. That is not a convenience — resolving `{pod_id}` and
    `{inbox_id}` independently picks the first pod and the first inbox, which need not be the same
    pod, and `GET /v0/pods/{pod_id}/inboxes/{inbox_id}` then 404s on one side and 200s on the
    other. The first run of this gate reported exactly that as an eight-field shape diff, and it
    was the harness's own doing.
    """
    out = {}
    for name, how in (spec or {}).items():
        src = fill(how["from"], out)
        if src is None:
            print(f"  ! cannot resolve {{{name}}} on {label}: its source path depends on an "
                  f"earlier placeholder that did not resolve", file=sys.stderr)
            out[name] = None
            continue
        st, _, body = call(base, key, "GET", src, None)
        val = dig(body, how["pick"]) if st == 200 else None
        if val is None:
            print(f"  ! cannot resolve {{{name}}} on {label}: GET {how['from']} -> {st}",
                  file=sys.stderr)
        out[name] = val
    return out

def fill(path, values):
    """Substitute {name} placeholders, percent-encoding each value for a path segment.

    `inbox_id` is an email address and `message_id` an angle-bracket Message-ID, so the '@', '<'
    and '>' must be encoded — the same rule the live API enforces on its own routes.
    """
    for name, val in values.items():
        token = "{" + name + "}"
        if token in path:
            if val is None:
                return None
            path = path.replace(token, urllib.parse.quote(str(val), safe=""))
    return path

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("manifest")
    ap.add_argument("--only", help="filter by HTTP method")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    ref_base = os.environ.get("REF_BASE", "https://api.agentmail.to")
    ref_key = os.environ.get("REF_KEY", "")
    cand_base = os.environ.get("CAND_BASE", "")
    cand_key = os.environ.get("CAND_KEY", "")
    if args.self_test:
        cand_base, cand_key = ref_base, ref_key
    if not cand_base:
        print("CAND_BASE unset and not --self-test; nothing to diff against.", file=sys.stderr)
        return 2

    manifest = json.load(open(args.manifest))
    reqs = [r for r in manifest["requests"] if not args.only or r["method"] == args.only]

    ref_ids = resolve(ref_base, ref_key, manifest.get("resolve"), "REF")
    cand_ids = resolve(cand_base, cand_key, manifest.get("resolve"), "CAND")
    if ref_ids or cand_ids:
        # Names only — an inbox_id IS an email address, so the values are not printed.
        print(f"resolved placeholders: {sorted(ref_ids)} "
              f"(ref: {sum(v is not None for v in ref_ids.values())}/{len(ref_ids)} found, "
              f"cand: {sum(v is not None for v in cand_ids.values())}/{len(cand_ids)} found)\n")

    diffs = 0
    skipped = 0
    for r in reqs:
        m, p = r["method"], r["path"]
        body = r.get("body")
        need = r.get("auth", True)
        rp, cp = fill(p, ref_ids), fill(p, cand_ids)
        if rp is None or cp is None:
            # Never silently drop a request: an unresolvable placeholder means the gate covered
            # less than the manifest claims, and a shrinking gate that still prints PASS is the
            # failure mode this project keeps finding in its own checks.
            side = "REF" if rp is None else "CAND"
            print(f"[SKIP] {m} {p} — placeholder unresolved on {side}")
            skipped += 1
            continue
        rs, rh, rb = call(ref_base, ref_key if need else "", m, rp, body)
        cs, ch, cb = call(cand_base, cand_key if need else "", m, cp, body)
        problems = []
        if rs != cs:
            problems.append(f"  ! status: ref={rs} cand={cs}")
        for h in SELECTED_HEADERS:
            rv = (rh.get(h) or "").split(";")[0].strip()
            cv = (ch.get(h) or "").split(";")[0].strip()
            if rv != cv:
                problems.append(f"  ! header {h}: ref={rv!r} cand={cv!r}")
        problems += list(diff_shape(shape(rb), shape(cb)))
        # a "~ nullable" soft note alone is not a failure
        hard = [x for x in problems if not x.strip().startswith("~")]
        tag = "PASS" if not hard else "DIFF"
        if hard:
            diffs += 1
        print(f"[{tag}] {m} {p}")
        for x in problems:
            print(x)
    print(f"\n{len(reqs)} requests, {len(reqs) - skipped} compared, "
          f"{skipped} skipped, {diffs} with structural diffs.")
    # A skip is a failure of the gate, not a neutral outcome — it means an endpoint this phase
    # implements went unchecked while the run still ended.
    return 1 if (diffs or skipped) else 0

if __name__ == "__main__":
    sys.exit(main())
