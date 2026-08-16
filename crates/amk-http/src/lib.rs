//! The axum HTTP surface: the tower auth layer, scope resolution into handlers, the error
//! envelope, pagination, and the P0/P1 handlers (25 operations — see
//! `.claude/contracts/amk-http.md`).
//!
//! # This crate ships a `Router`, not a binary
//!
//! [`router`] returns an `axum::Router` plus the [`AppState`] it needs. **No `main`, no
//! `[[bin]]`, no port binding** — that is a decision, not an omission (the dispatch contract's own
//! section explains why): `amk` and `amkd` are the next dispatch. Tests drive the router
//! in-process via `tower::ServiceExt::oneshot`/axum's `Router::into_service`.
//!
//! # Shape provenance
//!
//! Every wire type comes from `amk-types`; every persistence call goes through `amk-store`'s
//! public interface — no SQL in this crate. Nothing here may model a Stalwart or JMAP concept.

pub mod config;

use axum::Router;
use sqlx::PgPool;

pub use config::AppConfig;

/// Everything a handler needs beyond the request itself: the database pool and deployment
/// configuration. `Clone` is cheap — `PgPool` is reference-counted internally, and `AppConfig`
/// carries two `Option<String>`s.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
}

impl AppState {
    pub fn new(pool: PgPool, config: AppConfig) -> Self {
        Self { pool, config }
    }
}

/// Build the router. Grows as the crate's modules land — this stage wires no routes yet.
pub fn router(state: AppState) -> Router {
    Router::new().with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A router must actually build without a live database — construction is synchronous and
    /// does not touch `state.pool`. `PgPool::connect_lazy` itself needs a Tokio context to exist
    /// (it schedules background pool-maintenance tasks even though it makes no connection yet),
    /// hence `#[tokio::test]` rather than a bare `#[test]`.
    #[tokio::test]
    async fn router_builds_without_touching_the_database() {
        let pool = PgPool::connect_lazy("postgres://amk:amk-dev-local@127.0.0.1:55432/amk")
            .expect("lazy connect never touches the network");
        let state = AppState::new(pool, AppConfig::default());
        let _ = router(state);
    }
}
