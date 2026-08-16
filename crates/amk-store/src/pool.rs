//! Connection setup.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Connect and run the embedded migrations.
///
/// The migrator is compiled in (`sqlx::migrate!`), so a deployed binary carries its own schema
/// history and does not read `migrations/` off disk at runtime.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Connect without running migrations — for a caller that manages migration timing itself (e.g.
/// running against a pre-migrated database, or wanting the connection attempt and the migration
/// step to fail distinguishably).
pub async fn connect_unmigrated(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

/// What the embedded migrator has and has not applied to this database.
///
/// `applied` counts rows in sqlx's own `_sqlx_migrations` ledger; `embedded` counts the
/// migrations compiled into this binary. Equal means current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStatus {
    pub applied: usize,
    pub embedded: usize,
}

impl MigrationStatus {
    /// Every embedded migration is applied. A database ahead of the binary (`applied >
    /// embedded` — an older binary against a newer schema) is deliberately **not** current: it is
    /// the more dangerous direction, and reporting it as fine is how a rollback silently runs
    /// against a schema it does not understand.
    pub fn is_current(&self) -> bool {
        self.applied == self.embedded
    }
}

/// Read the migration ledger.
///
/// Lives here rather than in a caller because it needs the `_sqlx_migrations` ledger, which is
/// persistence, and this crate is the one place that owns persistence. A binary asking "is the
/// schema current?" must not reach past this crate to answer, and must not embed a second
/// `sqlx::migrate!` pointed at these same files: two declarations of migration ownership is the
/// "one obligation recorded in exactly one place" rule broken, and the second copy is the one
/// that drifts.
///
/// A database that has never been migrated has no `_sqlx_migrations` table at all. That is
/// reported as `applied: 0`, not as an error — "not migrated yet" is a legitimate state for
/// `amk doctor` to describe, and it is the one a fresh deployment is in. Every **other** database
/// failure propagates: a connection loss reported as "0 applied" would tell an operator their
/// schema is missing when in fact their database is unreachable.
pub async fn migration_status(pool: &PgPool) -> Result<MigrationStatus, sqlx::Error> {
    let embedded = sqlx::migrate!("./migrations").iter().count();
    let applied: i64 =
        match sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(pool)
            .await
        {
            Ok(n) => n,
            // SQLSTATE 42P01: undefined_table. The ledger is created by the first migration run, so
            // its absence means exactly "nothing has been applied here".
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => 0,
            Err(e) => return Err(e),
        };
    Ok(MigrationStatus { applied: applied as usize, embedded })
}
