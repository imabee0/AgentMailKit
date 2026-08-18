//! PR 5 assigned HTTP send / reply / reply-all / forward cases.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`. MIME-only unit tests do not discharge these:
//! every success path GETs the store/thread, not only the assembled bytes.

mod support;

use amk_outbound::INLINE_ATTACHMENT_MAX_BYTES;
use amk_types::ids::MessageId;
use base64::Engine as _;
use serde_json::json;

fn encode_mid(id: &str) -> String {
    MessageId::new(id).to_path_segment()
}

/// 1b. No-key send: fail-closed error **and** `messages::get`/list empty.
#[tokio::test]
async fn no_key_send_stores_nothing() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "nokey").await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/send"),
        Some(&key),
        json!({"to":"you@other.test","subject":"hi","text":"body"}),
    )
    .await;
    assert_ne!(resp.status, 200, "no key must fail closed: {}", resp.body);
    assert_eq!(resp.code(), Some("message_rejected"), "{}", resp.body);

    let listed = support::get(&router, &format!("/v0/inboxes/{seg}/messages"), Some(&key)).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.json.unwrap()["count"], 0, "store must be empty after a refused send");
}

/// 2. `reply` GET the thread: parent membership, same `thread_id`.
#[tokio::test]
async fn reply_joins_the_parent_thread() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "reply").await;
    let (parent_thread, parent_mid) =
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &["received"]).await;
    let key = support::org_key(&pool, &org).await;
    let (router, _rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/reply", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"text":"thanks"}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = resp.json.unwrap();
    let reply_thread = body["thread_id"].as_str().unwrap();
    assert_eq!(reply_thread, parent_thread.to_string(), "reply must keep the parent thread");

    let thread =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{reply_thread}"), Some(&key))
            .await;
    assert_eq!(thread.status, 200, "{}", thread.body);
    let t = thread.json.unwrap();
    assert_eq!(t["message_count"], 2, "{t}");
    let ids: Vec<&str> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&parent_mid.as_str()), "parent membership: {ids:?}");
    assert_eq!(ids.len(), 2, "{ids:?}");
}

/// 3. Unbracketed parent still joins (fixture 21 / C3), via GET thread.
#[tokio::test]
async fn unbracketed_parent_reply_still_joins() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "bare").await;
    let bare = format!("amkc3-root-{}@example.test", support::unique_suffix());
    let (parent_thread, parent_mid) = seed_named_parent(&pool, &org, pod, &inbox, &bare).await;
    assert!(!parent_mid.is_bracketed());
    let key = support::org_key(&pool, &org).await;
    let (router, _rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/reply", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"text":"bare reply"}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.json.unwrap()["thread_id"].as_str().unwrap(), parent_thread.to_string());

    let thread =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{parent_thread}"), Some(&key))
            .await;
    let t = thread.json.unwrap();
    assert_eq!(t["message_count"], 2, "C3: unbracketed parent still joins: {t}");
    let reply = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["message_id"].as_str() != Some(parent_mid.as_str()));
    let irt = reply.unwrap()["in_reply_to"].as_str().unwrap();
    assert!(irt.starts_with('<') && irt.ends_with('>'), "re-bracketed on store: {irt}");
}

/// 4. `reply-all` excludes sending inbox, de-duplicates.
#[tokio::test]
async fn reply_all_excludes_us_and_dedupes() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "rall").await;
    let (parent_thread, parent_mid) = seed_parent_with_recipients(&pool, &org, pod, &inbox).await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/reply-all", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"text":"all"}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);

    let sent = rec.sent();
    assert_eq!(sent.len(), 1);
    let envelope = &sent[0].envelope_to;
    assert!(
        !envelope
            .iter()
            .any(|a| a.eq_ignore_ascii_case(inbox.as_str())),
        "{envelope:?}"
    );
    assert!(
        envelope
            .iter()
            .any(|a| a.eq_ignore_ascii_case("alice@other.test")),
        "{envelope:?}"
    );
    assert!(
        envelope
            .iter()
            .any(|a| a.eq_ignore_ascii_case("bob@other.test")),
        "{envelope:?}"
    );
    assert!(
        envelope
            .iter()
            .any(|a| a.eq_ignore_ascii_case("carol@other.test")),
        "{envelope:?}"
    );
    assert_eq!(
        envelope
            .iter()
            .filter(|a| a.eq_ignore_ascii_case("bob@other.test"))
            .count(),
        1,
        "de-duplicated: {envelope:?}"
    );

    let thread =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{parent_thread}"), Some(&key))
            .await;
    assert_eq!(thread.json.unwrap()["message_count"], 2);
}

