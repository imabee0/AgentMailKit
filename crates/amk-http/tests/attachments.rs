//! `GET .../attachments/{attachment_id}` on its four mounts, against a real pool and a real
//! blob tree.
//!
//! The response contract is `[SPEC:openapi type_attachments:AttachmentResponse]` — flattened
//! metadata plus `download_url` and `expires_at` — and fixture 06 fixes the download's behaviour:
//! a time-limited URL that answers a flat 403 after expiry. What these tests add over the
//! binary smoke's Gate 7 is the access matrix: every way a caller might reach an attachment they
//! must not see resolves to the SAME not-found envelope, because `attachment_id` is a minted UUID
//! and an existence oracle over it enumerates every attachment in the deployment.
mod support;

use std::collections::BTreeMap;

use amk_http::{router, AppConfig, AppState};
use amk_outbound::Keyring;
use amk_store::blobs::{BlobStore, FsBlobStore};
use amk_store::messages::{self, NewMessage};
use amk_store::threads::{self, NewThread};
use amk_types::ids::{AttachmentId, InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::message::Attachment;
use amk_types::Timestamp;
use axum::http::StatusCode;
use axum::Router;
use sqlx::PgPool;
use uuid::Uuid;

const BODY: &[u8] = b"%PDF-1.4 attachment body bytes";

fn blob_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("amk-http-att-{}", support::unique_suffix()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A router with blobs and a master key — the deployment shape `amkd` enforces (a blob root and
/// a key arrive together or not at all).
fn blob_router(pool: PgPool, store: FsBlobStore) -> Router {
    let config = AppConfig {
        primary_domain: Some("example.test".into()),
        product_name: Some("AmkTest".into()),
        master_key: Some(vec![7u8; 32]),
        public_base_url: "http://amk.test".into(),
        ..AppConfig::default()
    };
    let mut state = AppState::new(pool, config, Keyring::new());
    state.blobs = Some(store);
    router(state)
}

struct Seeded {
    thread_id: ThreadId,
    message_id: MessageId,
    attachment_id: AttachmentId,
    /// In `attachments` but NOT in `attachment_blobs` — metadata whose body was never captured.
    bodyless_id: AttachmentId,
}

/// One thread, one message, two attachments: one with a stored body, one metadata-only.
async fn seed_message_with_attachment(
    pool: &PgPool,
    store: &FsBlobStore,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
    labels: &[&str],
) -> Seeded {
    let thread_id = ThreadId::from(Uuid::new_v4());
    let message_id = MessageId::new(format!("<{}@example.test>", support::unique_suffix()));
    let attachment_id = AttachmentId::new_random();
    let bodyless_id = AttachmentId::new_random();
    let now = Timestamp::now();
    let labels: Vec<String> = labels.iter().map(|l| (*l).to_owned()).collect();

    let blob = store.put(BODY).await.expect("body stored");
    let mut map = BTreeMap::new();
    map.insert(attachment_id.to_string(), blob.to_string());

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
            subject: Some("with attachment".into()),
            preview: None,
            last_message_id: message_id.clone(),
            message_count: 1,
            size: BODY.len() as u64,
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
            subject: Some("with attachment".into()),
            preview: None,
            attachments: Some(vec![
                Attachment {
                    attachment_id: attachment_id.clone(),
                    filename: Some("report.pdf".into()),
                    size: BODY.len() as u64,
                    content_type: Some("application/pdf".into()),
                    content_disposition: Some("attachment".into()),
                    content_id: None,
                },
                Attachment {
                    attachment_id: bodyless_id.clone(),
                    filename: Some("lost.bin".into()),
                    size: 3,
                    content_type: None,
                    content_disposition: None,
                    content_id: None,
                },
            ]),
            in_reply_to: None,
            references: None,
            headers: None,
            smtp_id: None,
            size: BODY.len() as u64,
            reply_to: None,
            text: Some("see attached".into()),
            html: None,
            extracted_text: None,
            extracted_html: None,
            raw_blob_id: None,
            attachment_blobs: Some(map),
        },
    )
    .await
    .expect("seed message");
    Seeded { thread_id, message_id, attachment_id, bodyless_id }
}

fn enc(s: &str) -> String {
    s.replace('<', "%3C")
        .replace('>', "%3E")
        .replace('@', "%40")
}

/// Assert the full response shape once, and hand back the download URL's path-and-query so the
/// caller can drive the fetch through the same router.
fn assert_response_shape(r: &support::TestResponse, seeded: &Seeded) -> String {
    assert_eq!(r.status, StatusCode::OK, "body: {}", r.body);
    let j = r.json.as_ref().expect("json body");
    assert_eq!(j["attachment_id"], seeded.attachment_id.to_string());
    assert_eq!(j["filename"], "report.pdf");
    assert_eq!(j["size"], BODY.len() as u64);
    assert_eq!(j["content_type"], "application/pdf");
    // Optionals are omitted when absent — never null (CLAUDE.md contract facts).
    assert!(
        !j.as_object().unwrap().contains_key("content_id"),
        "absent content_id must be omitted"
    );
    let expires = j["expires_at"].as_str().expect("expires_at");
    // Wire-exact timestamps: RFC 3339, exactly three fractional digits, Z.
    assert!(regex_lite_match(expires), "expires_at is not the wire-exact shape: {expires}");
    let url = j["download_url"].as_str().expect("download_url");
    assert!(
        url.starts_with("http://amk.test/v0/blobs/"),
        "download_url must be built from public_base_url: {url}"
    );
    url.trim_start_matches("http://amk.test").to_owned()
}

/// `\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z` without pulling a regex crate into the dev-deps.
fn regex_lite_match(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 24
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[23] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 23) || c.is_ascii_digit())
}

