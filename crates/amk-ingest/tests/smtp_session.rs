//! SMTP session cases. Every assertion is a reply code or a store row.

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use amk_core::scope::{Mount, Resolved, Scope};
use amk_ingest::{FixedInboxLookup, IngestConfig, StorePersist};
use amk_store::messages::{self, ListMessagesQuery};
use amk_store::pagination::SortDirection;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId};
use support::{
    mid, reply_code, seed_org_pod_inbox, short_pause_config, spawn_smtp, CountingPersist, MimeSpec,
    SmtpClient,
};

fn inbox_filter(org: &OrganizationId, pod: PodId, inbox: &InboxId) -> amk_core::scope::ScopeFilter {
    let scope = Scope::Inbox { organization_id: org.clone(), pod_id: pod, inbox_id: inbox.clone() };
    match scope.resolve(&Mount::Organization).unwrap() {
        Resolved::Ready(f) => f,
        Resolved::Probe(_) => panic!("expected Ready"),
    }
}

/// Case 1 / mutant 1: local_domains = ["local.test"], lookup stubbed Some for
/// alice@gmail.com → RCPT 550, persist never called.
#[tokio::test]
async fn open_relay_rcpt_is_550_and_store_empty() {
    let persist = CountingPersist::default();
    let mut lookup = FixedInboxLookup::new();
    lookup.insert(
        InboxId::new("alice@gmail.com"),
        OrganizationId::new("org-relay"),
        PodId::new_random(),
    );
    let addr =
        spawn_smtp(short_pause_config(&["local.test"], 64 * 1024), lookup, persist.clone()).await;

    let (mut c, banner) = SmtpClient::connect_after_banner(addr).await;
    assert!(banner.starts_with("220"), "banner {banner:?}");
    assert_eq!(reply_code(&c.cmd("EHLO client.test").await), 250);
    assert_eq!(reply_code(&c.cmd("MAIL FROM:<eve@evil.test>").await), 250);
    let rcpt = c.cmd("RCPT TO:<alice@gmail.com>").await;
    assert_eq!(reply_code(&rcpt), 550, "open relay must be 550, got {rcpt:?}");
    assert_eq!(persist.calls.load(Ordering::SeqCst), 0, "store must stay empty");
}

/// Case 2: domain is local, lookup None → RCPT 550, store empty.
#[tokio::test]
async fn unknown_local_user_rcpt_is_550_and_store_empty() {
    let persist = CountingPersist::default();
    let lookup = FixedInboxLookup::new();
    let addr =
        spawn_smtp(short_pause_config(&["local.test"], 64 * 1024), lookup, persist.clone()).await;

    let (mut c, _) = SmtpClient::connect_after_banner(addr).await;
    assert_eq!(reply_code(&c.cmd("EHLO client.test").await), 250);
    assert_eq!(reply_code(&c.cmd("MAIL FROM:<eve@evil.test>").await), 250);
    let rcpt = c.cmd("RCPT TO:<nobody@local.test>").await;
    assert_eq!(reply_code(&rcpt), 550, "unknown user must be 550, got {rcpt:?}");
    assert_eq!(persist.calls.load(Ordering::SeqCst), 0);
}

/// Case 3 / mutant 2: pipelined EHLO before greet_pause → 421; session never
/// reaches MAIL. After the pause, EHLO is 250.
#[tokio::test]
async fn pipelined_ehlo_before_greet_pause_is_421() {
    let persist = CountingPersist::default();
    let lookup = FixedInboxLookup::new();
    let config =
        IngestConfig::new("mx.test", &["local.test"], 64 * 1024, Duration::from_millis(250));
    let addr = spawn_smtp(config, lookup, persist.clone()).await;

    let mut early = SmtpClient::connect_raw(addr).await;
    let first = early.cmd("EHLO too-soon.test").await;
    assert_eq!(reply_code(&first), 421, "EHLO before greet-pause must be 421, got {first:?}");
    if let Ok(reply) = early.try_cmd("MAIL FROM:<a@b.test>").await {
        assert_ne!(reply_code(&reply), 250, "session must not reach MAIL after 421, got {reply:?}");
    }

    tokio::time::sleep(Duration::from_millis(280)).await;
    let (mut late, banner) = SmtpClient::connect_after_banner(addr).await;
    assert!(banner.starts_with("220"), "after pause the banner is 220, got {banner:?}");
    assert_eq!(
        reply_code(&late.cmd("EHLO after-pause.test").await),
        250,
        "EHLO after greet-pause is 250"
    );
}