/// 5. `forward` returned `thread_id` ≠ parent.
#[tokio::test]
async fn forward_opens_a_new_thread() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "fwd").await;
    let (parent_thread, parent_mid) =
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &["received"]).await;
    let key = support::org_key(&pool, &org).await;
    let (router, _rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/forward", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"to":"you@other.test","subject":"Fwd: seeded","text":"f"}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let fwd_thread = resp.json.unwrap()["thread_id"].as_str().unwrap().to_owned();
    assert_ne!(fwd_thread, parent_thread.to_string());

    let parent =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{parent_thread}"), Some(&key))
            .await;
    assert_eq!(parent.json.unwrap()["message_count"], 1, "forward must not join the parent");
}

/// 6. Hostile `headers` (From, Bcc, CR/LF) plus CR/LF in `to` and `subject`.
#[tokio::test]
async fn hostile_headers_and_crlf_in_to_or_subject_are_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "hostile").await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();
    let uri = format!("/v0/inboxes/{seg}/messages/send");

    for (label, body) in [
        (
            "caller From",
            json!({"to":"you@other.test","subject":"h","headers":{"From":"ev@il"}}),
        ),
        (
            "caller Bcc",
            json!({"to":"you@other.test","subject":"h","headers":{"Bcc":"ev@il"}}),
        ),
        (
            "CR/LF in header value",
            json!({"to":"you@other.test","subject":"h","headers":{"X-Evil":"a\r\nBcc: ev@il"}}),
        ),
        (
            "CR/LF in to",
            json!({"to":"you@other.test\r\nBcc: ev@il","subject":"h","text":"x"}),
        ),
        (
            "CR/LF in subject",
            json!({"to":"you@other.test","subject":"h\ninjected","text":"x"}),
        ),
    ] {
        let resp = support::post(&router, &uri, Some(&key), body).await;
        assert_eq!(resp.status, 400, "{label}: {}", resp.body);
        assert_eq!(resp.code(), Some("validation_error"), "{label}: {}", resp.body);
    }
    assert!(rec.sent().is_empty(), "hostile input must not reach the transport");

    let listed = support::get(&router, &format!("/v0/inboxes/{seg}/messages"), Some(&key)).await;
    assert_eq!(listed.json.unwrap()["count"], 0);
}

/// 7. Send to a local inbox still goes through Transport; stored raw carries DKIM-Signature.
#[tokio::test]
async fn send_to_a_local_inbox_still_goes_through_transport_and_is_signed() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "local").await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/send"),
        Some(&key),
        json!({
            "to": inbox.as_str(),
            "subject": "local",
            "text": "hi",
            "headers": {"X-Trace": "one"}
        }),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let sent = rec.sent();
    assert_eq!(sent.len(), 1, "no local-inbox short-circuit");
    let raw = String::from_utf8_lossy(&sent[0].raw);
    assert!(raw.contains("DKIM-Signature:"), "{raw}");
    assert_eq!(
        raw.matches("X-Trace:").count(),
        1,
        "header once, inside the signed bytes: {raw}"
    );

    let mid = resp.json.unwrap()["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let got = support::get(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}", encode_mid(&mid)),
        Some(&key),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.body);
    let headers = got.json.unwrap()["headers"].clone();
    let dkim = headers.as_object().and_then(|o| {
        o.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("dkim-signature"))
    });
    assert!(dkim.is_some(), "stored headers must carry DKIM-Signature: {headers}");
}

/// 8. Attachment size cap−1 accepted; cap and cap+1 rejected.
#[tokio::test]
async fn attachment_size_cap_is_enforced_on_both_sides() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "att").await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();
    let uri = format!("/v0/inboxes/{seg}/messages/send");
    let cap = INLINE_ATTACHMENT_MAX_BYTES;

    let under = support::post(
        &router,
        &uri,
        Some(&key),
        json!({
            "to": "you@other.test",
            "subject": "under",
            "attachments": [{
                "filename": "a.bin",
                "content": b64(cap - 1)
            }]
        }),
    )
    .await;
    assert_eq!(under.status, 200, "cap-1 must be accepted: {}", under.body);

    for n in [cap, cap + 1] {
        let resp = support::post(
            &router,
            &uri,
            Some(&key),
            json!({
                "to": "you@other.test",
                "subject": "over",
                "attachments": [{
                    "filename": "a.bin",
                    "content": b64(n)
                }]
            }),
        )
        .await;
        assert_eq!(resp.status, 400, "{n} must be rejected: {}", resp.body);
        assert_eq!(resp.code(), Some("validation_error"), "{n}: {}", resp.body);
    }
    assert_eq!(rec.sent().len(), 1, "only the under-cap send is delivered");
}

