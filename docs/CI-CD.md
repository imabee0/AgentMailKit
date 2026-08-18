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
| `gate-lane-l` | if Rust/conformance/migrations changed | always | — | minutes; schemathesis + both official SDKs |
| `image` | — | yes | — | one image per merged commit |
| `deploy-*` | — | if `DEPLOY_ENABLED` | — | promotion, gated by environments |

The pattern: **cheap checks run on everything; expensive checks run when their inputs changed; on
`main` everything runs regardless.** A pull request cannot skip its way to green, because `main`
re-runs the full set before anything is built or deployed.

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

---

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

### Known gap: actions are pinned by tag, not by SHA

`uses: actions/checkout@v4` resolves a mutable tag at run time, so whoever can move that tag runs
code inside a job holding a registry token. SHA pinning closes it.

This is **not yet done**, and is tracked as the open ledger obligation `ci-actions-sha-pinned`
rather than quietly asserted, because the environment these workflows were written in could not
reach `api.github.com` to resolve the tags — and a fabricated SHA fails every run while looking
rigorous. To close it, from a machine with network:

```bash
gh api repos/actions/checkout/commits/v4 --jq .sha    # for each `uses:` under .github/
```

Rewrite each `uses: owner/repo@tag` as `uses: owner/repo@<sha> # tag`, then promote
`ci-actions-sha-pinned` from a `pend` to a `check` asserting every non-local `uses:` ends in 40 hex
characters.

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
