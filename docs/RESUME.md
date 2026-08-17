# Resume here

Where the last session stopped, so a fresh one — on this workstation or in Claude's cloud sandbox —
can continue without re-deriving it. Update this file in the commit that invalidates it.

**Last updated:** 2026-08-17, after finishing the extractor-rejection work item.

## Verified state

```
$ ./scripts/check.sh
check: PASS
plan-ledger: PASS
$ ./scripts/check.sh --fast   # summed test results
total passed: 570
```

`main` is green at 570 workspace tests with a live Postgres on 127.0.0.1:55432. Without that
database the same command still exits PASS having skipped every DB-backed test — see the sandbox
section of `CLAUDE.md`.

**Re-confirmed in the cloud sandbox, 2026-08-17: `cargo test --workspace` = 570 passed, 0 failed**,
against a hand-started local Postgres (below). The 570 figure therefore holds on both PG17 and PG16.

### DB-backed tests need no Docker any more

`./scripts/dev-db.sh` used to `docker run postgres:17-alpine`, which made every DB-backed
`amk-store`/`amk-http` integration test unrunnable wherever there is no Docker daemon — including
this sandbox, which has the `docker` client and nothing behind it. `check.sh` then exited PASS
having silently skipped that whole suite. It now drives `initdb`/`pg_ctl` directly, so the plain
documented command works in both places and the suite actually runs:

```bash
./scripts/dev-db.sh up      # same port, DSN, role and database as before
```

It finds server binaries on `PATH`, under `/usr/lib/postgresql/*/bin`, `/usr/pgsql-*/bin` and the
Homebrew prefixes, taking the highest version; as root it owns the cluster as an unprivileged user
(`initdb` refuses to run as root) under `/var/lib/amk-dev-db`, otherwise under `.amk-dev-db/` in the
repo. Verified on PostgreSQL 16 here against a baseline recorded on 17, reproducing the 570 exactly,
so the two are interchangeable for this workload.

`./scripts/p1-gate.sh` had the same dependency in three `docker exec ... psql` calls and now talks
to the cluster over TCP. That was worth fixing for more than the sandbox: a container dependency the
gate never needed presented as "this gate is workstation-only", which is a much more expensive
belief than a missing client.

What still genuinely needs the LAN: `sdxd`/`secd` and therefore the live AgentMail key, the live
account, and the OVH box. Of `p1-gate.sh`'s four conjuncts, the dual-target conformance diff and
both SDK smokes need that key; the schemathesis conjunct does not.

## Phase position

**P0: closed.** `amk-types`, `amk-core`, `amk-store`, `amk-http`, `amk-cli` merged, review-panelled
and mutation-verified. Gate met (fixture 24).

**P1: gate conjuncts recorded MET, with one known open divergence.** The ledger reads
`p1-gate-conformance` MET (fixture 25) and `p1-gate-sdk-smoke` MET (fixture 26 — a clean run *and*
a falsification proving failure propagates). `./scripts/p1-gate.sh` is the four-conjunct runner:
dual-target conformance diff, Python SDK smoke, Node SDK smoke, schemathesis over the 25 mounted
operations.

`scripts/plan-ledger.sh` still reads `CURRENT_PHASE=P0`. The divergence that blocked it is now
fixed (next section), but **advancing it still needs `./scripts/p1-gate.sh` to pass in full**, and
three of that gate's four conjuncts need the live AgentMail key. Local green is not the gate.

## The extractor-rejection work item: DONE, unmerged, ungated

**axum extractor rejections escaped our JSON error contract.** schemathesis found that malformed
requests bypassed `AppError` entirely and surfaced axum's own rejections: `text/plain` bodies, and
statuses the error catalog has no code for — **415**, **422**, and **413** (the last from an
unconditional 2 MB `DEFAULT_LIMIT` inside `Bytes::from_request`, which applies whether or not a
`DefaultBodyLimit` layer is installed; amk-http installed none).

The reference's behaviour is `reference/fixtures/27-malformed-request-handling.txt`: every malformed
request is **400 + `application/json` + the full envelope with exactly one `errors[]` entry**.

Contract: `.claude/contracts/amk-http-extractor-rejections.md`, now at **revision 3**.

### What landed

`JsonBody<T>` and `QueryParams<T>` in `crates/amk-http/src/body.rs` wrap axum's own extractors with
`type Rejection = AppError`, mirroring `ids.rs`'s existing `PathPodId` pattern. Content-type is not
enforced and an absent body is `{}` (fixture 27 §2). An explicit `DefaultBodyLimit` comes from
`AppConfig::max_body_bytes`, default 8 MiB and marked `[INFERRED]`.

**Revision 3 fixed two things the first dispatch could not.** Both were defects in the *contract*,
not the work, and both are the same mistake in a third and fourth form — the contract enumerated
extractor *sites* and mistook that for enumerating everything a mandated change reaches:

1. It mandated `AppConfig::max_body_bytes` without making `AppConfig`'s other construction site
   writable. `crates/amk-cli/src/config.rs` builds it with an exhaustive struct literal, so the
   workspace did not compile — an added `impl Default` does not rescue an exhaustive literal.
2. It mandated `limit` behaviour `crate::pagination`'s own types could not express, while leaving
   `pagination.rs` unwritable. `ListQuery::limit` was `Option<u64>`, and fixture 27 §1 requires
   `?limit=abc` (`invalid_type`/`received:"NaN"`) to split from `?limit=-1`, `?limit=` and
   `?limit=0` (one *identical* `too_small` body). `u64::from_str` fails on `"-1"`, `""` and `"abc"`
   the same way, and `"0"` parses and reaches no validator — so the split was unrepresentable and
   the first dispatch pinned three divergences in tests instead. `MAX_LIMIT = 100` separately
   clamped `?limit=101` to echo `100`, which is a value clients paginate against, not cosmetics.

