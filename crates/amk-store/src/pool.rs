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