#[tokio::test]
async fn all_four_mounts_serve_the_attachment_and_the_url_downloads_without_a_credential() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let root = blob_root();
    let store = FsBlobStore::new(&root);
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "att").await;
    let key = support::org_key(&pool, &org).await;
    let seeded =
        seed_message_with_attachment(&pool, &store, &org, pod, &inbox, &["received"]).await;
    let app = blob_router(pool, store);

    let att = seeded.attachment_id.to_string();
    let mounts = [
        format!(
            "/v0/inboxes/{}/messages/{}/attachments/{att}",
            enc(inbox.as_str()),
            enc(seeded.message_id.as_str())
        ),
        format!(
            "/v0/inboxes/{}/threads/{}/attachments/{att}",
            enc(inbox.as_str()),
            seeded.thread_id
        ),
        format!("/v0/threads/{}/attachments/{att}", seeded.thread_id),
        format!("/v0/pods/{}/threads/{}/attachments/{att}", pod, seeded.thread_id),
    ];
    let mut download = String::new();
    for uri in &mounts {
        let r = support::send(&app, "GET", uri, Some(&key), None).await;
        download = assert_response_shape(&r, &seeded);
    }

    // The minted URL fetches the ORIGINAL bytes with no Authorization header at all.
    let r = support::send(&app, "GET", &download, None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body.as_bytes(), BODY, "the served bytes are the stored body");
    assert_eq!(r.content_type.as_deref(), Some("application/octet-stream"));

    // And a tampered token is the indistinguishable 403 (fixture 06's post-expiry shape).
    let mut tampered = download.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let r = support::send(&app, "GET", &tampered, None, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.message(), Some("Forbidden"));
}

