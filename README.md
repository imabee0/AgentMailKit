# AgentMailKit

A self-hosted, 1:1 API-compatible reimplementation of [AgentMail](https://agentmail.to) in Rust.

The goal is narrow and testable: the **unmodified** official AgentMail SDKs, CLI and MCP bridge
must work against this server by changing only the base URL. No billing surface — the hosted
product's Stripe/x402 paths are deliberately absent.

Status: **pre-release, under active construction.** P0 (skeleton, auth, error shapes, control-plane
storage and HTTP) is complete and gate-passed; P1 (control plane) is in its gate. Mail in/out,
drafts, events, domains and migration are planned but unwritten. It is not usable as a mail server
yet.

## Why

It replaces a Stalwart deployment on a single-node k3s cluster with something that speaks a
documented agent-facing HTTP API instead of JMAP, while keeping the same domains, DKIM keys and
public IP.

## Design in one paragraph

Postgres via `sqlx` for everything transactional; content-addressed filesystem blobs behind a
trait; axum + tower for HTTP; an SMTP daemon built from the `stalwart-labs` standalone parser
crates (`smtp-proto`, `mail-parser`, `mail-auth`, `mail-send`) rather than a general-purpose mail
server; an in-process Svix-wire-compatible webhook engine. Threading follows the RFC 5322
Message-ID reference chain, per inbox — subject is never a grouping key, which was measured rather
than assumed.

**Every wire type, storage model and identifier derives from AgentMail's own artifacts** —
`reference/openapi.json`, the official SDKs, and the live captures in `reference/fixtures/`. Never
from Stalwart or JMAP, not even as an optional field. That rule is enforced by
`scripts/shape-provenance.sh` and a write-time hook, not by convention.

## Layout

```
crates/amk-types     wire types, error envelope, ids          (written first, alone)
crates/amk-core      scope, permissions, labels, threading     (the security boundary)
crates/amk-store     sqlx, migrations, keyset pagination
crates/amk-http      axum router, auth layer, handlers
crates/amk-cli       `amk` (init|migrate|doctor), `amkd --role api`
conformance/         dual-target structural diff vs the live reference API
reference/fixtures/  live captures — the contract, and the regression suite
docs/PLAN.md         the full plan, phase gates and open registers
```

## Build and verify

Requires Rust 1.85+. Postgres 17 is needed for the storage and HTTP integration tests.

```bash
./scripts/dev-db.sh up      # Postgres on 127.0.0.1:55432 (needs Docker)
./scripts/check.sh          # fmt + clippy + tests + shape provenance + plan ledger
```

`check.sh` is the only verify command; the Stop hook runs `--fast` (same, minus clippy). Without a
reachable database it still exits PASS having **skipped** the DB-backed tests, and says so on one
line — read that line.

## Evidence discipline

`reference/fixtures/` holds unmodified request/response captures from the live AgentMail API. They
are not documentation: `crates/amk-types/tests/fixtures.rs` reads them at test time, and a capture
added without being wired in fails the build. Where a capture and the published spec disagree, the
capture wins — that has happened five times so far, including `openapi.json` being wrong on all
three DELETE status codes and understating the permission flag count by two.

No credential, DKIM private key or API key appears in this repository. The DKIM fixture records
key *metadata* only; the `am_us_000…` strings are a deliberately-invalid probe key.

## Contributing

This is a single-operator project with no CI — gating runs locally via `scripts/check.sh` and the
hooks in `scripts/hooks/`. Outside contributions are not being accepted while the API surface is
still being derived.

## License

AGPL-3.0-or-later.

AgentMail is a third-party product; this project is not affiliated with or endorsed by it, and
compatibility is derived from its public API artifacts and documentation.
