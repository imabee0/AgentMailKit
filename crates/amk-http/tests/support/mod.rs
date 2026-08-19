//! Shared integration-test scaffolding: a real Postgres-backed router, and helpers to seed
//! organizations/pods/inboxes/keys and drive HTTP requests through the router in-process.
//!
//! Mirrors `amk-store/tests/support/mod.rs`'s own shape and its skip-cleanly-without-a-database
//! contract, extended with an HTTP request helper (`amk_http::router` is what this crate exists
//! to test) and real key minting (`amk_store::api_keys::create`'s plaintext `api_key` is the
//! bearer token every authenticated test presents).

#![allow(dead_code)] // not every helper is used by every test binary in this integration suite.

use amk_http::{router, AppConfig, AppState};
use amk_outbound::{Keyring, OutboundTransport, RecordingTransport};
use amk_store::api_keys::{self, NewApiKey};
use amk_store::inboxes::{self, NewInbox};
use amk_store::messages::{self, NewMessage};
use amk_store::organizations::{self, NewOrganization};
use amk_store::pods::{self, NewPod};
use amk_store::threads::{self, NewThread};
use amk_types::api_key::ApiKeyPermissions;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::Timestamp;
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
        ..AppConfig::default()
    };
    router(AppState::new(pool, config, Keyring::new()))
}

/// A router that can send: fixture DKIM key for `example.test` plus a recording transport.
pub fn send_router(pool: PgPool, keyring: Keyring) -> (Router, RecordingTransport) {
    let rec = RecordingTransport::new();
    let config = AppConfig {
        primary_domain: Some("example.test".into()),
        product_name: Some("AmkTest".into()),
        ..AppConfig::default()
    };
    let app = router(AppState::with_outbound(
        pool,
        config,
        keyring,
        OutboundTransport::recording(rec.clone()),
    ));
    (app, rec)
}

/// The throwaway RSA fixture in `amk-outbound` testdata, registered for `domain`.
pub fn fixture_keyring(domain: &str) -> Keyring {
    use base64::Engine as _;
    let wrapped: String =
        include_str!("../../../amk-outbound/src/testdata/test-signing-key.pkcs8.b64")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(wrapped)
        .expect("embedded test key is valid base64");
    let mut k = Keyring::new();
    k.insert_der(domain, "amk", &der)
        .expect("fixture DER loads");
    k
}

/// A router with no configured domain/product name — for the tests that specifically assert the
/// fail-closed behaviour.
pub fn unconfigured_router(pool: PgPool) -> Router {
    router(AppState::new(pool, AppConfig::default(), Keyring::new()))
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

/// A parsed HTTP response: status, the response's own `Content-Type` header, raw body text, and
/// the body parsed as JSON when it is (every success and every `AppError` response is JSON; a
/// router-level 405 would not be, which is exactly the shape the "no 405 anywhere" edge case tests
/// for).
///
/// `content_type` was added for this dispatch (`amk-http-extractor-rejections`): every malformed-
/// request edge case it tests turns on whether the RESPONSE is `application/json` or axum's own
/// `text/plain` — a distinction `status`/`json`/`body` alone cannot make (a `text/plain` body that
/// happens to parse as JSON, or an empty JSON-looking string, would otherwise be indistinguishable
/// from the real envelope).
pub struct TestResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
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
    /// The first `errors[]` entry, for the malformed-request tests that assert the whole issue
    /// object (code + path + kind-specific extras), not merely the envelope's own `code`.
    pub fn first_error(&self) -> Option<&Value> {
        self.json.as_ref()?.get("errors")?.get(0)
    }
}

/// Send one request through `router` (cloned — `Router` is cheap to clone, an `Arc` handle) and
/// collect the response.
async fn dispatch(router: &Router, request: Request<Body>) -> TestResponse {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router never errors as a Service");
    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test bodies are always collectible");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_str(&body).ok();
    TestResponse { status, content_type, json, body }
}

/// Send a request whose body is a `serde_json::Value` — every ordinary, well-formed test request
/// in this suite. Always sets `content-type: application/json` for `Some(body)`, and sends neither
/// a body nor a `Content-Type` header for `None` (case 5's own shape — see [`send_raw`] for the
/// malformed-request cases this cannot express at all: a raw non-JSON body, a body sent under a
/// content type OTHER than `application/json`, and a request that carries a `Content-Type` header
/// but literally no body).
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
    dispatch(router, request).await
}

/// Send a request with a caller-controlled raw body and `Content-Type` — the malformed-request
/// edge cases `send`/`post`/`patch` cannot express (see [`send`]'s own doc for the exact three).
/// `content_type: None` omits the header entirely; `Some("")` is not a case any edge case needs
/// and is not specially handled.
pub async fn send_raw(
    router: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    content_type: Option<&str>,
    raw_body: &[u8],
) -> TestResponse {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    let request = builder
        .body(Body::from(raw_body.to_vec()))
        .expect("valid request");
    dispatch(router, request).await
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

/// Seeds one thread and one message in it, with the labels the caller names.
///
/// Added for the message/thread read dispatch: every earlier suite seeded only pods, inboxes and
/// keys. Returns both ids so a test can assert the get-by-id and list paths against the SAME row —
/// a restricted-label test that seeds one row for the list assertion and another for the by-id
/// assertion proves nothing about the asymmetry it claims to test.
pub async fn seed_thread_with_message(
    pool: &PgPool,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
    labels: &[&str],
) -> (ThreadId, MessageId) {
    let thread_id = ThreadId::from(Uuid::new_v4());
    let message_id = MessageId::new(format!("<{}@example.test>", unique_suffix()));
    let now = Timestamp::now();
    let labels: Vec<String> = labels.iter().map(|l| (*l).to_owned()).collect();
    threads::insert(
        pool,
        NewThread {
            thread_id,
            organization_id: org.clone(),
            pod_id: pod,
            inbox_id: inbox.clone(),
            labels: labels.clone(),
            timestamp: now,
            received_timestamp: Some(now),
            sent_timestamp: None,
            senders: vec!["sender@example.test".into()],
            recipients: vec![inbox.as_str().to_owned()],
            subject: Some("seeded".into()),
            preview: Some("seeded preview".into()),
            last_message_id: message_id.clone(),
            message_count: 1,
            size: 42,
        },
    )
    .await
    .expect("seed thread");
    messages::insert(
        pool,
        NewMessage {
            inbox_id: inbox.clone(),
            message_id: message_id.clone(),
            organization_id: org.clone(),
            pod_id: pod,
            thread_id,
            labels,
            timestamp: now,
            from: "sender@example.test".into(),
            to: vec![inbox.as_str().to_owned()],
            cc: None,
            bcc: None,
            subject: Some("seeded".into()),
            preview: Some("seeded preview".into()),
            attachments: None,
            in_reply_to: None,
            references: None,
            headers: None,
            smtp_id: None,
            size: 42,
            reply_to: None,
            text: Some("body".into()),
            html: None,
            extracted_text: None,
            extracted_html: None,
            raw_blob_id: None,
        },
    )
    .await
    .expect("seed message");
    (thread_id, message_id)
}
