//! Persistence for AgentMailKit: Postgres via sqlx, migrations, the blob store, full-text search
//! and signed download URLs. The only crate that talks to the database.
//!
//! # The rule that shapes every query
//!
//! Admission is a **query predicate**, never a post-filter. A row the credential may not see must
//! never be fetched, because filtering an already-fetched page leaks the thing it hides: with
//! `limit=1`, dropping the hidden row from each page returns `count: 0` *with* a
//! `next_page_token`, so walking the cursor counts the hidden mail exactly and the tokens disclose
//! its ids and timestamps. `amk-core` owns the rule and hands this crate the predicate; this crate
//! puts it in the `WHERE` clause. The same applies to scope coordinates — what cannot be fetched
//! cannot leak through a count, a total, or a cursor.
//!
//! # Shape provenance
//!
//! Storage models derive from AgentMail's artifacts, never from Stalwart or JMAP — not even as an
//! optional or legacy column. See `.claude/contracts/amk-store.md` for the dispatch contract.

// Modules are declared by the implementer, from the contract. Nothing is stubbed here: an
// interface invented by the orchestrator ahead of the work is a shape nobody derived from
// evidence, which is the failure mode this project is structured to prevent.
//
// # This dispatch's slice
//
// Migrations, the pool/error types, keyset pagination, and the P1 control-plane repositories
// (organizations, pods, inboxes) plus message/thread read queries. NOT in this slice: the
// `api_keys` repository (see below), the blob store, full-text search, signed download URLs, the
// jobs table, and the idempotency-key layer — deferred to a later dispatch, not dropped.
//
// ## Reported blocker: no `api_keys` repository
//
// `reference/openapi.json` has a full `type_api-keys:ApiKey` / `CreateApiKeyRequest` /
// `CreateApiKeyResponse` schema, but `amk_types::api_key` only ports `ApiKeyPermissions` and
// `KeyGrants` — there is no `ApiKey` wire resource in `amk-types`, and no fixture captured a live
// create/list/get response (the closest is `01-auth-me.http`'s `Identity.api_key_id`, which is
// not the resource itself). Building the table and repository anyway would require inventing the
// secret-hashing scheme, the `prefix` format, and how `pod_id`/`inbox_id` scoping is represented
// — exactly the "two modules disagreeing about the same shape" failure `amk_types::api_key`'s own
// doc comment names as this project's worst recurring defect. Per the dispatch's own prohibition
// ("If you need a type that does not exist, STOP and report"), this crate does not implement
// `api_keys`. It needs `amk-types::api_key::ApiKey` (ported from `openapi.json`) before it can.

pub mod error;
pub mod inboxes;
pub mod organizations;
pub mod pods;
pub mod pool;

pub use error::{PageTokenError, StoreError};
pub use pool::{connect, connect_unmigrated};