#[tokio::test]
async fn every_wrong_path_to_an_attachment_is_the_same_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let root = blob_root();
    let store = FsBlobStore::new(&root);
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "att").await;
    let key = support::org_key(&pool, &org).await;
    let seeded =
        seed_message_with_attachment(&pool, &store, &org, pod, &inbox, &["received"]).await;

    // A second, unrelated message in the SAME inbox: the message-scoped route must refuse to
    // serve an attachment through a message it does not belong to.
    let (_, other_message) =
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &["received"]).await;

    // A second organization holding a valid key of its own.
    let other_org = support::seed_org(&pool).await;
    let other_key = support::org_key(&pool, &other_org).await;

    let app = blob_router(pool, store);
    let att = seeded.attachment_id.to_string();

    // Expected mask per case. A miss at the ATTACHMENT level answers "Attachment not found"; a
    // foreign credential never gets that far -- the inbox mount itself masks first, exactly as it
    // does for `GET .../messages/{id}`, so the cross-tenant case reads "Inbox not found". Both
    // are the same envelope and the same 404; what must not vary is anything an attacker could
    // use to tell a real attachment id from an invented one, and neither message does.
    let cases: Vec<(String, &str, String, &str)> = vec![
        (
            format!(
                "/v0/inboxes/{}/messages/{}/attachments/{}",
                enc(inbox.as_str()),
                enc(seeded.message_id.as_str()),
                AttachmentId::new_random()
            ),
            "an attachment id that names nothing",
            key.clone(),
            "Attachment not found",
        ),
        (
            format!(
                "/v0/inboxes/{}/messages/{}/attachments/{}",
                enc(inbox.as_str()),
                enc(seeded.message_id.as_str()),
                seeded.bodyless_id
            ),
            "metadata whose body was never captured",
            key.clone(),
            "Attachment not found",
        ),
        (
            format!(
                "/v0/inboxes/{}/messages/{}/attachments/{att}",
                enc(inbox.as_str()),
                enc(other_message.as_str())
            ),
            "a real attachment reached through the wrong message",
            key.clone(),
            "Attachment not found",
        ),
        (
            format!("/v0/threads/{}/attachments/{att}", ThreadId::from(Uuid::new_v4())),
            "a real attachment reached through the wrong thread",
            key.clone(),
            "Attachment not found",
        ),
        (
            format!(
                "/v0/inboxes/{}/messages/{}/attachments/{att}",
                enc(inbox.as_str()),
                enc(seeded.message_id.as_str())
            ),
            "another organization's key on the true path",
            other_key.clone(),
            "Inbox not found",
        ),
        (
            format!("/v0/threads/{}/attachments/not-a-uuid", seeded.thread_id),
            "an attachment segment that is not even id-shaped",
            key.clone(),
            "Attachment not found",
        ),
    ];
    for (uri, why, bearer, mask) in &cases {
        let r = support::send(&app, "GET", uri, Some(bearer), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{why}: {}", r.body);
        assert_eq!(r.code(), Some("not_found"), "{why}: same envelope for every miss");
        assert_eq!(r.message(), Some(*mask), "{why}: the mask must not vary by cause");
    }
}

#[tokio::test]
async fn a_restricted_label_message_keeps_its_attachments_reachable_by_id() {
    // Fixture 09b's asymmetry, extended to attachments: `unauthenticated` mail is excluded from
    // lists but IS reachable by id, and its attachments ride the same rule — one visibility
    // decision, not a second one that could drift.
    let Some(pool) = support::pool().await else {
        return;
    };
    let root = blob_root();
    let store = FsBlobStore::new(&root);
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "att").await;
    let key = support::org_key(&pool, &org).await;
    let seeded = seed_message_with_attachment(
        &pool,
        &store,
        &org,
        pod,
        &inbox,
        &["received", "unauthenticated"],
    )
    .await;
    let app = blob_router(pool, store);

    let uri = format!(
        "/v0/inboxes/{}/messages/{}/attachments/{}",
        enc(inbox.as_str()),
        enc(seeded.message_id.as_str()),
        seeded.attachment_id
    );
    let r = support::send(&app, "GET", &uri, Some(&key), None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "by-id access must include restricted labels: {}",
        r.body
    );
}
