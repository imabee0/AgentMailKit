//! Shared integration-test scaffolding: a real Postgres-backed router, and helpers to seed
//! organizations/pods/inboxes/keys and drive HTTP requests through the router in-process.
//!
//! Mirrors `amk-store/tests/support/mod.rs`'s own shape and its skip-cleanly-without-a-database
//! contract, extended with an HTTP request helper (`amk_http::router` is what this crate exists
//! to test) and real key minting (`amk_store::api_keys::create`'s plaintext `api_key` is the
//! bearer token every authenticated test presents).

#![allow(dead_code)] // not every helper is used by every test binary in this integration suite.

use amk_http::{router, AppConfig, AppState};
use amk_store::api_keys::{self, NewApiKey};
use amk_store::inboxes::{self, NewInbox};
use amk_store::organizations::{self, NewOrganization};
use amk_store::pods::{self, NewPod};
use amk_types::api_key::ApiKeyPermissions;
use amk_types::ids::{InboxId, OrganizationId, PodId};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const DATABASE_URL: &str = "postgres://amk:amk-dev-local@127.0.0.1:55432/amk";

/// See `amk-store/tests/support/mod.rs::pool` for the full reasoning (migration-mismatch vs
/// genuinely-unreachable, and why `AMK_REQUIRE_DB=1` turns a skip into a panic).
pub async fn pool() -> Option<PgPool> {
    match amk_store::connect(DATABASE_URL).await {
        Ok(p) => Some(p),
        Err(e) => {
            if std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1") {
                panic!(
                    "AMK_REQUIRE_DB=1 but the dev database is unreachable ({e}). Run \
                     `./scripts/dev-db.sh up`, or unset AMK_REQUIRE_DB to allow this suite to \
                     skip its database-backed tests."
                );
            }
            eprintln!("skipping: dev database unreachable ({e})");
            None
        }
    }
}

/// A router wired to a real pool, with a fixed, deterministic deployment configuration so tests
/// never depend on operator-supplied config: a primary domain and product name are always
/// present, so inbox-creation defaults never hit the fail-closed path unless a test asks for
/// exactly that.
pub fn test_router(pool: PgPool) -> Router {
    let config = AppConfig {
        primary_domain: Some("example.test".into()),
        product_name: Some("AmkTest".into()),
    };
    router(AppState::new(pool, config))
}

/// A router with no configured domain/product name — for the tests that specifically assert the
/// fail-closed behaviour.
pub fn unconfigured_router(pool: PgPool) -> Router {
    router(AppState::new(pool, AppConfig::default()))
}

pub fn unique_suffix() -> String {
    Uuid::new_v4().simple().to_string()
}

pub async fn seed_org(pool: &PgPool) -> OrganizationId {
    let id = OrganizationId::new(format!("org-{}", unique_suffix()));
    organizations::create(
        pool,
        NewOrganization {
            organization_id: id.clone(),
            name: None,
            inbox_limit: None,
            domain_limit: None,
        },
    )
    .await
    .expect("seed organization");
    id
}

/// Seeds an organization AND a pod carrying the organization's own id as its `pod_id` — the
/// "default pod" shape fixture 22 observed (`amk init` mints it this way), so tests that exercise
/// `POST /v0/inboxes` at the org mount with an org-scoped credential resolve a real pod rather
/// than hitting the internal-error fail-closed path.
pub async fn seed_org_with_default_pod(pool: &PgPool) -> OrganizationId {
    let id = OrganizationId::new(Uuid::new_v4().to_string());
    organizations::create(
        pool,
        NewOrganization {
            organization_id: id.clone(),
            name: None,
            inbox_limit: None,
            domain_limit: None,
        },
    )
    .await
    .expect("seed organization");
    let default_pod_id =
        PodId::from(Uuid::parse_str(id.as_str()).expect("organization id is itself a UUID here"));
    pods::create(
        pool,
        NewPod {
            organization_id: id.clone(),
            pod_id: default_pod_id,
            client_id: None,
            name: "Default Pod".into(),
        },
    )
    .await
    .expect("seed default pod");
    id
}

pub async fn seed_pod(pool: &PgPool, org: &OrganizationId) -> PodId {
    let pod_id = PodId::new_random();
    pods::create(
        pool,
        NewPod { organization_id: org.clone(), pod_id, client_id: None, name: "test-pod".into() },
    )
    .await
    .expect("seed pod");
    pod_id
}

pub async fn seed_inbox(
    pool: &PgPool,
    org: &OrganizationId,
    pod: PodId,
    local_part: &str,
) -> InboxId {
    let inbox_id = InboxId::new(format!("{local_part}-{}@example.test", unique_suffix()));
    let inbox = inboxes::create(
        pool,
        NewInbox {
            inbox_id,
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .expect("seed inbox");
    inbox.inbox_id
}

/// Mint a real key and return its plaintext secret — the `Authorization: Bearer <secret>` value
/// every authenticated request in this suite presents.
pub async fn mint_key(
    pool: &PgPool,
    org: &OrganizationId,
    pod_id: Option<PodId>,
    inbox_id: Option<InboxId>,
    permissions: Option<ApiKeyPermissions>,
) -> String {
    let created = api_keys::create(
        pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id,
            inbox_id,
            name: "test-key".into(),
            permissions,
        },
    )
    .await
    .expect("mint key");
    created.api_key
}

pub async fn org_key(pool: &PgPool, org: &OrganizationId) -> String {
    mint_key(pool, org, None, None, None).await
}

pub async fn pod_key(pool: &PgPool, org: &OrganizationId, pod: PodId) -> String {
    mint_key(pool, org, Some(pod), None, None).await
}

pub async fn inbox_key(pool: &PgPool, org: &OrganizationId, inbox: &InboxId) -> String {
    mint_key(pool, org, None, Some(inbox.clone()), None).await
}

/// A parsed HTTP response: status, raw body text, and the body parsed as JSON when it is (every
/// success and every `AppError` response is JSON; a router-level 405 would not be, which is
/// exactly the shape the "no 405 anywhere" edge case tests for).
pub struct TestResponse {
    pub status: StatusCode,
    pub json: Option<Value>,
    pub body: String,
}

impl TestResponse {
    pub fn code(&self) -> Option<&str> {
        self.json.as_ref()?.get("code")?.as_str()
    }
    pub fn message(&self) -> Option<&str> {
        self.json.as_ref()?.get("message")?.as_str()
    }
}

/// Send one request through `router` (cloned — `Router` is cheap to clone, an `Arc` handle) and
/// collect the response body.
pub async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> TestResponse {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).expect("test body serializes")))
            .expect("valid request"),
        None => builder.body(Body::empty()).expect("valid request"),
    };
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router never errors as a Service");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test bodies are always collectible");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_str(&body).ok();
    TestResponse { status, json, body }
}

pub async fn get(router: &Router, uri: &str, bearer: Option<&str>) -> TestResponse {
    send(router, "GET", uri, bearer, None).await
}
pub async fn post(router: &Router, uri: &str, bearer: Option<&str>, body: Value) -> TestResponse {
    send(router, "POST", uri, bearer, Some(body)).await
}
pub async fn patch(router: &Router, uri: &str, bearer: Option<&str>, body: Value) -> TestResponse {
    send(router, "PATCH", uri, bearer, Some(body)).await
}
pub async fn delete(router: &Router, uri: &str, bearer: Option<&str>) -> TestResponse {
    send(router, "DELETE", uri, bearer, None).await
}
