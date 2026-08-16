//! **Never print a `sqlx::Error` (or anything that wraps one) verbatim.**
//!
//! `AMK_DATABASE_URL` carries a password, and `sqlx::Error`'s `Display` can carry the connection
//! string a failure happened against (`Configuration`'s inner error in particular — the DSN parse
//! failure it wraps is built from the input string). Every place in this crate that might
//! otherwise format a connection failure routes through [`describe_connect_failure`] instead,
//! which classifies the failure by *variant only* and never touches the error's own rendered
//! text, its `source()`, or any value it might carry.
//!
//! [`amk_store::StoreError::Database`] wraps a `sqlx::Error` the exact same way, so
//! [`describe_store_failure`] gives it the identical treatment rather than falling through to
//! `StoreError`'s own `Display` (which does format the wrapped error's text, via `#[error(
//! "database error: {0}")]` — safe once a connection is already established and query-level, but
//! this module does not assume that; it is cheaper to be uniform than to be right about it in one
//! call site and wrong in the next).

use amk_store::StoreError;

/// A safe, DSN-free description of why connecting failed. Names the failure *kind* only — the
/// caller names which variable was being used (`AMK_DATABASE_URL` today; the parameter exists so
/// a future second DSN-bearing variable does not have to duplicate this module).
pub fn describe_connect_failure(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Configuration(_) => {
            "the value is not a well-formed Postgres connection string"
        }
        sqlx::Error::Io(_) => {
            "an I/O error while talking to the database (host unreachable, connection refused, \
             or similar)"
        }
        sqlx::Error::Tls(_) => "a TLS error while establishing the connection",
        sqlx::Error::PoolTimedOut => {
            "timed out waiting for a connection (the host may be unreachable, or the port or \
             database name may be wrong)"
        }
        sqlx::Error::PoolClosed => {
            "the connection pool was closed before a connection could be made"
        }
        sqlx::Error::WorkerCrashed => "the connection worker crashed",
        sqlx::Error::Database(_) => "the database rejected the connection",
        sqlx::Error::Migrate(_) => "connected, but applying an embedded migration failed",
        // `sqlx::Error` is `#[non_exhaustive]`: a future sqlx upgrade can add a variant this match
        // has never seen. Fail closed to the same generic, DSN-free wording rather than refusing
        // to compile or (worse) reaching for the unmatched variant's own `Display`.
        _ => "an unexpected error occurred while connecting",
    }
}

/// The store-layer counterpart of [`describe_connect_failure`] — every [`StoreError`] variant's
/// own `Display` is safe to show as-is *except* [`StoreError::Database`], which this delegates to
/// [`describe_connect_failure`] instead of formatting directly.
pub fn describe_store_failure(err: &StoreError) -> String {
    match err {
        StoreError::Database(inner) => describe_connect_failure(inner).to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// Every unit-constructible `sqlx::Error` variant, run through [`describe_connect_failure`]
    /// with a recognisable payload embedded in the error it wraps — none of the payload may
    /// appear in the classification, because the classification must never format the wrapped
    /// error at all, whatever its own `Display` says.
    const SECRET: &str = "hunter2-a-recognisable-password-payload";

    #[test]
    fn configuration_failure_never_echoes_its_inner_error() {
        let err = sqlx::Error::Configuration(Box::new(io::Error::other(SECRET)));
        let msg = describe_connect_failure(&err);
        assert!(!msg.contains(SECRET), "leaked the inner configuration error: {msg:?}");
    }

    #[test]
    fn io_failure_never_echoes_its_inner_error() {
        let err = sqlx::Error::Io(io::Error::other(SECRET));
        let msg = describe_connect_failure(&err);
        assert!(!msg.contains(SECRET), "leaked the inner io error: {msg:?}");
    }

    #[test]
    fn tls_failure_never_echoes_its_inner_error() {
        let err = sqlx::Error::Tls(Box::new(io::Error::other(SECRET)));
        let msg = describe_connect_failure(&err);
        assert!(!msg.contains(SECRET), "leaked the inner tls error: {msg:?}");
    }

    /// The unit-variant failure kinds each still classify to a distinct, non-empty message — not
    /// load-bearing for secrecy (they carry no payload), but pinning them means a future variant
    /// added to the match can't silently fall through to the same wording as its neighbours.
    #[test]
    fn unit_variants_classify_distinctly() {
        let pool_timed_out = describe_connect_failure(&sqlx::Error::PoolTimedOut);
        let pool_closed = describe_connect_failure(&sqlx::Error::PoolClosed);
        let worker_crashed = describe_connect_failure(&sqlx::Error::WorkerCrashed);
        assert_ne!(pool_timed_out, pool_closed);
        assert_ne!(pool_closed, worker_crashed);
        assert_ne!(pool_timed_out, worker_crashed);
    }

    #[test]
    fn store_error_database_variant_delegates_to_connect_classification() {
        let store_err = StoreError::Database(sqlx::Error::Io(io::Error::other(SECRET)));
        let msg = describe_store_failure(&store_err);
        assert!(!msg.contains(SECRET), "leaked the inner io error through StoreError: {msg:?}");
    }

    /// Every other `StoreError` variant carries no secret and its own `Display` is fine to show —
    /// this pins that [`describe_store_failure`] does not accidentally swallow or blank those
    /// messages while adding the `Database` special case.
    #[test]
    fn non_database_store_errors_pass_through_their_own_display() {
        let err = StoreError::InvalidValue("name");
        assert_eq!(describe_store_failure(&err), err.to_string());
    }
}