#[tokio::test]
async fn reply_all_true_with_to_is_a_validation_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "conflict").await;
    let (_t, parent_mid) =
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &["received"]).await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/reply", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"reply_all": true, "to": "x@y.z", "text": "nope"}),
    )
    .await;
    assert_eq!(resp.status, 400, "{}", resp.body);
    assert_eq!(resp.code(), Some("validation_error"), "{}", resp.body);
    assert!(rec.sent().is_empty());
}

fn b64(n: usize) -> String {
    base64::engine::general_purpose::STANDARD.encode(vec![b'A'; n])
}

async fn seed_named_parent(
    pool: &sqlx::PgPool,
    org: &amk_types::ids::OrganizationId,
    pod: amk_types::ids::PodId,
    inbox: &amk_types::ids::InboxId,
    message_id: &str,
) -> (amk_types::ids::ThreadId, MessageId) {
    use amk_store::messages::{self, NewMessage};
    use amk_store::threads::{self, NewThread};
    use amk_types::Timestamp;

    let thread_id = amk_types::ids::ThreadId::from(uuid::Uuid::new_v4());
    let mid = MessageId::new(message_id);
    let now = Timestamp::now();
    threads::insert(
        pool,
        NewThread {
            thread_id,
            organization_id: org.clone(),
            pod_id: pod,
            inbox_id: inbox.clone(),
            labels: vec!["received".into()],
            timestamp: now,
            received_timestamp: Some(now),
            sent_timestamp: None,
            senders: vec!["alice@other.test".into()],
            recipients: vec![inbox.as_str().to_owned()],
            subject: Some("root".into()),
            preview: Some("root".into()),
            last_message_id: mid.clone(),
            message_count: 1,
            size: 10,
        },
    )
    .await
    .expect("seed thread");
    messages::insert(
        pool,
        NewMessage {
            inbox_id: inbox.clone(),
            message_id: mid.clone(),
            organization_id: org.clone(),
            pod_id: pod,
            thread_id,
            labels: vec!["received".into()],
            timestamp: now,
            from: "alice@other.test".into(),
            to: vec![inbox.as_str().to_owned()],
            cc: None,
            bcc: None,
            subject: Some("root".into()),
            preview: Some("root".into()),
            attachments: None,
            in_reply_to: None,
            references: None,
            headers: None,
            smtp_id: None,
            size: 10,
            reply_to: None,
            text: Some("root".into()),
            html: None,
            extracted_text: None,
            extracted_html: None,
        },
    )
    .await
    .expect("seed message");
    (thread_id, mid)
}

async fn seed_parent_with_recipients(
    pool: &sqlx::PgPool,
    org: &amk_types::ids::OrganizationId,
    pod: amk_types::ids::PodId,
    inbox: &amk_types::ids::InboxId,
) -> (amk_types::ids::ThreadId, MessageId) {
    use amk_store::messages::{self, NewMessage};
    use amk_store::threads::{self, NewThread};
    use amk_types::Timestamp;

    let thread_id = amk_types::ids::ThreadId::from(uuid::Uuid::new_v4());
    let mid = MessageId::new(format!("<rall-{}@example.test>", support::unique_suffix()));
    let now = Timestamp::now();
    threads::insert(
        pool,
        NewThread {
            thread_id,
            organization_id: org.clone(),
            pod_id: pod,
            inbox_id: inbox.clone(),
            labels: vec!["received".into()],
            timestamp: now,
            received_timestamp: Some(now),
            sent_timestamp: None,
            senders: vec!["alice@other.test".into()],
            recipients: vec![
                inbox.as_str().to_owned(),
                "bob@other.test".into(),
                "carol@other.test".into(),
            ],
            subject: Some("all".into()),
            preview: Some("all".into()),
            last_message_id: mid.clone(),
            message_count: 1,
            size: 10,
        },
    )
    .await
    .expect("seed thread");
    messages::insert(
        pool,
        NewMessage {
            inbox_id: inbox.clone(),
            message_id: mid.clone(),
            organization_id: org.clone(),
            pod_id: pod,
            thread_id,
            labels: vec!["received".into()],
            timestamp: now,
            from: "alice@other.test".into(),
            to: vec![inbox.as_str().to_owned(), "bob@other.test".into()],
            cc: Some(vec!["Bob@other.test".into(), "carol@other.test".into()]),
            bcc: None,
            subject: Some("all".into()),
            preview: Some("all".into()),
            attachments: None,
            in_reply_to: None,
            references: None,
            headers: None,
            smtp_id: None,
            size: 10,
            reply_to: None,
            text: Some("all".into()),
            html: None,
            extracted_text: None,
            extracted_html: None,
        },
    )
    .await
    .expect("seed message");
    (thread_id, mid)
}
