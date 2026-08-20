//! The axum HTTP surface: the tower auth layer, scope resolution into handlers, the error
//! envelope, pagination, and the P0/P1 handlers plus the P2 mail surface (41 operations — see
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
pub mod body;
pub mod config;
pub mod error;
pub mod handlers;
pub mod ids;
pub mod pagination;
pub mod scope_ext;
mod words;

use std::sync::Arc;

use amk_outbound::signing::Keyring;
use amk_outbound::OutboundTransport;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::Router;
use sqlx::PgPool;

pub mod observability;
pub mod ratelimit;

pub use config::AppConfig;
pub use error::AppError;
pub use observability::Metrics;
pub use ratelimit::RateLimiter;

/// Everything a handler needs beyond the request itself: the database pool, deployment
/// configuration, the DKIM keyring, and the outbound transport.
///
/// `Clone` is cheap — `PgPool` and the keyring/`Transport` are reference-counted. [`Self::new`]
/// takes the keyring as an ARGUMENT and derives a live SMTP transport from `config`. Tests that
/// send mail call [`Self::with_outbound`] with a fixture keyring and a
/// [`amk_outbound::RecordingTransport`].
///
/// # Why the keyring is a parameter and not a default
///
/// It used to be neither: `new` called `Keyring::new()` unconditionally, so **every deployment
/// answered every send with `NoSigningKey`** — the crate compiled, all 697 tests passed, three
/// review lenses were clean, and the product could not send mail. Nothing caught it because
/// nothing tested the composed binary; `amk-outbound` was exercised as a library and `amk-http`
/// through handlers that injected their own keyring.
///
/// Making it a parameter is the fix that generalises: a caller must now say what it is doing.
/// `amk-cli` passes `config::dkim_keyring()`; a read-only test passes `Keyring::new()` and means
/// it. `scripts/binary-smoke.sh` is the gate that keeps it honest end to end.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub keyring: Arc<Keyring>,
    pub transport: OutboundTransport,
    /// Shared, lock-free counters behind `GET /metrics`. `Arc` because `AppState` is cloned per
    /// request and every clone must increment the SAME counters -- a per-clone copy would report
    /// one request per request forever.
    pub metrics: Arc<Metrics>,
    /// Shared token buckets. `Arc` for the same reason `metrics` is: every clone must consult the
    /// SAME buckets, or the limit is per-request and limits nothing.
    pub limiter: Arc<RateLimiter>,
    /// Where raw MIME and attachment bodies live. `None` keeps the pre-blob behaviour -- parse and
    /// discard -- so a deployment without a configured root still sends and receives; only
    /// `GET .../raw` degrades, and it degrades to a 404 rather than to a lie.
    pub blobs: Option<amk_store::blobs::FsBlobStore>,
}

impl AppState {
    pub fn new(pool: PgPool, config: AppConfig, keyring: Keyring) -> Self {
        let transport = match &config.smtp_smarthost {
            Some((host, port)) => OutboundTransport::smarthost(host, *port),
            None => OutboundTransport::direct_mx(),
        };
        Self::with_outbound(pool, config, keyring, transport)
    }

    pub fn with_outbound(
        pool: PgPool,
        config: AppConfig,
        keyring: Keyring,
        transport: OutboundTransport,
    ) -> Self {
        Self {
            pool,
            config,
            keyring: Arc::new(keyring),
            transport,
            metrics: Arc::new(Metrics::new()),
            limiter: Arc::new(RateLimiter::default()),
            blobs: None,
        }
    }
}

