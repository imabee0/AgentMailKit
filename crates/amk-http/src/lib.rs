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

use axum::routing::{delete, get};
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

/// Build the router for this dispatch's 25 operations. Three mounts share one handler set per
/// collection (see each `handlers::*` module) — every `*_org`/`*_pod`/`*_inbox` sibling calls the
/// same shared inner function, so the routing table below is the only place mount and path are
/// spelled out.
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
        // ---- api-keys (9) — the one collection mounted all three ways ----
        .route(
            "/v0/api-keys",
            get(handlers::api_keys::list_org).post(handlers::api_keys::create_org),
        )
        .route("/v0/api-keys/{api_key_id}", delete(handlers::api_keys::delete_org))
        .route(
            "/v0/pods/{pod_id}/api-keys",
            get(handlers::api_keys::list_pod).post(handlers::api_keys::create_pod),
        )
        .route(
            "/v0/pods/{pod_id}/api-keys/{api_key_id}",
            delete(handlers::api_keys::delete_pod),
        )
        .route(
            "/v0/inboxes/{inbox_id}/api-keys",
            get(handlers::api_keys::list_inbox).post(handlers::api_keys::create_inbox),
        )
        .route(
            "/v0/inboxes/{inbox_id}/api-keys/{api_key_id}",
            delete(handlers::api_keys::delete_inbox),
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
///
/// The explicit `with_fix` is divergence 2 (`reference/fixtures/25-p1-gate-conformance.txt`):
/// `GET /v0/no-such-route` carries a route-specific `fix` live ("No route matches this path and
/// HTTP method...") — a different sentence from `error::fix_for`'s generic `NotFound` default,
/// which exists for a resource lookup, not a route. Overriding it here, once, at the one call
/// site that means "no route", is cheaper and more honest than teaching the generic default to
/// guess which of the two situations produced it.
async fn not_found_fallback() -> AppError {
    AppError::new(amk_types::ErrorCode::NotFound, "Route not found").with_fix(
        "No route matches this path and HTTP method; check the URL and the HTTP verb against \
         the documented operations.",
    )
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

    /// Every `permissions::require(&ctx.grants, "…")` literal across `handlers/{pods,inboxes,
    /// api_keys}.rs` must be a real flag `amk_core::permissions::is_known_flag` recognizes. A
    /// typo here does not fail to compile — `require`'s second argument is `&'static str` — it
    /// silently denies the gated operation to every caller forever, with no test able to tell the
    /// difference between "correctly gated" and "gated on a name nothing ever grants".
    ///
    /// The literals are extracted from the handler sources themselves at test time
    /// (`include_str!`, re-read on every rebuild — no build script, no hand-kept list to fall out
    /// of date) rather than hand-copied: a future handler adding or renaming a gate is covered
    /// automatically, and a typo'd flag fails this test instead of silently denying forever.
    #[test]
    fn every_permission_flag_literal_used_by_a_handler_is_a_real_flag() {
        const NEEDLE: &str = "permissions::require(&ctx.grants, \"";
        const SOURCES: &[(&str, &str)] = &[
            ("handlers/pods.rs", include_str!("handlers/pods.rs")),
            ("handlers/inboxes.rs", include_str!("handlers/inboxes.rs")),
            ("handlers/api_keys.rs", include_str!("handlers/api_keys.rs")),
        ];

        let mut checked = 0usize;
        for (path, source) in SOURCES {
            let mut rest = *source;
            while let Some(start) = rest.find(NEEDLE) {
                let after_needle = &rest[start + NEEDLE.len()..];
                let end = after_needle
                    .find('"')
                    .expect("permissions::require's flag literal must be a terminated string");
                let flag = &after_needle[..end];
                assert!(
                    amk_core::permissions::is_known_flag(flag),
                    "{path}: {flag:?} is not a real ApiKeyPermissions flag — a handler is \
                     gating on a typo"
                );
                checked += 1;
                rest = &after_needle[end + 1..];
            }
        }
        // A silently-empty scan (e.g. the call shape changed and the needle no longer matches
        // anything) would make this test pass for the wrong reason — assert it actually found the
        // gates this crate is known to have.
        assert!(
            checked >= 10,
            "expected at least 10 permissions::require call sites across pods/inboxes/api_keys, \
             found {checked} — has the call shape changed and the NEEDLE above gone stale?"
        );
    }
}
