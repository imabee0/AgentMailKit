# amk-store, second dispatch — api-keys — dispatch contract

Written by the orchestrator before dispatch. The design decisions here are settled; the
implementer resolves ordinary coding detail inside them and escalates anything else.

## Why this dispatch exists (read this first)

The first `amk-store` dispatch was deliberately narrowed to migrations, the pool, error mapping,
the four P1 control-plane repositories, keyset pagination, and the message/thread read path. Its
deferral list named blob store, FTS/search, signed downloads, jobs and idempotency as a second
dispatch — **and did not name api-keys at all.** That was a gap, not a decision.

It surfaced when the `amk-http` contract was checked against what `amk-store` actually exposes:
`amk-http`'s tower auth layer requires "O(1) lookup by key id then a **constant-time** verify of an
argon2id hash", and there is no `api_keys` table, no repository, and no hash anywhere in the crate.
`amk init` (P0: "default org+pod, root key shown once") has the same dependency. So **`amk-http`
cannot start until this lands** — the write order is not advisory, and an implementer that hit this
would have had to invent a storage shape, which rule 3 forbids.

## Writable paths (exact)

`crates/amk-store/**`, plus the workspace `Cargo.lock` **only** as the automatic consequence of
adding a dependency. Nothing else. If the work requires a path outside that tree — including
`amk-types`, which is frozen — **STOP and report** rather than widening scope.

`Cargo.lock` was missing from this clause on the first pass and the omission was real: adding a
dependency necessarily rewrites the root lockfile, and **the scope hook never saw it** — the guard
is a `PreToolUse` hook on `Write`/`Edit`/`Bash`, so it observes what an agent writes and is
structurally blind to what cargo writes. That left the implementer choosing between violating its
stated scope and being unable to do what this contract asked. Committing lockfiles is a project
rule; so the contract names it.

## `[SPEC:*]` citations governing every shape here

- `[SPEC:openapi]` `reference/openapi.json`, `type_api-keys:ApiKey`, `CreateApiKeyRequest`,
  `CreateApiKeyResponse`, `ApiKeyPermissions` — already modelled in `amk_types::api_key`. **Use
  those types; do not restate them.** Of the 9 `/v0/**/api-keys*` paths, **6** are this dispatch's
  subject — create + list + delete-by-id at each of the org, pod and inbox mounts. The other 3
  (`/v0/api-keys/public-keys`, `…/public-keys/{api_key_id}` PATCH, `…/agentid-sign-in/revoke-all`)
  are the **AgentID P-256 public-key** feature, which is a different credential kind entirely and
  is parked until after V1. **Out of scope — do not build a table, column or enum variant for it.**
- `[SPEC:openapi]` `ApiKeyPermissions` has **36** boolean flags. `amk_types::api_key` owns them and
  `KeyGrants::from_wire` owns their absent/empty semantics. Never define a second representation —
  two modules independently defining these flags is the exact fan-out collision that cost this
  project a review round.
- `[TESTED]` `reference/fixtures/01-auth-me.http` — the `Identity` an authenticated key resolves to:
  an org-scoped key reports `scope_id == organization_id` and carries **no** `pod_id`/`inbox_id`.
  Your scope columns must be able to reproduce that exactly, for all three scope types.
- `[TESTED]` `reference/fixtures/18-inbox-case-normalization.txt` — an inbox-scoped key's
  `inbox_id` folds ASCII case like every other `inbox_id`. Compare with `InboxId::eq_normalized`,
  never `==` on the raw value.
- `[TESTED]` `reference/fixtures/05-error-catalog.http` — a well-formed but unknown key gets the
  **bare** `{"message":"Forbidden"}` 403. That response is `amk-http`'s to build; what it means for
  you is that "key not found" and "key found, hash mismatch" must be **indistinguishable** to the
  caller and must cost the same time.
- `[TESTED]` `reference/fixtures/05-error-catalog.http:6` — the **only key shape this project has
  ever seen the reference API treat as well-formed**: `am_us_` followed by 32 characters. It drew
  the bare 403 reserved for a well-formed-but-unknown key rather than a malformed-credential
  response. That is thin evidence for a format, but it is the evidence there is, and it is what the
  minting rule below is built on.
- `[UNVERIFIED]` — the node SDK is recorded in the plan as routing an **`am_eu_`-prefixed** key to
  AgentMail's EU host (`environments.ts`, `Client.ts:80`), read at planning time. **That source is
  NOT vendored under `reference/`, so the claim cannot be checked from this repository** — the
  review panel caught the contract citing it as `[SPEC:sdk]`, which overstated it. It is downgraded
  rather than deleted because the rule it produces is fail-closed and costs nothing: never minting
  an `am_eu_` key is correct whether or not the SDK dispatches on that prefix. Do not treat the
  citation as verified evidence, and do not build anything else on it.

## The rule that governs every other decision

**No fixture shows a real AgentMail API key** — none was ever created against the reference
account, and `amk_types::api_key`'s own doc comment records that this type is `[SPEC:openapi]`
only. So: the wire *shapes* are pinned and you must match them exactly, while the *secret's own
format* is ours to choose within the one constraint the evidence imposes. Mark that choice
`[ASSUMED]` in a doc comment with its reasoning. Do not mark anything else assumed, and do not add
a field no artifact shows — `ApiKey` deliberately omits `organization_id` for that reason even
though sibling resources carry it.

