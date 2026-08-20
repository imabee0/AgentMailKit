//! PR 5 assigned HTTP send / reply / reply-all / forward cases.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`. MIME-only unit tests do not discharge these:
//! every success path GETs the store/thread, not only the assembled bytes.

mod support;

use amk_core::scope::{Mount, Resolved, Scope};
use amk_outbound::INLINE_ATTACHMENT_MAX_BYTES;
use amk_store::messages::{self, ListMessagesQuery};
use amk_store::pagination::SortDirection;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId};
use base64::Engine as _;
use serde_json::json;

fn inbox_scope(org: &OrganizationId, pod: PodId, inbox: &InboxId) -> amk_core::scope::ScopeFilter {
    let scope = Scope::Inbox { organization_id: org.clone(), pod_id: pod, inbox_id: inbox.clone() };
    match scope
        .resolve(&Mount::Organization)
        .expect("inbox scope resolves")
    {
        Resolved::Ready(f) => f,
        Resolved::Probe(_) => panic!("expected a settled inbox window"),
    }
}

async fn store_rows_for_inbox(
    pool: &sqlx::PgPool,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
) -> usize {
    let page = messages::list(
        pool,
        &inbox_scope(org, pod, inbox),
        &[],
        ListMessagesQuery { limit: 100, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .expect("store list");
    page.items.len()
}

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
    let router = support::test_router(pool.clone());
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
    assert_eq!(listed.json.unwrap()["count"], 0, "HTTP list must be empty after a refused send");
    assert_eq!(
        store_rows_for_inbox(&pool, &org, pod, &inbox).await,
        0,
        "store must hold zero rows even under no label exclusion (trash persist would hide from list)"
    );
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
///
/// A decoy in another thread is seeded first so `items[0]` is the wrong join. Persist must
/// use the IRT that actually went on the MIME — a second independent `MessageId::bracketed`
/// of the stored parent still joins when the inbox has only the parent.
#[tokio::test]
async fn unbracketed_parent_reply_still_joins() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "bare").await;
    let decoy_id = format!("<decoy-{}@example.test>", support::unique_suffix());
    let (decoy_thread, _) = seed_named_parent(&pool, &org, pod, &inbox, &decoy_id).await;
    let bare = format!("amkc3-root-{}@example.test", support::unique_suffix());
    let (parent_thread, parent_mid) = seed_named_parent(&pool, &org, pod, &inbox, &bare).await;
    assert!(!parent_mid.is_bracketed());
    assert_ne!(decoy_thread, parent_thread);
    let key = support::org_key(&pool, &org).await;
    let (router, rec) = support::send_router(pool, support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}/reply", encode_mid(parent_mid.as_str())),
        Some(&key),
        json!({"text":"bare reply"}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = resp.json.unwrap();
    let reply_thread = body["thread_id"].as_str().unwrap();
    assert_eq!(
        reply_thread,
        parent_thread.to_string(),
        "must join the parent, not a new thread"
    );
    assert_ne!(
        reply_thread,
        decoy_thread.to_string(),
        "must not fall back to the first inbox row (the decoy)"
    );

    let sent = rec.sent();
    assert_eq!(sent.len(), 1);
    let raw = String::from_utf8_lossy(&sent[0].raw);
    let wire_irt = format!("In-Reply-To: <{}>", parent_mid.as_str());
    assert!(raw.contains(&wire_irt), "MIME IRT is the bracketed parent: {raw}");

    let mid = body["message_id"].as_str().unwrap().to_owned();
    let got = support::get(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}", encode_mid(&mid)),
        Some(&key),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.body);
    let stored = got.json.unwrap();
    assert_eq!(
        stored["thread_id"].as_str().unwrap(),
        parent_thread.to_string(),
        "GET thread_id is the parent, not the decoy"
    );
    assert_ne!(stored["thread_id"].as_str().unwrap(), decoy_thread.to_string());
    assert_eq!(
        stored["in_reply_to"].as_str().unwrap(),
        format!("<{}>", parent_mid.as_str()),
        "GET in_reply_to is the re-bracketed parent (the MIME IRT)"
    );

    let thread =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{parent_thread}"), Some(&key))
            .await;
    let t = thread.json.unwrap();
    assert_eq!(t["message_count"], 2, "C3: unbracketed parent still joins: {t}");
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
    let stored = got.json.unwrap();
    let stored_to: Vec<&str> = stored["to"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let stored_cc: Vec<&str> = stored
        .get("cc")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect())
        .unwrap_or_default();
    assert_eq!(stored_to, ["alice@other.test", "bob@other.test"]);
    assert_eq!(stored_cc, ["carol@other.test"]);
    assert!(!stored_to
        .iter()
        .any(|a| a.eq_ignore_ascii_case(inbox.as_str())));
    assert!(!stored_cc
        .iter()
        .any(|a| a.eq_ignore_ascii_case(inbox.as_str())));
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
    let body = resp.json.unwrap();
    let fwd_thread = body["thread_id"].as_str().unwrap().to_owned();
    let fwd_mid = body["message_id"].as_str().unwrap().to_owned();
    assert_ne!(fwd_thread, parent_thread.to_string());

    let parent =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{parent_thread}"), Some(&key))
            .await;
    assert_eq!(parent.json.unwrap()["message_count"], 1, "forward must not join the parent");

    let created =
        support::get(&router, &format!("/v0/inboxes/{seg}/threads/{fwd_thread}"), Some(&key)).await;
    assert_eq!(created.status, 200, "{}", created.body);
    let t = created.json.unwrap();
    assert_eq!(t["message_count"], 1, "{t}");
    let members: Vec<&str> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_str().unwrap())
        .collect();
    assert_eq!(members, [fwd_mid.as_str()], "forwarded message is the sole member: {members:?}");
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
        let path = resp.first_error().and_then(|e| e.get("path"));
        if label.contains("to") && !label.contains("header") {
            assert_eq!(path, Some(&json!(["to"])), "{label}: {}", resp.body);
        } else if label.contains("subject") {
            assert_eq!(path, Some(&json!(["subject"])), "{label}: {}", resp.body);
        } else {
            assert_eq!(path, Some(&json!(["headers"])), "{label}: {}", resp.body);
        }
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
    let dkim_at = raw.find("DKIM-Signature:").expect("DKIM-Signature in raw");
    let from_at = raw.find("From:").expect("From in raw");
    assert!(dkim_at < from_at, "DKIM-Signature must precede From: {raw}");
    assert_eq!(
        raw.lines()
            .filter(|l| l
                .split_once(':')
                .is_some_and(|(n, _)| n.eq_ignore_ascii_case("X-Trace")))
            .count(),
        1,
        "header once, inside the signed bytes: {raw}"
    );
    let h_tag = dkim_h_headers(&raw);
    assert!(
        h_tag.iter().any(|n| n.eq_ignore_ascii_case("x-trace")),
        "DKIM h= must list x-trace (append-after-sign leaves it out): {h_tag:?} in {raw}"
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
    let body = got.json.unwrap();
    let smtp_id = body["smtp_id"].as_str().unwrap_or("");
    assert!(!smtp_id.is_empty(), "smtp_id must be emitted: {body}");
    let headers = body["headers"].clone();
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

/// URL attachments are refused (fetch is P3 SSRF) and store nothing.
#[tokio::test]
async fn url_attachment_is_rejected_and_stores_nothing() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "urlatt").await;
    let key = support::org_key(&pool, &org).await;
    let (router, rec) =
        support::send_router(pool.clone(), support::fixture_keyring("example.test"));
    let seg = inbox.to_path_segment();

    let resp = support::post(
        &router,
        &format!("/v0/inboxes/{seg}/messages/send"),
        Some(&key),
        json!({
            "to": "you@other.test",
            "subject": "url",
            "attachments": [{"url": "https://example.test/file.bin"}]
        }),
    )
    .await;
    assert_eq!(resp.status, 400, "{}", resp.body);
    assert_eq!(resp.code(), Some("validation_error"), "{}", resp.body);
    assert!(rec.sent().is_empty());
    assert_eq!(store_rows_for_inbox(&pool, &org, pod, &inbox).await, 0);
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

/// Unfold the first `DKIM-Signature` header and return the `h=` names.
fn dkim_h_headers(raw: &str) -> Vec<String> {
    let header_block = raw.split("\r\n\r\n").next().unwrap_or(raw);
    let mut dkim = String::new();
    let mut in_dkim = false;
    for line in header_block.split("\r\n") {
        if in_dkim && line.starts_with([' ', '\t']) {
            dkim.push_str(line.trim());
            continue;
        }
        if in_dkim {
            break;
        }
        if let Some(rest) = line
            .split_once(':')
            .filter(|(n, _)| n.eq_ignore_ascii_case("DKIM-Signature"))
            .map(|(_, v)| v)
        {
            dkim.push_str(rest.trim());
            in_dkim = true;
        }
    }
    for tag in dkim.split(';') {
        let tag = tag.trim();
        let Some((name, value)) = tag.split_once('=') else {
            continue;
        };
        if name.eq_ignore_ascii_case("h") {
            return value
                .split(':')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
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
            raw_blob_id: None,
            attachment_blobs: None,
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
            raw_blob_id: None,
            attachment_blobs: None,
        },
    )
    .await
    .expect("seed message");
    (thread_id, mid)
}
