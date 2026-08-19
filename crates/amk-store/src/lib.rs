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
// (organizations, pods, inboxes) plus message/thread read queries. NOT in this slice: the blob
// store, full-text search, signed download URLs, the jobs table, and the idempotency-key layer —
// deferred to a later dispatch, not dropped.
//
// `api_keys` (second dispatch): migration 0007 plus create/get/list/delete/authenticate/
// touch_used_at, now that `amk_types::api_key::ApiKey` exists to build them from. See
// `api_keys.rs`'s own module doc for the minting format and its `[ASSUMED]` reasoning.
//
// http-prereqs (third dispatch, `.claude/contracts/amk-store-http-prereqs.md`): `pods::list`,
// `inboxes::list` and `api_keys::list` become keyset-paginated `Page<T>`, matching
// `messages::list`/`threads::list`; `pods::delete` gains `StoreError::PodNotEmpty` (fixture 22);
// migration 0008 cascades the FKs referencing `inboxes` (deliberately not the ones referencing
// `pods`); the minted-key constants are corrected against fixture 23; `organizations::list` is
// deleted (no wire route, one call site, now rewritten against `organizations::get`).

pub mod api_keys;
pub mod blobs;
pub mod error;
pub mod inboxes;
pub mod messages;
pub mod organizations;
pub mod pagination;
pub mod pods;
pub mod pool;
pub mod threads;

pub use error::{PageTokenError, StoreError};
pub use pagination::{
    ApiKeyCursor, InboxCursor, MessageCursor, Page, PodCursor, SortDirection, ThreadCursor,
};
pub use pool::{connect, connect_unmigrated, migration_status, MigrationStatus};
