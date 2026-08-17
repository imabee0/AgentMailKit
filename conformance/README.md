# Conformance harness

Four independent gates over the same running server. `scripts/p1-gate.sh` stands one up on a
throwaway database and runs all four; every one must exit 0 or the gate does not pass.

| File | Asks | Oracle |
|---|---|---|
| `dual_target.py` + `manifest.json` | does our response have the reference's shape? | the **live** api.agentmail.to |
| `sdk_smoke.py` | can the official Python client use it? | `agentmail==0.5.9` |
| `sdk_smoke.mjs` | can the official Node client use it? | `agentmail@0.5.19` |
| `schemathesis_scope.py` + `schemathesis_checks.py` | does it hold on inputs nobody would write down? | `reference/openapi.json` + our own invariants |

None subsumes another. A response can have the reference's exact shape and still break a typed
client; both clients can be happy on the paths they walk while a malformed page token produces a
500; and the fuzzer knows nothing about what the *reference* does, which is the only thing
`dual_target.py` measures.

## Run

```bash
./scripts/p1-gate.sh          # all four, against a throwaway deployment. This is the gate.
```

Individually, against a server you already have:

```bash
# 1. structural diff vs the live reference. Keys come from sdxd, never inline.
AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c \
  'REF_KEY="$AGENTMAIL_API_KEY" CAND_BASE=http://127.0.0.1:8111 CAND_KEY="$AMK_KEY" \
   python3 conformance/dual_target.py conformance/manifest.json'

# comparator self-test: the reference diffed against itself must be all PASS
AGENTMAIL_API_KEY='sdxd:agentmail' sdxd run -- bash -c \
  'REF_KEY="$AGENTMAIL_API_KEY" python3 conformance/dual_target.py conformance/manifest.json --self-test'

# 2/3. the official clients, unmodified, with only the base URL changed
AMK_BASE=http://127.0.0.1:8111 AMK_KEY=<root key> .venv-gate/bin/python conformance/sdk_smoke.py
AMK_BASE=http://127.0.0.1:8111 AMK_KEY=<root key> node conformance/sdk_smoke.mjs

# 4. which operations are in scope, and why
./scripts/derive-implemented-paths.sh
```

Keys come from the environment (via `sdxd run`), never hard-coded, never printed.

## dual_target.py

Structural diff only: keys and value **types**, recursively, never values — two accounts hold
different resources, so a value diff is noise. `null` on one side alone is a soft `~` note, not a
failure. List element shapes are de-duplicated, so item order and count never diff.

Placeholders (`{pod_id}`, `{inbox_id}`) are resolved **per target**, by listing that target — the
reference's pod is not our pod, and `inbox_id` IS an email address, so it cannot be guessed.
Resolution is ordered and dependent: a later placeholder's source path is filled with what has
already resolved, because resolving `{pod_id}` and `{inbox_id}` independently picks a pod and an
inbox that need not belong together, and the nested route then 404s on one side and 200s on the
other. An unresolvable placeholder is a `[SKIP]` that **fails** the run; a gate that quietly covers
less while still printing a pass is the failure mode this project keeps finding in its own checks.

`expected_divergences` declares the two fields we will never emit — `billing_plan_id` and
`clerk_organization_id`, one per project non-negotiable — by exact `$.field` path, with a reason,
printed on every run. Without it the gate could never pass; with anything broader it would hide.

## schemathesis

Scope is **derived**, not listed: `schemathesis_scope.py` parses `router()` and reconciles the
result against `openapi.json`, failing if the two disagree, then emits the `--include-path`
arguments. `p1-gate.sh` and `scripts/derive-implemented-paths.sh` both call it, so the gate and any
contract scoped over these handlers cannot disagree about what "implemented paths" means.

`status_code_conformance` is **excluded**, with cause: the schema is AgentMail's own
`openapi.json`, which live captures have contradicted four times, and it is 0-for-3 on DELETE
statuses alone (fixtures 22, 23). Statuses have a better oracle — `dual_target.py` compares ours
against the live reference's. What is kept is what a spec is good at (response shapes, content
types, no 5xx) plus three project invariants no OpenAPI document can express, in
`schemathesis_checks.py`: optionals are omitted rather than `null`, timestamps are RFC 3339 with
exactly three fractional digits and a `Z`, and every error body is one of the **two** observed
shapes — bare `{"message"}` at 401/403 only, full envelope everywhere else.

## Status

- Comparator validated 2026-08-15 (`--self-test`: 11 read-only GETs, 0 structural diffs, exit 0).
- P1 gate captures: `reference/fixtures/25-p1-gate-conformance.txt` (diff),
  `26-p1-gate-sdk-smoke.txt` (both clients), asserted by `scripts/plan-ledger.sh`.
- `manifest.json` covers the P0/P1 surface; it expands per phase. Mutating requests are added only
  against throwaway scopes.
