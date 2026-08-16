//! `amk migrate` — apply the embedded migrations (idempotent by construction — the migrator is
//! compiled into `amk-store`, `sqlx::migrate!`) and report the resulting state.
//!
//! This is deliberately thin: `amk_store::connect` both connects *and* runs the embedded
//! migrations, so applying them is a side effect of a successful connect, and
//! `amk_store::migration_status` is the one place the resulting ledger state is read. Neither is
//! reimplemented here — see the dispatch contract's own instruction not to read
//! `_sqlx_migrations` directly or embed a second `sqlx::migrate!`.

use amk_store::MigrationStatus;

use crate::redact::describe_connect_failure;

pub async fn run(database_url: &str) -> Result<MigrationStatus, String> {
    let pool = amk_store::connect(database_url).await.map_err(|e| {
        format!("could not connect using AMK_DATABASE_URL: {}", describe_connect_failure(&e))
    })?;
    amk_store::migration_status(&pool).await.map_err(|e| {
        format!(
            "connected and migrated, but could not read the migration ledger back: {}",
            describe_connect_failure(&e)
        )
    })
}

/// The line `amk migrate` prints on success — shared with `amk doctor`'s own rendering so the two
/// commands describe the same state the same way.
pub fn describe(status: MigrationStatus) -> String {
    let current = if status.is_current() {
        "current"
    } else {
        "NOT current"
    };
    format!("Applied {} of {} migrations ({current}).", status.applied, status.embedded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_current_when_applied_equals_embedded() {
        let text = describe(MigrationStatus { applied: 8, embedded: 8 });
        assert!(text.contains("8 of 8"));
        assert!(text.contains("(current)"));
        assert!(!text.contains("NOT current"));
    }

    #[test]
    fn describe_names_not_current_when_behind() {
        let text = describe(MigrationStatus { applied: 6, embedded: 8 });
        assert!(text.contains("NOT current"));
    }

    /// A DSN whose password appears nowhere in a failed connection's returned message. This is
    /// the specific, security-critical edge case the dispatch contract requires — asserted here
    /// on `migrate`'s own returned `Err` message directly, and again at the process level in
    /// `tests/process.rs` against the real compiled `amk` binary's captured stdout/stderr.
    #[tokio::test]
    async fn a_failed_connection_never_echoes_the_dsn_password() {
        const SENTINEL: &str = "amk-cli-migrate-sentinel-password";
        // Port 1 on loopback: well-formed DSN, fast connection-refused rather than a 5s timeout.
        let dsn = format!("postgres://amk:{SENTINEL}@127.0.0.1:1/amk");
        let err = run(&dsn)
            .await
            .expect_err("port 1 must not accept a Postgres connection");
        assert!(!err.contains(SENTINEL), "password leaked into migrate's error message: {err}");
        assert!(err.contains("AMK_DATABASE_URL"), "message must still name the variable: {err}");
    }
}
