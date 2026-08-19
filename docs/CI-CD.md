# Development and CI/CD

How this project is verified, from a laptop to production. `docs/PLAN.md` remains the **contract** —
what the software must do and which gates define each phase. This file is the **execution layer**:
how those gates actually run, where, and when.

The rule that ties them together: **CI is the authoritative gate.** Local tooling is a pre-flight
that helps you get there faster; it is not the thing that decides whether code is good.

---

## 1. The local workflow

```bash
./scripts/bootstrap.sh        # provision everything a check needs (idempotent)
./scripts/check.sh            # the full local pre-flight
./scripts/check.sh --fast     # drops clippy and the audit (what the Stop hook runs)
```

`bootstrap.sh` installs the pinned toolchain, `cargo-deny`, both pinned conformance virtualenvs and
the pinned Node SDK, puts the Postgres server binaries on `PATH`, and starts the dev cluster. It
ends by printing which of the eight steps can actually run here.

It is **tracked in the repository on purpose.** Most work on this project happens in an ephemeral
sandbox that starts with the repo and whatever the base image carries; anything a check needs must
therefore ship *as a script in the repo*, not be assumed present. A step that is chronically
`NOT RUN` is a step nobody is really running. `bootstrap.sh` makes `NOT RUN` rare; the `NOT RUN`
status itself proves it stayed rare.

In **Claude Code on the web** this runs automatically — `.claude/hooks/session-start.sh` is a
registered `SessionStart` hook that calls it, synchronously, so a session never begins before the
database is up. It is a no-op outside a remote session (`CLAUDE_CODE_REMOTE`), so it never starts a
database under someone's local editor. CI does **not** use it: Actions provisions through
`services:` containers, `setup-python`, `setup-node` and a prebuilt `cargo-deny`, which are faster
and cacheable there.

Running one specific check — this is how you reproduce a CI failure:

```bash
./scripts/verify.sh --list
./scripts/verify.sh clippy            # exactly what CI's "clippy" job runs
./scripts/verify.sh test              # exactly what CI's "test" job runs
./scripts/verify.sh fmt clippy test   # several, in order, stopping at the first failure
```

### Why there are two scripts and not one

`scripts/verify.sh` **defines** every step. `scripts/check.sh` **sequences** some of them for local
use. CI calls `verify.sh` directly, one step per job.

That split exists because the previous single script could not serve both masters. `check.sh` is
run by the Stop hook, which blocks a turn from ending and is disabled by Claude Code after 8
consecutive blocks — so it must be fast and must never hang. It paid for that by degrading quietly:
no rustfmt → skip, no clippy → skip, no Postgres → run the suite anyway and exit `PASS` having
verified none of the DB-backed integration tests.

Those are reasonable in a pre-flight and disqualifying in a merge gate. A gate that passes because a
tool was missing reports the same green as one that passed because the code was correct.

### Three outcomes, and two of them are failures

| Exit | Status | Meaning |
|---|---|---|
| 0 | **PASS** | the step ran, and the code satisfied it |
| 1 | **FAIL** | the step ran, and the code did not |
| 3 | **DEPENDENCY MISSING** | the step could not run — **also a failure** |

Exit 3 exists only so the remedy printed is the right one — "provision this machine" rather than
"fix your code". It is never a pass, in any environment, under any flag. There is no `INCOMPLETE`
state and no escape-hatch variable; `check.sh` goes red if any step could not run.

| Situation | old `check.sh` | now, everywhere |
|---|---|---|
| rustfmt missing | skipped, PASS | **FAIL** |
| clippy missing | skipped, PASS | **FAIL** |
| Postgres unreachable | partial suite, **PASS** | **FAIL** |
| cargo-deny missing | (no such check) | **FAIL** |

Being this strict is affordable because provisioning is solved rather than assumed. The project
targets exactly two environments and `./scripts/bootstrap.sh` fully provisions both, so a missing
dependency means bootstrap was not run or failed — which is worth a red run. The alternative is a
green run that examined less than it appears to have, and this project has already shipped that
defect once, when `check.sh` printed `PASS` with no Postgres.