/// Case 4: DATA of cap-1 and cap is 250 with stored size equal to DATA length;
/// cap+1 is 5xx and the store stays empty for that message.
#[tokio::test]
async fn size_cap_minus_one_and_cap_accepted_cap_plus_one_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let mut lookup = FixedInboxLookup::new();
    lookup.insert(inbox.clone(), org.clone(), pod);
    let auth = amk_ingest::Authenticator::unresolved_is_none().expect("auth");
    let persist = StorePersist { pool: pool.clone(), auth };
    const CAP: usize = 400;
    let addr = spawn_smtp(short_pause_config(&["local.test"], CAP), lookup, persist).await;

    let filter = inbox_filter(&org, pod, &inbox);

    for (tag, len) in [("under", CAP - 1), ("exact", CAP)] {
        let mid = mid(tag);
        let raw = sized_message(&mid, inbox.as_str(), len);
        assert_eq!(raw.len(), len);
        let (mut c, _) = SmtpClient::connect_after_banner(addr).await;
        assert_eq!(reply_code(&c.cmd("EHLO client.test").await), 250);
        assert_eq!(reply_code(&c.cmd("MAIL FROM:<a@probe.test>").await), 250);
        assert_eq!(reply_code(&c.cmd(&format!("RCPT TO:<{}>", inbox.as_str())).await), 250);
        let data = c.data(&raw).await;
        assert_eq!(reply_code(&data), 250, "{tag} DATA {len} must be 250, got {data:?}");
        let stored = messages::get(&pool, &filter, &inbox, &MessageId::new(&mid), &[])
            .await
            .unwrap()
            .expect("stored");
        assert_eq!(stored.item.size, len as u64, "{tag}: stored size must equal DATA length");
    }

    let mid_over = mid("over");
    let raw_over = sized_message(&mid_over, inbox.as_str(), CAP + 1);
    assert_eq!(raw_over.len(), CAP + 1);
    let (mut c, _) = SmtpClient::connect_after_banner(addr).await;
    assert_eq!(reply_code(&c.cmd("EHLO client.test").await), 250);
    assert_eq!(reply_code(&c.cmd("MAIL FROM:<a@probe.test>").await), 250);
    assert_eq!(reply_code(&c.cmd(&format!("RCPT TO:<{}>", inbox.as_str())).await), 250);
    let data = c.data(&raw_over).await;
    assert!((500..600).contains(&reply_code(&data)), "cap+1 must be 5xx, got {data:?}");
    assert!(
        messages::get(&pool, &filter, &inbox, &MessageId::new(&mid_over), &[])
            .await
            .unwrap()
            .is_none(),
        "oversize must store nothing"
    );

    let listed = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 50, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(listed.items.len(), 2, "only the two accepted sizes");
}

/// Case 11 via SMTP: missing Message-ID is DATA 554 and stores nothing.
#[tokio::test]
async fn missing_message_id_data_is_554_and_store_empty() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let mut lookup = FixedInboxLookup::new();
    lookup.insert(inbox.clone(), org.clone(), pod);
    let auth = amk_ingest::Authenticator::unresolved_is_none().expect("auth");
    let persist = StorePersist { pool: pool.clone(), auth };
    let addr = spawn_smtp(short_pause_config(&["local.test"], 64 * 1024), lookup, persist).await;

    let mut spec = MimeSpec::simple("a@probe.test", inbox.as_str(), "no mid", "<unused@x>", "body");
    spec.message_id = None;
    let raw = spec.render();

    let (mut c, _) = SmtpClient::connect_after_banner(addr).await;
    assert_eq!(reply_code(&c.cmd("EHLO client.test").await), 250);
    assert_eq!(reply_code(&c.cmd("MAIL FROM:<a@probe.test>").await), 250);
    assert_eq!(reply_code(&c.cmd(&format!("RCPT TO:<{}>", inbox.as_str())).await), 250);
    let data = c.data(&raw).await;
    assert_eq!(reply_code(&data), 554, "missing Message-ID is DATA 554, got {data:?}");

    let filter = inbox_filter(&org, pod, &inbox);
    let listed = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 50, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert!(listed.items.is_empty(), "store must stay empty");
}

fn sized_message(message_id: &str, to: &str, len: usize) -> Vec<u8> {
    let mut spec = MimeSpec::simple("a@probe.test", to, "size", message_id, "");
    spec.body.clear();
    let mut raw = spec.render();
    if raw.len() > len {
        panic!("headers already {0} bytes, cannot make {len}", raw.len());
    }
    raw.extend(std::iter::repeat_n(b'x', len - raw.len()));
    if len >= 2 {
        raw[len - 2] = b'\r';
        raw[len - 1] = b'\n';
    }
    raw
}