`limit` is now `Option<String>` classified structurally by `pagination::parse_limit`, which *deletes*
the `limit` half of `body.rs`'s serde-message string matching rather than adding to it. The clamp
and `MAX_LIMIT` are gone: fixture 27 §1 records that no cap is enforced. **No divergences remain.**

### Evidence

```
./scripts/check.sh              check: PASS   shape-provenance: PASS   plan-ledger: PASS
cargo test --workspace          587 passed; 0 failed        (main was 570)
```

Mutation pass, both directions, on a scratch copy outside the tree, since deleted:

| mutation | kills |
|---|---|
| delete the empty-`limit` guard | `empty_negative_and_zero_limits_are_all_the_same_too_small_issue` |
| widen: every rejection -> one `too_small` | `a_non_numeric_limit_is_invalid_type_nan_not_too_small` |
| restore the `MAX_LIMIT` clamp | `a_limit_above_one_hundred_is_accepted_and_echoed_verbatim` **and** `a_limit_above_the_maximum_is_neither_clamped_nor_rejected` |
| narrow `i128` -> `i64` | `a_negative_below_i64_min_is_still_too_small_not_nan` |

`./scripts/derive-request-extractors.sh` re-run: sections 1–2 show **no** bare `Json<`/`Query<` left
in argument position; section 4 lists both new wrappers. Fixture 27's probe table was replayed
against a locally-served `amkd --role api` and every row matches, including `?limit=101` -> 200 with
`"limit":101`, and the two content-type rows now returning 200 rather than 415. No 415, 422, 413 or
`text/plain` anywhere in the surface.

### What is NOT done

- **The three review lenses have not run** on this diff. Contract-conformance, provenance and
  test-adequacy are required before merge and none has been dispatched.
- **`./scripts/p1-gate.sh` has not been re-run in full.** Three of its four conjuncts need the live
  AgentMail key via `sdxd`, so this is workstation-only. Until it passes, `CURRENT_PHASE` stays at
  `P0` — declaring P1 met on the strength of the local suite alone is exactly the "gate its own
  evidence contradicts" this file warned about.
- The schemathesis conjunct still needs its own fixture capture and ledger check.

## Outstanding, needs the user

**Two pods are still on the live AgentMail account** and should be deleted:
`083523ee-276c-417a-85a3-8703d230c543` and `1c5a543a-5219-41fd-b4a1-289355162f2f`, both named
"My Pod", created 2026-08-17T00:59:41Z. They were created by a probe that wrongly reasoned a
body-less POST could not create anything — `CreatePodRequest`'s fields are all optional, so an
absent body **is** a valid create. Recorded in fixture 27 §4. The local permission layer correctly
blocked the agent from issuing the deletes.

```bash
# run this yourself; the classifier blocks an agent from deleting live resources
sdxd run --with agentmail=kv/agentmail -- bash -c '
  for p in 083523ee-276c-417a-85a3-8703d230c543 1c5a543a-5219-41fd-b4a1-289355162f2f; do
    curl -sS -o /dev/null -w "$p -> %{http_code}\n" -X DELETE \
      -H "Authorization: Bearer $AGENTMAIL_API_KEY" "https://api.agentmail.to/v0/pods/$p"
  done'
```

**The rule this bought:** read the request *schema* before calling a POST non-creating. "Malformed"
is a property of the parse, not of the outcome.

## Registers

- **Register A: empty** except A10(g), a non-blocking confirmation tail on its own T+30d clock.
- **Register C2 is the only open boundary question** — whether a thread's labels are a strict union
  of its members'. No fixture has a mixed-label thread. The fail-closed choice (filter membership,
  recompute aggregates) is implemented and marked `[INFERRED]` in one function in
  `amk-core::labels`.
- Deferred by decision, not by oversight: blobs, FTS, signed download URLs, the jobs table,
  idempotency.

## Migration notes (2026-08-17)

- Forge is now `https://github.com/Appsynergy-io/AgentMailKit` (private). The Gitea remote was
  dropped; that copy is unmaintained.
- The plan moved from `~/.claude/plans/download-agents-mail-sdk-drifting-frog.md` to `docs/PLAN.md`
  so a sandbox session reads the same contract. `scripts/hooks/guard.sh` protects both paths, and
  `guard.test.sh` covers the new one in both directions (falsified: removing the glob makes the
  blocking test fail).
- Machine-local operating rules were copied into `docs/OPERATING-RULES.md` for the same reason.
- `harness-no-github` was **not** in fact retired from the ledger, contrary to what this file said
  until 2026-08-17: `scripts/plan-ledger.sh:94` still asserts it, still described as "no .github/
  (Gitea only)". It currently reads MET only because no `.github/` directory exists. It will fire
  the moment anyone adds a PR template or an issue template — neither of which is CI, and both of
  which are ordinary on GitHub. Either retire it for real or re-key it on workflow directories
  alone; `ci-layer-local-only` already holds the no-CI decision that way.
- **Pending, needs the user:** `.claude/settings.json` still denies `Bash(gh:*)`. The auto-mode
  classifier correctly blocks an agent from editing its own permissions, so that patch is applied
  by hand — see the migration commit message.
