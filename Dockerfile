# BASE IMAGES: pinned by DIGEST, and switchable to a GitHub-hosted mirror.
#
# A digest is immutable; `rust:1.94.1-bookworm` is a tag and can be repointed by whoever controls
# it. That is the same supply-chain hole the workflows' `uses:` pins close, and it was open here
# while being closed there.
#
# The two ARGs default to Docker Hub, the canonical home of these official images, which works with
# no setup. Point them at the GHCR mirror to keep every pull on the same platform as CI:
#
#   .github/workflows/mirror-base-images.yml copies these exact digests into
#   ghcr.io/<owner>/<repo>/base/*, and `docker buildx imagetools create` copies BY digest, so the
#   mirrored image carries the identical sha256 — the pins below do not change when you switch.
#
# Why mirror at all: Docker Hub rate-limits anonymous pulls per IP and GitHub's runners share IPs,
# so a busy day can turn an unrelated pull into a red build. GHCR has no such limit for Actions and
# is already authenticated by GITHUB_TOKEN, so it needs no extra credential.
#
# The tag each digest came from rides on its OWN comment line above the ARG. It cannot ride at the
# end of the ARG line: a Dockerfile only treats `#` as a comment at the START of a line, so a
# trailing one becomes part of the value and buildx rejects it with "invalid reference format".
# That mistake shipped and was caught by image-validate on the first real build of this file.
#
# To re-resolve after a version bump (no Docker daemon and no registry login needed):
#   t=$(curl -s "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/rust:pull" | jq -r .token)
#   curl -sI -H "Authorization: Bearer $t" -H 'Accept: application/vnd.oci.image.index.v1+json' \
#     https://registry-1.docker.io/v2/library/rust/manifests/<tag> | grep -i docker-content-digest
# base-tag rust: 1.94.1-bookworm
ARG RUST_IMAGE=docker.io/library/rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
# base-tag debian: bookworm-slim
ARG RUNTIME_IMAGE=docker.io/library/debian@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

# MUST match rust-toolchain.toml's channel. Pinned at 1.85.0 while the toolchain file said 1.94.1,
# which would have failed every merge to main: the locked dependency set needs >= 1.94
# (sqlx 0.9.0 declares rust-version 1.94.0), so `cargo build --locked` on 1.85 refuses outright.
# Caught locally rather than by a red main. `plan-ledger.sh`'s `docker-rust-version-matches`
# asserts the two agree, because Docker cannot read the TOML at FROM time and a version that only
# has to be remembered is a version that drifts.

# One image, both roles. `amkd --role api` and `amkd --role smtpd` are the same binary with
# different arguments, so shipping two images would mean two things to promote and two chances for
# them to differ. The Kubernetes Deployments differ only in their command.

# ---------------------------------------------------------------------------- planner
# cargo-chef exists to solve one problem: `COPY . .` before `cargo build` means any source edit
# invalidates the dependency layer and recompiles every crate in the tree. The planner reduces the
# manifests to a recipe that changes ONLY when dependencies change, so the expensive layer is
# keyed on the thing that actually affects it.
FROM ${RUST_IMAGE} AS planner
WORKDIR /build
RUN cargo install cargo-chef --locked --version 0.1.78
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------- dependency cache
FROM ${RUST_IMAGE} AS deps
WORKDIR /build
RUN cargo install cargo-chef --locked --version 0.1.78
COPY --from=planner /build/recipe.json recipe.json
# This layer is reused across every build whose dependency graph is unchanged — which is almost
# all of them, and it is the layer that costs minutes.
RUN cargo chef cook --release --locked --recipe-path recipe.json

# ---------------------------------------------------------------------------- build
FROM ${RUST_IMAGE} AS builder
WORKDIR /build
COPY --from=deps /build/target target
COPY --from=deps /usr/local/cargo /usr/local/cargo
COPY . .
# --locked so an image build can never silently resolve a different dependency set than the one
# CI tested. SQLX_OFFLINE is not set: this workspace uses runtime-checked queries, not the
# compile-time macros, so no database is needed to build.
RUN cargo build --release --locked -p amk-cli --bins \
 && strip target/release/amk target/release/amkd

# ---------------------------------------------------------------------------- runtime
# Debian slim rather than distroless or alpine, for two specific reasons: the binaries link
# against glibc (musl would need a separate target and a separate test matrix to be honest about),
# and DKIM/TLS verification needs a real CA bundle.
FROM ${RUNTIME_IMAGE} AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*

# Non-root, no shell, no home. The SMTP role binds 2525 rather than 25 precisely so the container
# never needs NET_BIND_SERVICE; the Service maps 25 to it.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin amk
USER 10001:10001

COPY --from=builder /build/target/release/amk  /usr/local/bin/amk
COPY --from=builder /build/target/release/amkd /usr/local/bin/amkd

# The licence travels with the binaries. Under AGPL-3.0 section 13 a network server is
# distribution to everyone who interacts with it, so shipping the terms inside the image is the
# baseline obligation rather than a courtesy. The org.opencontainers.image.licenses label below
# only NAMES the licence; this is the text.
COPY LICENSE /usr/share/licenses/agentmailkit/LICENSE

# tini reaps zombies and forwards signals, so a rollout's SIGTERM reaches amkd and in-flight SMTP
# sessions close cleanly instead of being killed at the end of the grace period.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/amkd"]
CMD ["--role", "api"]

# Consumed by .github/workflows/ci.yml's metadata-action; these are what make the published
# package link back to its source and licence in the GHCR UI.
LABEL org.opencontainers.image.source="https://github.com/Appsynergy-io/AgentMailKit" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="Self-hosted, API-compatible AgentMail server"