```
== check summary ==
  passed: ledger
  DEPENDENCY MISSING: fmt fixtures test provenance

check: FAIL — this machine is not provisioned, so the steps above did not run.
Run ./scripts/bootstrap.sh and try again. A run that skipped checks is not a pass.
```

### What the Claude sandbox actually has

Verified against the published cloud-environment reference and confirmed on a live sandbox:

| | Status |
|---|---|
| Ubuntu 24.04, running as root, `sudo` available | preinstalled |
| Rust — `rustc`, `cargo`, `rustup` | preinstalled (so `rust-toolchain.toml` is honoured) |
| Python 3.x with `pip` | preinstalled |
| Node 20 / 21 / 22 under `/opt/nodeNN`, 22 on `PATH` | preinstalled |
| **PostgreSQL 16** | **installed but NOT running**, and `initdb`/`pg_ctl` are **not on `PATH`** — they live under `/usr/lib/postgresql/16/bin`, so `psql` being present does not imply `initdb` is |
| `git`, `jq`, `ripgrep` | preinstalled |
| Docker client | present; the daemon is not running by default |
| **`cargo-deny`** | **not preinstalled** — the only thing this project needs that isn't |

Network access is **Trusted** by default, which reaches crates.io, PyPI and npm — so `bootstrap.sh`
can install what's missing. At the **None** access level it cannot, and the checks then fail, which
is the correct outcome rather than a reason to soften them.

Whatever bootstrap installs is kept by the environment cache, so later sessions in the same
environment start already provisioned and pay the `cargo-deny` build only once.

### Reproducing a CI failure

Every CI job runs a single `./scripts/verify.sh <step>` and the log names it. Run that same step
locally and you get the same commands, the same flags, and the same compiler — `rust-toolchain.toml`
pins the toolchain for both, so "works on my machine" cannot be a version difference.

---

## 2. The CI pipeline

One workflow, `.github/workflows/ci.yml`. The job graph:

```
                    ┌──────────┐
                    │ changes  │  path filters → booleans
                    └────┬─────┘
        ┌────────────────┼────────────────┬───────────────┐
        │                │                │               │
      fmt            ledger            hooks            audit        (no compile — start instantly)
                                                                      
                    ┌──────────┐
                    │  build   │  compiles workspace + all test targets ONCE
                    └────┬─────┘  saves the only cache; uploads amk/amkd
        ┌────────┬───────┼────────┬──────────────┐
        │        │       │        │              │
     clippy    test   fixtures provenance   gate-lane-l   (all restore build's cache)
        └────────┴───────┴────────┴──────────────┘
                         │
                    ┌────┴─────┐
                    │   gate   │  ← the ONLY required status check
                    └────┬─────┘
                         │  main only
                    ┌────┴─────┐
                    │  image   │  build once → GHCR, by digest
                    └────┬─────┘
                deploy-staging → deploy-production   (same digest, promoted)
```

### When each class of check runs

| Check | Pull request | main | Nightly | Why |
|---|---|---|---|---|
| `fmt` | if Rust changed | always | — | seconds; no compile |
| `ledger` | **always** | always | — | gates the harness *and* this pipeline's own config |
| `hooks` | if `scripts/`, `.claude/`, `.github/` changed | always | — | the write-guard's own 49 tests |
| `build` | if Rust/fixtures/conformance changed | always | — | produces the cache and the binaries |
| `clippy` | with `build` | always | — | `-D warnings` |
| `test` | with `build` | always | — | full suite against a real Postgres |
| `fixtures` | with `build` | always | — | fixture corpus reconciliation |
| `provenance` | with `build` | always | — | no Stalwart/JMAP shapes |
| `audit` | if dependencies changed | always | **yes** | advisories land without this repo changing |
| `gate-lane-l` | **reduced** (~3 min) if Rust/conformance/migrations changed | **full** (~50 min) | — | schemathesis + both official SDKs |
| `image` | — | if `PUBLISH_IMAGE` | — | off by default — see below |
| `image-validate` | if `Dockerfile` changed | always | — | builds the image, never pushes; required by `gate` |
| `deploy-*` | — | if `DEPLOY_ENABLED` | — | promotion, gated by environments |

