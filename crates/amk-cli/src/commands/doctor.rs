//! `amk doctor` — read-only deployment diagnostics. Every line is safe to paste into a chat
//! window: no configuration value ever appears, only whether each variable is set, and no raw
//! `sqlx::Error`/`StoreError` is ever formatted directly (see `crate::redact`).
//!
//! # Why this parses the DSN itself
//!
//! `[TESTED]` against this workspace (dispatch contract, 2026-08-16): `PgPoolOptions::connect`
//! defers parsing the connection string, so a DSN that is not a URL at all and a DSN with an
//! unroutable/unreachable host surface identically — `sqlx::Error::PoolTimedOut` after the pool's
//! five-second `acquire_timeout` (`amk_store::pool::connect`/`connect_unmigrated`). An operator
//! reading "timed out" for a typo'd DSN would restart a database that was never the problem.
//! `doctor` exists to tell those two apart, so it parses `AMK_DATABASE_URL` with
//! `.parse::<sqlx::postgres::PgConnectOptions>()` **before** ever attempting to connect —
//! see `parses_a_malformed_dsn_as_malformed_without_waiting_on_a_connection_attempt` below for the
//! specific case this was written to catch.

use amk_store::MigrationStatus;
use sqlx::postgres::PgConnectOptions;

use crate::config::VarPresence;
use crate::redact::describe_connect_failure;

/// Everything `doctor` reads from the environment, taken as explicit arguments rather than read
/// directly — this keeps the function itself free of any `std::env` access, so it is testable
/// without touching real process environment (which `cargo test`'s default parallelism makes
/// unsafe to mutate from more than one test — see `crate::config`'s own note on this).
pub struct DoctorInputs {
    pub vars: Vec<VarPresence>,
    pub database_url: Option<String>,
}

/// One diagnostic report, safe to paste — see the module doc. `to_text` is how both the `amk`
/// binary and this module's own tests read it, so there is exactly one rendering to check against.
pub struct DoctorReport {
    pub lines: Vec<String>,
}

impl DoctorReport {
    pub fn to_text(&self) -> String {
        self.lines.join("\n")
    }
}

pub async fn run(inputs: DoctorInputs) -> DoctorReport {
    let mut lines = Vec::new();

    for var in &inputs.vars {
        lines.push(format!("{}: {}", var.name, if var.set { "set" } else { "unset" }));
    }

    let Some(url) = inputs.database_url else {
        lines.push("database: AMK_DATABASE_URL is not set -- nothing further to check".to_owned());
        return DoctorReport { lines };
    };

    if url.parse::<PgConnectOptions>().is_err() {
        lines.push(
            "database DSN: MALFORMED -- AMK_DATABASE_URL does not parse as a Postgres connection \
             string"
                .to_owned(),
        );
        return DoctorReport { lines };
    }
    lines.push("database DSN: parses as a well-formed Postgres connection string".to_owned());

    match amk_store::connect_unmigrated(&url).await {
        Ok(pool) => {
            lines.push("database: reachable".to_owned());
            lines.push(migration_status_line(amk_store::migration_status(&pool).await));
        }
        Err(e) => {
            lines.push(format!("database: UNREACHABLE ({})", describe_connect_failure(&e)));
        }
    }

    DoctorReport { lines }
}

fn migration_line(status: MigrationStatus) -> String {
    let current = if status.is_current() {
        "current"
    } else {
        "NOT current"
    };
    format!("migrations: {} of {} applied ({current})", status.applied, status.embedded)
}

