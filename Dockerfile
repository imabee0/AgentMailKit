# syntax=docker/dockerfile:1.7
#
# AgentMailKit runtime image.
#
# Two properties this file exists to hold, both of which are easy to lose silently:
#
# 1. THE DEPENDENCY LAYER IS CACHED SEPARATELY FROM THE SOURCE LAYER. The workspace resolves 308
#    packages, and a plain `COPY . . && cargo build` rebuilds every one of them whenever a single
#    line of our own code changes. BuildKit's GHA cache would then be almost worthless: it would
#    hit on a layer nothing ever reuses. cargo-chef splits the build so the expensive layer is
#    invalidated only by Cargo.lock -- which, because every dependency here is pinned exactly, is
#    a file that changes deliberately and rarely.
#
# 2. THE BUILD IS REPRODUCIBLE. Both bases are pinned BY DIGEST, not by tag: `rust:1.94.1-slim`
#    is whatever that tag points at today, and a rebuilt image that differs from the one CI gated
#    is not the artifact anybody reviewed. SOURCE_DATE_EPOCH comes from the commit, so timestamps
#    inside the image derive from the source rather than from the clock.
#
# ONE TRADEOFF, STATED. This stage compiles from source rather than COPYing the binaries
# `ci.yml`'s build-bins job already produced, so a CI run compiles the workspace twice: once for
# the smoke/test jobs and once inside Docker. Copying prebuilt binaries in would remove that, at
# the cost of making `docker build .` impossible outside CI -- the image would only be buildable
# from an artifact of a particular workflow run. Reproducibility and standalone buildability win.
#
# What "build once" guarantees here is the part that matters for promotion: ONE image digest is
# built and then re-tagged, never rebuilt, for every environment it reaches (release.yml uses
# `buildx imagetools create`, which copies a manifest and pulls no layers). The duplicated compile
# is inside a single CI run and is absorbed by the cargo-chef + GHA layer cache.

# The image carries no migration step: `sqlx::migrate!` compiles the migrations INTO the binary
# (crates/amk-store/src/pool.rs:10), so `amk migrate` and `amkd` ship as one artifact and cannot
# disagree about the schema.

ARG RUST_IMAGE=rust:1.94.1-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# ---------------------------------------------------------------------------- chef
FROM ${RUST_IMAGE} AS chef
WORKDIR /build
# `ring` and `aws-lc-sys` compile C, and sqlx/mail-send need a linker. `--no-install-recommends`
# keeps this layer from dragging in a toolchain we do not use.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.68

# ---------------------------------------------------------------------------- planner
# Produces a recipe describing ONLY the dependency graph. Source changes do not alter it, which is
# precisely why the next stage caches.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------- builder
FROM chef AS builder
ARG SOURCE_DATE_EPOCH=0
ENV SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}
# The expensive layer, and the only one that matters for cache hit rate. Invalidated by recipe.json
# -- i.e. by Cargo.lock -- and by nothing else.
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Now the source. Everything above is reused; only our own ~20k lines recompile.
COPY . .
# rust-toolchain.toml is COPYed with the source above, so this build uses the same pinned compiler
# as CI and as a developer's machine. A toolchain named here instead would be a second record of
# that fact, free to drift from the file.
RUN cargo build --release -p amk-cli --bins \
 && strip target/release/amk target/release/amkd

# ---------------------------------------------------------------------------- runtime
FROM ${RUNTIME_IMAGE} AS runtime
# ca-certificates is not optional: outbound SMTP does STARTTLS to every MX, and DKIM verification
# resolves over TLS. Without it every send fails at handshake.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Non-root, no shell, no home. `docs/PLAN.md` requires PSA restricted / non-root / no capabilities;
# this is the image half of that, and it must hold whether or not the eventual manifests set it.
RUN groupadd --system --gid 10001 amk \
 && useradd --system --uid 10001 --gid amk --no-create-home --shell /usr/sbin/nologin amk

COPY --from=builder --chown=root:root --chmod=0755 /build/target/release/amk  /usr/local/bin/amk
COPY --from=builder --chown=root:root --chmod=0755 /build/target/release/amkd /usr/local/bin/amkd

USER 10001:10001

# Documentation only -- the actual listen address is AMK_BIND, and the API and SMTP roles are
# separate containers sharing this image. EXPOSE publishes nothing by itself.
EXPOSE 8080 25

# No default AMK_DATABASE_URL, deliberately: crates/amk-cli/src/config.rs refuses to start without
# one rather than falling back to a development database, and a default here would undo that.
ENTRYPOINT ["/usr/local/bin/amkd"]
CMD ["--role", "api"]
