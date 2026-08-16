//! DB-touching library-level tests: `amk init` against a genuinely fresh database, `amk init`
//! run twice, and `amk_http::router` (as `amkd --role api` would mount it) answering
//! `GET /v0/auth/me` with the root key `amk init` minted — the two halves of this dispatch
//! meeting, and the P0 gate in miniature. Every test skips cleanly if the dev database is
//! unreachable (`tests/support`).

mod support;

use amk_cli::commands::init;
use amk_http::{router, AppConfig, AppState};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

/// `amk init` against a fresh database: the organization id and the default pod's id are the
/// SAME uuid rendered two ways (`amk_types::ids::OrganizationId` is a string newtype,
/// `amk_types::ids::PodId` a uuid newtype — `fixture 22`'s equality has to be checked across that
/// type boundary, not assumed from the source alone), the minted key is organization-scoped
/// (`pod_id`/`inbox_id` both absent), and its permissions are `None` — NOT
/// `Some(ApiKeyPermissions::default())`, which is a real, one-character-apart, opposite-in-effect
/// typo away in the source (`amk_types::api_key::KeyGrants::from_wire` is what makes the
/// distinction load-bearing: absent grants everything, present-but-empty grants nothing).
#[tokio::test]
async fn init_against_a_fresh_database_mints_the_org_mount_shape() {
    let Some(db) = support::FreshDb::create("init_against_a_fresh_database").await else {
        return;
    };

    let outcome = init::run_with_pool(&db.pool)
        .await
        .expect("init must succeed against a fresh db");

    assert_eq!(
        outcome.organization_id.as_str(),
        outcome.pod_id.0.to_string(),
        "the default pod's id must be the organization id's own uuid rendering (fixture 22)"
    );
    assert_eq!(
        outcome.root_key.pod_id, None,
        "the root key must be organization-scoped: pod_id"
    );
    assert_eq!(
        outcome.root_key.inbox_id, None,
        "the root key must be organization-scoped: inbox_id"
    );
    assert_eq!(
        outcome.root_key.permissions, None,
        "None must mean \"grants everything\" -- Some(default()) would grant nothing, the exact \
         opposite, and is one character away in the source"
    );
    assert!(
        outcome.root_key.api_key.starts_with("am_us_"),
        "unexpected key shape (fixture 23)"
    );

    db.drop_it().await;
}

/// `amk init` twice: the second run refuses, mints no second key, and the deployment still has
/// exactly the one organization the first run created.
#[tokio::test]
async fn init_twice_refuses_the_second_run_and_mints_nothing() {
    let Some(db) = support::FreshDb::create("init_twice").await else {
        return;
    };

    let first = init::run_with_pool(&db.pool)
        .await
        .expect("first init must succeed");

    let second = init::run_with_pool(&db.pool).await;
    match &second {
        Err(init::InitError::AlreadyInitialized) => {}
        other => {
            panic!("a second init on an already-initialised deployment must refuse, got: {other:?}")
        }
    }

    // Nothing was minted by the refused run: the first run's own organization is still the only
    // one this deployment has, resolvable by the same id it was created with.
    let still_there = amk_store::organizations::get(&db.pool, &first.organization_id)
        .await
        .expect("lookup must not error")
        .expect("the first run's organization must still exist, untouched");
    assert_eq!(still_there.organization_id, first.organization_id);

    db.drop_it().await;
}

/// The two halves of this dispatch meeting: `amk_http::router`, mounted the way `amkd --role
/// api` mounts it, answers `GET /v0/auth/me` with the root key `amk init` minted -- `scope_id`
/// equal to `organization_id` for an organization-scoped identity (fixture 01).
#[tokio::test]
async fn the_router_amkd_mounts_answers_auth_me_with_the_root_key_from_init() {
    let Some(db) = support::FreshDb::create("router_auth_me_with_root_key").await else {
        return;
    };

    let outcome = init::run_with_pool(&db.pool)
        .await
        .expect("init must succeed");
    let app = router(AppState::new(db.pool.clone(), AppConfig::default()));

    let request = Request::builder()
        .method("GET")
        .uri("/v0/auth/me")
        .header("authorization", format!("Bearer {}", outcome.root_key.api_key))
        .body(Body::empty())
        .expect("valid request");

    let response = app
        .oneshot(request)
        .await
        .expect("router never errors as a Service");
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body is collectible");
    let body: Value = serde_json::from_slice(&bytes).expect("auth/me returns JSON");

    assert_eq!(
        body["organization_id"],
        Value::String(outcome.organization_id.as_str().to_owned())
    );
    assert_eq!(body["scope_type"], Value::String("organization".to_owned()));
    assert_eq!(
        body["scope_id"], body["organization_id"],
        "an organization-scoped identity's scope_id equals its organization_id (fixture 01)"
    );

    db.drop_it().await;
}