/// Render `amk_store::migration_status`'s outcome as the one report line it produces once the
/// database is reachable — factored out from [`run`] specifically so the failure arm is
/// unit-testable against a synthetic `sqlx::Error`. No live database this crate's tests control
/// can be made to fail *this* query (`SELECT ... FROM _sqlx_migrations`) with anything other than
/// the one error `migration_status` already handles specially (`42P01`, "not migrated yet",
/// `Ok(applied: 0)`, not an `Err` at all): reaching a real "connected, but the ledger read still
/// failed" state deterministically would mean either raw SQL corrupting `amk-store`'s own
/// migration ledger table (past the DDL-provisioning carve-out `tests/support/mod.rs` already
/// documents) or a timing race against a statement timeout (flaky by construction — see the
/// project's own standing rule against seed-dependent tests). Testing the rendering directly
/// against a constructed `Err` values covers the branch this crate can actually own without
/// either.
fn migration_status_line(result: Result<MigrationStatus, sqlx::Error>) -> String {
    match result {
        Ok(status) => migration_line(status),
        Err(e) => {
            format!(
                "migrations: could not read the migration ledger ({})",
                describe_connect_failure(&e)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(name: &'static str, set: bool) -> VarPresence {
        VarPresence { name, set }
    }

    // ---- migration_status_line: the connected-but-ledger-read-failed arm --------------------

    #[test]
    fn a_migration_ledger_read_failure_is_redacted_not_the_raw_sqlx_error() {
        const SECRET: &str = "amk-cli-doctor-migration-ledger-sentinel";
        let err = sqlx::Error::Io(std::io::Error::other(SECRET));
        let line = migration_status_line(Err(err));
        assert!(
            !line.contains(SECRET),
            "leaked the raw sqlx::Error into doctor's report: {line}"
        );
        assert!(line.contains("could not read the migration ledger"), "got: {line}");
    }

    #[test]
    fn a_migration_ledger_read_success_reports_the_counts() {
        let line = migration_status_line(Ok(MigrationStatus { applied: 8, embedded: 8 }));
        assert!(line.contains("8 of 8"));
        assert!(line.contains("(current)"));
    }

    // ---- the parse-before-connect claim, checked directly against this workspace's sqlx pin ---

    #[test]
    fn a_string_with_no_url_scheme_at_all_fails_to_parse() {
        assert!(
            "not-a-url-at-all".parse::<PgConnectOptions>().is_err(),
            "doctor's malformed-DSN branch depends on this failing to parse"
        );
    }

    #[test]
    fn a_well_formed_url_parses_regardless_of_scheme() {
        // [TESTED] against sqlx-postgres 0.9.0's `FromStr` impl (`options/parse.rs`): it parses
        // via the generic `url` crate and never inspects `Url::scheme()`, so a syntactically
        // valid URL with the wrong scheme (e.g. a MySQL DSN pointed at this Postgres deployment
        // by mistake) is NOT caught by the parse step -- only genuinely malformed input is.
        // Recorded here so a future sqlx upgrade that starts validating the scheme is a visible
        // test change, not a silent behaviour change in `doctor`.
        assert!("mysql://user:pass@localhost/db"
            .parse::<PgConnectOptions>()
            .is_ok());
    }

    // ---- report shape ---------------------------------------------------------------------

    #[tokio::test]
    async fn every_variable_reports_set_or_unset_never_a_value() {
        let inputs = DoctorInputs {
            vars: vec![
                present("AMK_DATABASE_URL", true),
                present("AMK_BIND", false),
                present("AMK_PRIMARY_DOMAIN", true),
                present("AMK_PRODUCT_NAME", false),
            ],
            database_url: None,
        };
        let report = run(inputs).await;
        let text = report.to_text();
        assert!(text.contains("AMK_DATABASE_URL: set"));
        assert!(text.contains("AMK_BIND: unset"));
        assert!(text.contains("AMK_PRIMARY_DOMAIN: set"));
        assert!(text.contains("AMK_PRODUCT_NAME: unset"));
    }

    #[tokio::test]
    async fn unset_database_url_is_reported_without_attempting_to_parse_or_connect() {
        let inputs =
            DoctorInputs { vars: vec![present("AMK_DATABASE_URL", false)], database_url: None };
        let report = run(inputs).await;
        assert!(report.to_text().contains("is not set"));
    }

    #[tokio::test]
    async fn a_malformed_dsn_is_reported_as_malformed_not_unreachable() {
        let inputs = DoctorInputs {
            vars: vec![present("AMK_DATABASE_URL", true)],
            database_url: Some("not-a-url-at-all".to_owned()),
        };
        let report = run(inputs).await;
        let text = report.to_text();
        assert!(text.contains("MALFORMED"), "expected the malformed branch, got: {text}");
        assert!(
            !text.contains("UNREACHABLE"),
            "malformed and unreachable must stay distinct: {text}"
        );
    }

    #[tokio::test]
    async fn a_well_formed_but_unroutable_dsn_never_leaks_its_password() {
        const SENTINEL: &str = "amk-cli-doctor-sentinel-password";
        // Port 1 on loopback: a well-formed DSN (the parse step must pass it) that fails to
        // connect immediately (connection refused) rather than waiting out the 5s pool timeout,
        // so this test stays fast regardless of whether the dev database is up.
        let dsn = format!("postgres://amk:{SENTINEL}@127.0.0.1:1/amk");
        let inputs =
            DoctorInputs { vars: vec![present("AMK_DATABASE_URL", true)], database_url: Some(dsn) };
        let report = run(inputs).await;
        let text = report.to_text();
        assert!(!text.contains(SENTINEL), "password leaked into doctor's report: {text}");
        assert!(text.contains("UNREACHABLE"), "expected the unreachable branch, got: {text}");
    }
}