## Decisions (settled — implement, do not relitigate)

### Migration `0007_api_keys.sql`

- `api_key_id uuid PRIMARY KEY`, `organization_id` NOT NULL FK, `pod_id` NULL FK, `inbox_id` NULL
  FK, `name text NOT NULL`, `prefix text NOT NULL UNIQUE`, `hash text NOT NULL`,
  `permissions jsonb NULL`, `used_at timestamptz NULL`, `created_at timestamptz NOT NULL`.
- **`permissions` must be nullable and the nullability is load-bearing**: SQL `NULL` is the absent
  object (grants everything) and `'{}'::jsonb` is the present-but-empty one (grants nothing).
  Collapsing them is a privilege bug in both directions. Assert both round-trip in a test.
- Scope is derived, not stored as an enum: both null → org, `pod_id` set → pod, `inbox_id` set →
  inbox. A row with both `pod_id` and `inbox_id` set is **rejected by a CHECK constraint**, because
  it has no representation in `Identity`.
- Unique index on `prefix` is the O(1) lookup path. Follow the existing migrations' idiom for FKs
  and ON DELETE behaviour — deleting a pod must not orphan its keys.

### Minting and verification

- **Format (`[ASSUMED]`):** `am_us_` followed by 32 characters of URL-safe random.
  **Not** bare `am_` + random, which was this contract's first answer and was wrong: the review
  panel pointed at `reference/fixtures/05-error-catalog.http:6`, where
  `am_us_00000000000000000000000000000000` drew the bare 403 reserved for a *well-formed* unknown
  key. So the reference API's keys carry a **region segment**, and a bare `am_` + random neither
  matches the one shape we have seen accepted nor gains anything. Reproduce the observed shape.
  Two consequences, both of which you write as tests rather than comments:
  - a minted key **never** begins `am_eu_` — satisfied automatically by the `am_us_` prefix, and
    asserted anyway so that changing the prefix later cannot silently break it. The reason is the
    `[UNVERIFIED]` EU-routing note above; the assertion is cheap and fail-closed either way.
  - the random portion comes from a **CSPRNG**, not a general-purpose RNG, and is 32 characters.
- `prefix` is the leading identifying segment, stored in clear and safe to display; the remainder
  is the secret. Never store, log, or return the secret except in `CreateApiKeyResponse`, which
  exists precisely so the secret is unrepresentable outside creation.
- **argon2id lives here, not in `amk-http`.** The hash must never leave this crate, and exactly one
  place should know the parameters. `create` takes the plaintext once and returns the
  `CreateApiKeyResponse`; `authenticate(pool, presented) -> Option<…>` does prefix lookup then a
  **constant-time** verify.
- `authenticate` **must not write.** Do not update `used_at` on the auth path — expose a separate
  `touch_used_at` and let `amk-http` decide when to call it. An auth hot path that writes on every
  request is a different design with different failure modes, and it is not the one chosen.
- Unknown prefix and bad secret must take the same code path shape: verify against a dummy hash
  when the prefix misses, so the miss is not measurably faster. Timing is the leak here.

### Repository surface

`create`, `get`, `list`, `delete`, `authenticate`, `touch_used_at` — matching the existing modules'
signatures and error mapping (`StoreError`, `Result<bool>` on delete for found/not-found). Scope
every query by the caller's scope the way `messages.rs` and `threads.rs` already pin theirs; the
pod pin in those modules is the pattern to copy, and it is there because a missing pin is a
cross-pod read.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export. (A hook blocks this at write time.)
- No JMAP, Sieve, RocksDB, or mailbox-role concept.
- Do not edit `amk-types`, `amk-core`, the plan, or any contract file. If a type or field you need
  does not exist, **STOP and report** — do not add a field that obviously belongs.
- No second definition of the 36 permission flags, in any form, including a string list.
- No billing surface: no plan, price, quota-upsell string, or `upgrade_url`.
- Never write a key, hash parameter, or secret into a fixture, test snapshot, or commit.

## Assigned edge cases (write the test before the code it targets)

- `permissions` NULL vs `'{}'` round-trip, and that `KeyGrants::from_wire` reads each back with the
  opposite verdict.
- A minted key matches `am_us_` + 32 chars, never begins `am_eu_`, and two successive mints differ;
  the minted secret is not recoverable from a stored row (assert the row contains neither the
  secret nor any prefix of it beyond `prefix` itself).
- `authenticate` with: an unknown prefix; a known prefix and a wrong secret; a known prefix and the
  right secret; a presented value with no prefix separator at all; an empty string; a value whose
  prefix matches but which is longer than any key ever minted.
- Two keys can never share a prefix — assert the unique index actually fires.
- An inbox-scoped key created with `AMKCASE@…` authenticates and resolves the same inbox as
  `amkcase@…` (fixture 18).
- A row with both `pod_id` and `inbox_id` is rejected by the database, not merely by Rust.
- Deleting a pod that owns keys behaves as the FK declares — assert the declared behaviour.
- Listing keys at one pod never returns another pod's, and listing at org scope does not leak a
  sibling org's (the cross-scope test `messages.rs` already models).

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-store`, `./scripts/check.sh`,
and the mutation table required at every phase gate — **`cargo-mutants` does not mutate string
literals, so every SQL scope pin and predicate must be mutated by hand.** "Tests pass" without the
output is not a report. Name anything you did not do and why.
