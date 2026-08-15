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
