//! Shared scaffolding for amk-cli's DB-touching integration tests.
//!
//! `amk_store::organizations::exists` — the guard `amk init` refuses a second run on — is a bare
//! "does any organization row exist at all" check with no scope of its own. Every other crate's
//! integration suite shares the one dev database (`amk`, `postgres://amk:amk-dev-local@
//! 127.0.0.1:55432/amk`) and seeds it with uniquely-named rows so concurrent tests never collide
//! on an id — but `exists` does not care about ids, only whether the `organizations` table has
//! any row in it, so that sharing scheme cannot give `amk init`'s own tests a database `exists`
//! reports as empty. A test asserting "against a fresh database" that runs against a table other
//! suites are concurrently inserting into is not testing what it claims to, and would be exactly
//! the kind of nondeterministic, order-dependent test this project's own history (see the project
//! CLAUDE.md's keyset-tiebreak note) singles out as a defect in itself.
//!
//! So: every test that needs `exists` to genuinely be `false` creates its own throwaway
//! Postgres database on the same instance, migrates it fresh via `amk_store::connect`, and drops
//! it when done. The `amk` role `scripts/dev-db.sh` provisions is a full superuser (`CREATEDB`
//! included), so this needs no privilege this checkout doesn't already grant itself.

#![allow(dead_code)] // not every helper is used by every test binary in this integration suite.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

const HOST: &str = "127.0.0.1:55432";
const ADMIN_DSN: &str = "postgres://amk:amk-dev-local@127.0.0.1:55432/postgres";

fn require_db() -> bool {
    std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1")
}

async fn admin_pool_or_skip(test_name: &str) -> Option<PgPool> {
    match PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(ADMIN_DSN)
        .await
    {
        Ok(p) => Some(p),
        Err(e) => {
            if require_db() {
                panic!(
                    "AMK_REQUIRE_DB=1 but the dev database is unreachable ({e}) in {test_name}. \
                     Run `./scripts/dev-db.sh up`, or unset AMK_REQUIRE_DB to allow this suite to \
                     skip its database-backed tests."
                );
            }
            eprintln!("skipping {test_name}: dev database unreachable ({e})");
            None
        }
    }
}

/// A freshly created, freshly migrated, guaranteed-empty database — `organizations::exists`
/// reports `false` against it until a test inserts into it. Dropped by [`FreshDb::drop_it`].
pub struct FreshDb {
    name: String,
    pub pool: PgPool,
}

impl FreshDb {
    /// `None` (or panic under `AMK_REQUIRE_DB=1`) when the dev database is unreachable — every
    /// DB-touching test in this crate must skip cleanly, mirroring every other crate's own rule
    /// (`amk-store/tests/support/mod.rs`, `amk-http/tests/support/mod.rs`).
    pub async fn create(test_name: &str) -> Option<Self> {
        let admin = admin_pool_or_skip(test_name).await?;
        let name = format!("amk_cli_test_{}", Uuid::new_v4().simple());
        // `CREATE DATABASE` cannot take a bind parameter for the identifier — Postgres does not
        // allow it there. `AssertSqlSafe` is warranted here specifically because `name` is never
        // caller/user input: it is this function's own `Uuid::new_v4().simple()` output, which is
        // ASCII hex and cannot contain a `"` to break out of the quoted identifier.
        sqlx::query(sqlx::AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("creating throwaway database {name} for {test_name}: {e}"));
        admin.close().await;

        let dsn = format!("postgres://amk:amk-dev-local@{HOST}/{name}");
        let pool = amk_store::connect(&dsn)
            .await
            .unwrap_or_else(|e| panic!("migrating throwaway database {name} for {test_name}: {e}"));
        Some(Self { name, pool })
    }

    /// Best-effort teardown, called explicitly at the end of a test rather than from `Drop`:
    /// dropping a database is async, and a test that panics before reaching this simply leaks one
    /// throwaway database (named, findable, harmless) rather than failing a second time inside a
    /// destructor.
    pub async fn drop_it(self) {
        let name = self.name;
        self.pool.close().await;
        if let Some(admin) = admin_pool_or_skip(&format!("drop {name}")).await {
            // `WITH (FORCE)` (Postgres 13+): terminates any straggling connection to the target
            // database rather than failing the DROP on one. This test's own pool was already
            // closed above; FORCE is defence against a connection this helper doesn't know about
            // (e.g. one still winding down), not a substitute for closing it.
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#
            )))
            .execute(&admin)
            .await;
        }
    }
}
