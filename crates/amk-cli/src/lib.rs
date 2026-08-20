//! Library half of `amk` and `amkd` — argument parsing, environment loading, connection-failure
//! redaction, and the commands themselves. Each `src/bin/*.rs` stays a thin translation from real
//! argv/env to these functions, so everything here is unit-testable directly (see each module's
//! own tests) and `tests/` drives the compiled binaries as real subprocesses for the edge cases
//! that are about actual process output (argv parsing, and anything env-var-shaped — see
//! `crate::config`'s own note on why environment-dependent behaviour is tested that way rather
//! than by mutating `std::env` in-process).
//!
//! # Shape provenance
//!
//! This crate defines no wire type, no storage model, and writes no SQL. Every value it passes to
//! `amk-store`/`amk-http` is one of their own types; every value that reaches an operator's
//! terminal is a plain string this crate formats itself. Nothing here may model a Stalwart or
//! JMAP concept.
//!
//! # The one security requirement of this crate
//!
//! **`AMK_DATABASE_URL`'s value and the root key's plaintext must never reach a log, an error
//! message, or a file.** See [`redact`] for the DSN half and `commands::init`'s own doc for the
//! root key half (printed to stdout exactly once, by `src/bin/amk.rs`, and nowhere else).

pub mod args;
pub mod commands;
pub mod config;
pub mod logging;
pub mod redact;
pub mod server;

/// Exit codes shared by both binaries. `0` success; `2` a usage error (bad argv — always paired
/// with a message from `crate::args`); `1` everything else (a fail-closed runtime error: a
/// missing variable, a connection failure, an already-initialised deployment, a role that parses
/// but is not implemented yet). `--help` is the only zero-exit outcome that is not success in the
/// "did the requested work" sense, and it uses `OK` deliberately — printing help on request is
/// not a failure.
pub mod exit {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 2;
    pub const FAILURE: i32 = 1;
}
