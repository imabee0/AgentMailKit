//! Domain logic for AgentMailKit: scope resolution, permission intersection, label rules and
//! threading. Pure logic — no I/O, no database, no HTTP.
//!
//! # Why this crate is the security boundary
//!
//! Scope masking and permission intersection decide whether one tenant can observe another's
//! mail. A mistake here does not fail a test — it silently leaks across pods. Every rule is
//! therefore stated once, here, and every handler defers to it rather than re-deriving it.
//!
//! # Shape provenance
//!
//! Types come from `amk-types` (which derives from AgentMail's own artifacts). Nothing here may
//! model a Stalwart or JMAP concept.

pub mod labels;
pub mod permissions;
pub mod scope;
pub mod threading;