/// Build the router for this dispatch's 41 operations. Three mounts share one handler set per
/// collection (see each `handlers::*` module) — every `*_org`/`*_pod`/`*_inbox` sibling calls the
/// same shared inner function, so the routing table below is the only place mount and path are
/// spelled out.
pub fn router(state: AppState) -> Router {
    // Read before `state` moves into `.with_state` below. `DefaultBodyLimit` is a request-level
    // extension (`axum_core::extract::default_body_limit`'s own doc), inserted by this `.layer()`
    // on every request BEFORE routing, which is what makes `body::JsonBody`'s
    // `Bytes::from_request` (and hence `body::JsonBody` at all 8 body sites) see the CONFIGURED
    // limit instead of axum's own unconditional 2 MB `DEFAULT_LIMIT` —
    // `reference/fixtures/27-malformed-request-handling.txt` §5.
    let max_body_bytes = state.config.max_body_bytes;
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
        // ---- messages (3) and threads (9): lists, and now the full get/patch/delete triple ----
        //
        // The get-by-id paths carry GET, PATCH and DELETE in the spec, and all three are served
        // here — they waited on `amk-store`'s update and delete, which is why the LIST slice landed
        // first with these deliberately unmounted. A path serving only some of its described
        // methods is what `scripts/derive-implemented-paths.sh` reports, and the P1 gate's
        // schemathesis scope is derived by PATH, so a half-served path would have the gate fuzzing
        // operations this server does not implement and reporting absences as failures.
        .route("/v0/inboxes/{inbox_id}/messages", get(handlers::messages::list))
        .route(
            "/v0/inboxes/{inbox_id}/messages/{message_id}",
            get(handlers::messages::get)
                .patch(handlers::messages::update)
                .delete(handlers::messages::delete),
        )
        .route("/v0/inboxes/{inbox_id}/messages/send", post(handlers::messages::send))
        .route(
            "/v0/inboxes/{inbox_id}/messages/{message_id}/reply",
            post(handlers::messages::reply),
        )
        .route(
            "/v0/inboxes/{inbox_id}/messages/{message_id}/reply-all",
            post(handlers::messages::reply_all),
        )
        .route(
            "/v0/inboxes/{inbox_id}/messages/{message_id}/forward",
            post(handlers::messages::forward),
        )
        // Search and the attachment downloads are deferred with FTS and blobs respectively; every
        // exclusion is recorded in `.claude/contracts/amk-http-message-thread-reads.md` rather than
        // left as an absence, because an unexplained gap in the reconciliation reads as oversight.
        .route("/v0/threads", get(handlers::threads::list_org))
        .route(
            "/v0/threads/{thread_id}",
            get(handlers::threads::get_org)
                .patch(handlers::threads::update_org)
                .delete(handlers::threads::delete_org),
        )
        .route("/v0/pods/{pod_id}/threads", get(handlers::threads::list_pod))
        .route(
            "/v0/pods/{pod_id}/threads/{thread_id}",
            get(handlers::threads::get_pod)
                .patch(handlers::threads::update_pod)
                .delete(handlers::threads::delete_pod),
        )
        .route("/v0/inboxes/{inbox_id}/threads", get(handlers::threads::list_inbox))
        .route(
            "/v0/inboxes/{inbox_id}/threads/{thread_id}",
            get(handlers::threads::get_inbox)
                .patch(handlers::threads::update_inbox)
                .delete(handlers::threads::delete_inbox),
        )
        // Unknown path -> 404 envelope.
        // ---- raw message fetch + the signed download it points at ----
        .route("/v0/inboxes/{inbox_id}/messages/{message_id}/raw", get(handlers::messages::raw))
        // ---- get-attachment, on its four non-draft mounts. The three draft-scoped mounts in the
        // spec wait for drafts themselves; recording the exclusion here is what keeps the
        // reconciliation's gap explained rather than an oversight. ----
        .route(
            "/v0/inboxes/{inbox_id}/messages/{message_id}/attachments/{attachment_id}",
            get(handlers::attachments::get_message),
        )
        .route(
            "/v0/inboxes/{inbox_id}/threads/{thread_id}/attachments/{attachment_id}",
            get(handlers::attachments::get_inbox_thread),
        )
        .route(
            "/v0/threads/{thread_id}/attachments/{attachment_id}",
            get(handlers::attachments::get_org_thread),
        )
        .route(
            "/v0/pods/{pod_id}/threads/{thread_id}/attachments/{attachment_id}",
            get(handlers::attachments::get_pod_thread),
        )
        // Unauthenticated by design: the token IS the authorisation. Mounted under /v0 because it
        // is part of the product surface a client follows, unlike /health and /metrics below.
        .route("/v0/blobs/{blob_id}", get(handlers::messages::download_blob))
        .fallback(not_found_fallback)
        // A path that exists but with the wrong method -> the SAME 404 envelope, never axum's
        // default 405 — the dispatch contract is explicit: "There is no 405."
        .method_not_allowed_fallback(not_found_fallback)
        // ---- operational surface (3) — NOT part of the AgentMail API ----
        //
        // Unversioned and outside /v0 deliberately. These are not reference-API operations, and
        // mounting them under /v0 would make `derive-implemented-paths.sh` count them against the
        // 130 the spec describes -- inflating the coverage number with endpoints AgentMail does
        // not have. The conformance diff would then flag three paths the reference never serves.
        .route("/health", get(observability::health))
        .route("/ready", get(observability::ready))
        .route("/metrics", get(observability::metrics))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        // Outermost, so it sees every request including ones rejected by routing or the body
        // limit -- a 404 or a 413 is exactly the kind of thing worth counting, and a layer added
        // after `.with_state` would never observe them.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            observability::trace_requests,
        ))
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
        let state = AppState::new(pool, AppConfig::default(), Keyring::new());
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
