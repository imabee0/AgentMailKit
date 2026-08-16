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

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod ids;
pub mod pagination;
pub mod scope_ext;
mod words;

use axum::routing::get;
use axum::Router;
use sqlx::PgPool;

pub use config::AppConfig;
pub use error::AppError;

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

/// Build the router. Grows as the crate's modules land.
pub fn router(state: AppState) -> Router {
    Router::new()
        // ---- identity + organization (2) ----
        .route("/v0/auth/me", get(handlers::identity::auth_me))
        .route("/v0/organizations", get(handlers::identity::get_organization))
        // ---- pods (4) — the only mount pods have ----
        .route("/v0/pods", get(handlers::pods::list).post(handlers::pods::create))
        .route("/v0/pods/{pod_id}", get(handlers::pods::get).delete(handlers::pods::delete))
        // ---- inboxes (10) — organization mount + pod mount ----
        .route(
            "/v0/inboxes",
            get(handlers::inboxes::list_org).post(handlers::inboxes::create_org),
        )
        .route(
            "/v0/inboxes/{inbox_id}",
            get(handlers::inboxes::get_org)
                .patch(handlers::inboxes::update_org)
                .delete(handlers::inboxes::delete_org),
        )
        .route(
            "/v0/pods/{pod_id}/inboxes",
            get(handlers::inboxes::list_pod).post(handlers::inboxes::create_pod),
        )
        .route(
            "/v0/pods/{pod_id}/inboxes/{inbox_id}",
            get(handlers::inboxes::get_pod)
                .patch(handlers::inboxes::update_pod)
                .delete(handlers::inboxes::delete_pod),
        )
        // Unknown path -> 404 envelope.
        .fallback(not_found_fallback)
        // A path that exists but with the wrong method -> the SAME 404 envelope, never axum's
        // default 405 — the dispatch contract is explicit: "There is no 405."
        .method_not_allowed_fallback(not_found_fallback)
        .with_state(state)
}

/// `[SPEC:fixture 05-error-catalog.http]`: unknown path or wrong method both answer the full
/// envelope, `code: "not_found"`, HTTP 404 — never axum's default plaintext 405/404 body, and
/// never the bare auth-layer shape (this fires before or independent of auth; a client can learn
/// a route does not exist without presenting any credential at all, matching the live API's own
/// `am_us_...` route-not-found capture in fixture 23: *"Route not found"*).
async fn not_found_fallback() -> AppError {
    AppError::new(amk_types::ErrorCode::NotFound, "Route not found")
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
