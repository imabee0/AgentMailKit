# Conformance harness

`dual_target.py` issues each request in `manifest.json` against a **reference**
base URL (`api.agentmail.to`) and a **candidate** (our server), then structurally
diffs status + selected headers + JSON body **shape** (keys and value types,
recursively — never values). Exit non-zero on any structural diff, so it gates a
phase in CI.

Why it matters: naming lints and dependency-direction checks catch *structural*
Stalwart leakage; only this diff catches *semantic* leakage — a correctly-named
field that behaves differently. Every phase gate P1–P5 requires this clean for
the endpoints that phase implements.

## Run

```
# self-test: reference vs itself must be all PASS (validates the comparator)
REF_BASE=https://api.agentmail.to REF_KEY=<am_...> \
  python3 dual_target.py manifest.json --self-test

# real run: candidate = local server
REF_BASE=https://api.agentmail.to REF_KEY=<am_...> \
CAND_BASE=http://localhost:8080   CAND_KEY=<amk_...> \
  python3 dual_target.py manifest.json
```

Keys come from the environment (via `sdxd run`), never hard-coded, never printed.

## Status
- Comparator **validated** 2026-08-15: `--self-test` = 11 read-only GETs, 0
  structural diffs, exit 0.
- `manifest.json` starts with the P0/P1 read-only surface; it expands per phase.
  Mutating/destructive requests are added only against throwaway scopes.
- Nullable fields (`null` on one side) are reported as soft `~` notes, not failures.
- List element shapes are de-duplicated so item order/count never causes a diff.