The pattern: **cheap checks run on everything; expensive checks run when their inputs changed; on
`main` everything runs regardless.** A pull request cannot skip its way to green, because `main`
re-runs the full set before anything is built or deployed.

### The Lane L gate runs at two depths

It is the most expensive thing in the pipeline. Measured on this repository, 45 mounted operations,
one full local run:

| schemathesis phase | time |
|---|---|
| coverage (deterministic edge cases) | 1519s — 25 min |
| fuzzing (property-based, scales with `--max-examples`) | 1039s — 17 min |
| stateful (follows OpenAPI links) | 249s — 4 min |
| examples | 0.2s |

About **47 minutes of schemathesis**, ~50 for the job. It first ran with `timeout-minutes: 45`, was
killed at 45.3 minutes mid-run, and produced nothing — the most expensive way this pipeline could
fail. The ceiling is now 90.

Paying 50 minutes on every pull request is disproportionate; never paying it is not a gate. So:

| Trigger | Profile | Measured |
|---|---|---|
| pull request | reduced — `examples,fuzzing`, 5 examples | **2m 51s** |
| pull request labelled `full-gate` | full | ~50 min |
| push to `main` | full | ~50 min |
| `workflow_dispatch` | full | ~50 min |

The reduced profile still stands up a real server, runs **both official SDK smokes in full**, and
fuzzes **every** mounted operation. It lowers depth, not surface coverage. Coverage and stateful —
the two most expensive phases — are what it drops.

`full-gate` is the pre-merge escape hatch: label a pull request and the full guard runs on it
before merge. Otherwise the full guard runs on `main` immediately after merge, which is a
deliberate cost trade and is stated here rather than left implicit.

The gate prints which profile ran (`profile: FULL` / `profile: REDUCED — NOT the full guard`), for
the same reason `--lane-l` prints its lane: a narrower run must never read as the wider one.

### `gate` — the required status check

Branch protection points at the `gate` job and nothing else.

This is not cosmetic. **A required check that was skipped reports success to branch protection.** If
`test` were required directly, a pull request that arranged for change-detection to skip it would
merge green having run nothing. `gate` runs unconditionally (`if: always()`), inspects the result of
every job it depends on, and fails unless each is `success` or `skipped` — `failure` and `cancelled`
both stop the merge.

Adding a new job means adding it to `gate`'s `needs:` list. That is the one piece of manual
bookkeeping in the pipeline, and it is deliberate: a job nobody put behind the gate is a job nobody
decided was required.

---

## 3. Change detection

`dorny/paths-filter` computes booleans once, in the `changes` job; everything else reads them.

The filters name **inputs to a build**, not directories for tidiness:

- `rust` — `crates/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `rustfmt.toml`, `deny.toml`.
  `Cargo.lock` is in there because a dependency change alters what compiles; `rust-toolchain.toml`
  because a compiler change does; `deny.toml` because it changes what the audit accepts.
- `migrations` — schema changes, which can break the gate without touching a `.rs` file.
- `conformance` — the harness and `reference/openapi.json`, which define what schemathesis fuzzes.
- `harness` — `scripts/**`, `.claude/**`, `.github/**`.
- `fixtures` — `reference/fixtures/**`; the corpus is the regression suite.
- `docker` — `Dockerfile`, `.dockerignore`.

One derived output, `always`, is `true` for anything that is not a pull request. It is what makes
`main`, manual runs and the nightly audit ignore change detection entirely.

---

## 4. Caching

Three caches, each keyed on the inputs that actually affect what it stores.

**Cargo / `target/`** — `Swatinem/rust-cache`, keyed on `Cargo.lock` + `rust-toolchain.toml` +
a shared key. Two shared keys exist:

- `workspace` — used by `build`, `clippy`, `test`, `fixtures`, `provenance`. **Only `build` saves**
  (`save-cache: "true"`); everything else reads. One writer is deliberate: parallel jobs saving
  near-identical multi-hundred-megabyte caches race and evict each other, and the loser's upload is
  pure cost.
- `lint` — `fmt` and `audit`, which need a toolchain but no compiled workspace. Sharing the big
  cache with them would mean downloading hundreds of megabytes to run `cargo fmt --check`.

**Python virtualenvs** — `.venv-gate` and `.venv-schemathesis`, keyed on
`hashFiles('conformance/requirements-*.txt')`. A pin change invalidates; nothing else does.

**Docker layers** — `type=gha` with `mode=max`, so intermediate stages survive. Combined with
`cargo-chef` in the Dockerfile, a source-only change reuses the compiled-dependency layer, which is
the layer that costs minutes.

### Why not key on the commit SHA

Because then every cache would miss. Cache keys must name the *inputs to the cached work*: the
lockfile, the toolchain, the requirements files. Keying on the OS alone is the opposite error —
it serves stale artifacts after a toolchain bump, which is how `-D warnings` produces a red build
on a day nobody changed any Rust.

### A caching bug this exposed

`p1-gate.sh` used to install its Python requirements **only when the virtualenv did not exist**. A
venv that existed but was incomplete — a half-built one, or one restored from a cache built against
different pins — was reused silently, and the SDK smoke failed with `ModuleNotFoundError` as though
the SDK itself were broken. It now creates the venv if absent and **syncs the pinned requirements on
every run**; `pip install -r` is a no-op in about a second when already satisfied. A restored cache
is now corrected rather than trusted.

---

### The image job is off by default, deliberately

`image` and both `deploy-*` stages are behind repository variables (`PUBLISH_IMAGE`,
`DEPLOY_ENABLED`) and do not run until you set them.

**Nothing consumes the image yet.** With deploys off, building and *pushing* a container on every
merge is work whose output has no reader, so publishing stays off until something reads it.

The Dockerfile itself is **not** unverified. A separate `image-validate` job builds the container
**without pushing**, on any pull request that changes `Dockerfile` or `.dockerignore`, and it is
one of `gate`'s required jobs. That job exists precisely because the environment these workflows
were authored in has Docker Hub blocked at its proxy (`docker pull alpine:3.20` → 403 from the
CDN): the Dockerfile could not be built locally, and "I could not verify it here" is not a reason
to ship it unverified. A GitHub runner reaches Docker Hub, so CI is where it gets proven.

`image-validate` holds **no registry permissions at all** — it cannot push by construction rather
than by an `if:`. It is scoped to changes in the image definition, not to every Rust change, because
a release build costs minutes and source correctness is already covered by `build`/`clippy`/`test`.

`workflow_dispatch` triggers `image` when `PUBLISH_IMAGE` is set, so it can be validated once on
demand before being switched on, instead of discovering its first defect as a red `main`.

One defect in it was already found and fixed without running it: all three stages pinned
`rust:1.85.0-bookworm` while `rust-toolchain.toml` says `1.94.1` and the locked dependency set
refuses to build below 1.94 (`sqlx 0.9.0` declares `rust-version = 1.94.0`). That would have failed
every merge. `plan-ledger.sh`'s `docker-rust-version-matches` now asserts the two agree, because
Docker cannot read the TOML at `FROM` time and a version that only has to be remembered drifts.

---

## 5. Artifact flow

Compiled **once**, consumed everywhere:

1. `build` runs `cargo build --workspace --all-targets --locked`, saves the cache, and uploads
   `target/debug/amk` and `target/debug/amkd` as the `amk-binaries` artifact.
2. `clippy`, `test`, `fixtures`, `provenance` restore the **cache**, so their cargo invocations are
   link steps rather than rebuilds.
3. `gate-lane-l` downloads the **binary artifact** and never invokes cargo at all.
4. On `main`, `image` builds one container and pushes it to GHCR. Its **digest** is a job output.
5. `deploy-staging` and `deploy-production` are two calls to the same reusable workflow, both given
   that same digest.

Production runs the bytes staging validated. Deploying by tag would break that — a tag can move
between the staging deploy and the production pull; a digest cannot. Nothing is rebuilt per
environment, because an image rebuilt for production is an image nothing tested.

`actions/attest-build-provenance` signs the digest, so the published image is traceable to the
workflow run and commit that produced it.

---

## 6. Reusable workflow vs composite action

Both are used, for opposite reasons, and the distinction is worth keeping straight:

- `.github/actions/setup-rust` is a **composite action** — toolchain + cache, needed by six jobs. A
  composite action inlines into the calling job. Making this a reusable workflow would add a whole
  runner spin-up per job for four lines of setup.
- `.github/workflows/deploy.yml` is a **reusable workflow** — it *has* to be a separate job, because
  `environment:` is a job-level key, and the environment is what carries GitHub's protection rules
  (required reviewers, wait timers, scoped secrets). Those rules are the production approval gate.
  An `if:` expression is not: a contributor can edit an `if:` in a pull request and cannot edit an
  environment's protection rules.

### The permissions rule that cost five red runs

**A called workflow may not request more permissions than its calling job holds, and GitHub
enforces this when it parses the run.** `deploy.yml`'s job asks for `packages: read` to pull the
image it promotes. The calling jobs originally inherited the workflow default of `contents: read`,
so every run concluded **`startup_failure`** — no job, no log, no annotation, and nothing in the
API to read. `actionlint` and a strict YAML parse were both clean.

It was found by bisection, because there was nothing else to read: removing the deploy jobs let the
workflow start, and eight one-feature probe workflows then isolated it — a declared secret, an
`environment:` expression and a job-`name:` expression were each individually fine; adding the
job-level `permissions` block was not. Two final probes differing *only* in the caller's
`permissions` settled it: without it `startup_failure`, with it the run started and reached
`deploy.yml`'s own credential check.

So `deploy-staging` and `deploy-production` each carry:

```yaml
    permissions:
      contents: read
      packages: read
```

**Keep these in step with `deploy.yml`.** Any permission added to the called workflow must be added
to both callers, or the entire pipeline stops starting — including every job unrelated to deploy.

---

### Why there is no matrix job

Matrices were considered and deliberately not used. Each candidate axis makes this pipeline worse,
not better:

- **Operating system.** The deployment target is Linux on k3s. A macOS or Windows leg would test a
  platform this project never runs on, at triple the minutes.
- **Rust version.** `rust-toolchain.toml` pins exactly one compiler, and that pin is the point —
  it is what makes a CI failure reproducible locally. An MSRV-versus-stable matrix earns its keep
  for a published library with downstream consumers; these crates are `publish = false` and are
  consumed only by this repository's own binary.
- **Test sharding.** Compilation dominates the test job — the suite itself runs in seconds once
  built. Sharding across N runners would multiply the expensive half and parallelise the cheap
  half, increasing total minutes to reduce wall-clock on a job that already finishes quickly.

Where genuine parallelism exists it is expressed as separate jobs (`clippy`, `test`, `fixtures`,
`provenance` all run concurrently off one `build`), which is the right shape for work that differs
in kind rather than in parameter.

## 7. Security posture

- **Least privilege.** Every workflow declares top-level `permissions: contents: read`. Only `image`
  escalates, to `packages: write` plus the attestation scopes. `plan-ledger.sh`'s
  `ci-layer-github-actions` check fails the build if any workflow omits a top-level `permissions:`
  block or grants `write-all`.
- **No third-party credential in CI, ever.** The dual-target conformance diff is the only check that
  calls `api.agentmail.to`. `p1-gate.sh --lane-l` omits it, and that is the only form CI runs. A
  pull request can read any secret its workflow is given; the right number of places a live
  AgentMail key can appear in Actions is zero. Lane R stays an operator-run step on a trusted
  machine — which is a security decision, not a limitation.
- **Supply chain.** `cargo-deny` covers advisories, licence policy and source restrictions, on
  dependency changes and nightly.
- **Concurrency.** Superseded pull-request runs are cancelled; runs on `main` never are, because
  they publish and deploy and a half-finished promotion is worse than a slow one.
- **Deploy is opt-in.** Until the repository variable `DEPLOY_ENABLED` is `true`, the deploy stages
  do not run. Once enabled, a missing `KUBE_CONFIG` **fails** the job rather than no-op'ing. A
  deploy that silently does nothing and reports green is the same defect as a skipped test that
  reports green.

### Every action is pinned to a commit

`uses: actions/checkout@v4` resolves a **mutable** tag at run time, so whoever can move that tag
runs arbitrary code inside a job holding a registry token. All fourteen third-party actions are
pinned to 40-character commit SHAs, with the tag kept in a trailing comment for readability.

`plan-ledger.sh`'s `ci-actions-sha-pinned` enforces it — a real check, not an attestation.

This was briefly recorded as an open obligation on the belief that the authoring environment could
not reach GitHub to resolve tags. That belief was wrong: the session's git proxy serves anonymous
reads of public repositories, so no API access is needed at all. To re-pin after a version bump:

```bash
git ls-remote https://github.com/<owner>/<repo> 'refs/tags/<tag>^{}'
```

The `^{}` is load-bearing: on an **annotated** tag, plain `refs/tags/<tag>` yields the tag object,
and a `uses:` pinned to a tag-object SHA does not resolve.

The check's own matcher had to be falsified before it could be trusted. Its first version anchored
at `uses:` and so matched only 5 of 35 lines — almost every `uses:` here is a YAML list item
(`- uses:`) — and it reported clean while inspecting a seventh of the file. It now allows the list
dash and additionally fails if it sees fewer than 20 lines, so a future matcher regression surfaces
as a failure rather than as a vacuous pass.

---

## 8. Branches and merge order

Branch **names** are not a rule. Whatever the session harness assigns is fine, and CI does not look
at them — branch protection targets the `gate` job, which is name-agnostic.

That is a deliberate retirement, not an omission. The plan used to mandate `amk/<phase>/<crate>`,
which was a proxy for three real requirements and enforced none of them: no hook ever read a branch
name (`grep -c 'amk/' scripts/hooks/guard.sh` → 0), and the repository accumulated three competing
schemes with nothing to stop it. What the rule stood for is kept:

| Requirement | How it is held now |
|---|---|
| One crate per pull request | The PR title, which already follows conventional commits (`feat(amk-ingest): …`) |
| Merged in crate write order | `plan-ledger.sh` → `crate-write-order`, run by the `ledger` job on every event |
| One worktree per dispatch | `scripts/hooks/guard.sh`, which scopes writes by worktree **path** — unchanged |
| No branch outliving its phase | Hygiene; `hygiene-worktrees-swept` covers the worktree half |

`crate-write-order` asserts that if a crate is present, every crate upstream of it in
`amk-types → amk-core → amk-store → amk-http → ingest+outbound → events+jobs →
dns+mcp+reply-extract → import` is present too. A downstream crate landing before its upstream
means the upstream's types were not frozen when the downstream was written against them, which is
the thing the naming convention only gestured at.

---

## 8. What CI does *not* replace

The lane split from `docs/PLAN.md` still holds, and CI runs exactly one side of it.

- **Lane L** — everything in this document. Fully automated, no credential, every pull request.
- **Lane R-key** — the dual-target conformance diff against the live reference API. Operator-run.
- **Lane R-phys** — mail injected from the OVH box, Gmail DKIM/SPF confirmation, a real verified
  domain, an induced bounce, the restore drill, the cutover. Hardware; no pipeline substitutes.

CI going green means **Lane L is satisfied**. It does not mean a phase is gated. `plan-ledger.sh`
still holds the phase obligations, still reads the gate transcripts in `reference/fixtures/`, and
still refuses to advance a phase on local evidence alone.

Three ledger obligations remain `ATTEST` — printed on every run, never enforced, because no machine
can check them: `review-panel-per-diff`, `mutation-at-gate`, `evidence-not-assert`. **CI does not
bind these.** They are listed rather than omitted precisely so nobody mistakes the pipeline's green
for coverage it does not have.
