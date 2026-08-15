//! Message/thread read-path tests: the two security rules (restricted-label predicate pushdown,
//! scope pinning), keyset pagination boundaries against a real database, and the assigned
//! `message_id` special-character round trip.

mod support;

use amk_core::labels::{excluded_labels, IncludeFlags, LabelAccess};
use amk_core::scope::{Mount, Resolved, Scope, ScopeFilter};
use amk_store::inboxes::{self, NewInbox};
use amk_store::messages::{self, ListMessagesQuery, NewMessage};
use amk_store::pagination::{MessageCursor, SortDirection, ThreadCursor};
use amk_store::threads::{self, ListThreadsQuery, NewThread};
use amk_store::{PageTokenError, StoreError};
use amk_types::api_key::{ApiKeyPermissions, KeyGrants};
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::Timestamp;
use chrono::{DateTime, Utc};

fn scope_filter(scope: &Scope, mount: &Mount) -> ScopeFilter {
    match scope.resolve(mount).unwrap() {
        Resolved::Ready(f) => f,
        Resolved::Probe(_) => panic!("expected a settled window for {mount:?}"),
    }
}

fn org_filter(org: &OrganizationId) -> ScopeFilter {
    scope_filter(&Scope::Organization { organization_id: org.clone() }, &Mount::Organization)
}

fn pod_filter(org: &OrganizationId, pod: PodId) -> ScopeFilter {
    scope_filter(&Scope::Pod { organization_id: org.clone(), pod_id: pod }, &Mount::Organization)
}

fn inbox_filter(org: &OrganizationId, pod: PodId, inbox: &InboxId) -> ScopeFilter {
    scope_filter(
        &Scope::Inbox { organization_id: org.clone(), pod_id: pod, inbox_id: inbox.clone() },
        &Mount::Organization,
    )
}

fn ts(s: &str) -> Timestamp {
    Timestamp::from(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
}

fn new_message(
    inbox: &InboxId,
    org: &OrganizationId,
    pod: PodId,
    thread_id: ThreadId,
    message_id: &str,
    labels: &[&str],
    timestamp: &str,
) -> NewMessage {
    NewMessage {
        inbox_id: inbox.clone(),
        message_id: MessageId::new(message_id),
        organization_id: org.clone(),
        pod_id: pod,
        thread_id,
        labels: labels.iter().map(|s| s.to_string()).collect(),
        timestamp: ts(timestamp),
        from: "sender@example.test".into(),
        to: vec![inbox.as_str().to_string()],
        cc: None,
        bcc: None,
        subject: Some("subject".into()),
        preview: Some("preview".into()),
        attachments: None,
        in_reply_to: None,
        references: None,
        headers: None,
        smtp_id: None,
        size: 100,
        reply_to: None,
        text: Some("body".into()),
        html: None,
        extracted_text: None,
        extracted_html: None,
    }
}

fn new_thread(
    inbox: &InboxId,
    org: &OrganizationId,
    pod: PodId,
    thread_id: ThreadId,
    labels: &[&str],
    timestamp: &str,
    last_message_id: &str,
) -> NewThread {
    NewThread {
        thread_id,
        organization_id: org.clone(),
        pod_id: pod,
        inbox_id: inbox.clone(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        timestamp: ts(timestamp),
        received_timestamp: None,
        sent_timestamp: None,
        senders: vec!["sender@example.test".into()],
        recipients: vec![inbox.as_str().to_string()],
        subject: Some("subject".into()),
        preview: Some("preview".into()),
        last_message_id: MessageId::new(last_message_id),
        message_count: 1,
        size: 100,
    }
}

#[tokio::test]
async fn message_id_round_trips_special_characters_through_storage() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    // <, >, @, +, %, /, and non-ASCII, all in one id.
    let weird_id = "<test+tag/seg%25ment@exämple.test>";
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod,
            thread_id,
            weird_id,
            &["received"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let fetched = messages::get(&pool, &filter, &inbox, &MessageId::new(weird_id), &[])
        .await
        .unwrap()
        .expect("must round trip");
    assert_eq!(
        fetched.item.message_id.as_str(),
        weird_id,
        "byte-exact round trip, brackets included"
    );
}

/// The regression this crate exists to prevent: `reference/fixtures/09b-unauthenticated-variant.txt`
/// — a restricted-label row must never appear in a paginated walk, and the walk must never show a
/// gap (an empty page carrying a token) at the position of the hidden row.
#[tokio::test]
async fn restricted_label_rows_are_absent_from_a_paginated_walk_with_no_gap() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    let rows: [(&str, &[&str], &str); 4] = [
        ("<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
        ("<b@x>", &["received", "unauthenticated"], "2026-08-15T05:00:02.000Z"),
        ("<c@x>", &["sent"], "2026-08-15T05:00:03.000Z"),
        ("<d@x>", &["sent"], "2026-08-15T05:00:04.000Z"),
    ];
    for (id, labels, when) in rows {
        messages::insert(&pool, new_message(&inbox, &org, pod, thread_id, id, labels, when))
            .await
            .unwrap();
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);

    let query =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor };

    let page1 = messages::list(&pool, &filter, &excluded, query(None))
        .await
        .unwrap();
    assert_eq!(page1.items.len(), 1);
    assert_eq!(page1.items[0].message_id.as_str(), "<a@x>");
    let cursor1 =
        MessageCursor::decode(page1.next.as_deref().expect("more rows remain"), filter.inbox_id())
            .unwrap();

    let page2 = messages::list(&pool, &filter, &excluded, query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(
        page2.items[0].message_id.as_str(),
        "<c@x>",
        "the hidden row must be skipped outright, not returned as an empty page"
    );
    let cursor2 =
        MessageCursor::decode(page2.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();

    let page3 = messages::list(&pool, &filter, &excluded, query(Some(cursor2)))
        .await
        .unwrap();
    assert_eq!(page3.items[0].message_id.as_str(), "<d@x>");
    assert!(
        page3.next.is_none(),
        "the last page must omit the token, never carry an empty-page token"
    );

    let walked: Vec<_> = [&page1, &page2, &page3]
        .iter()
        .flat_map(|p| p.items.iter().map(|m| m.message_id.as_str().to_string()))
        .collect();
    assert_eq!(
        walked,
        vec!["<a@x>", "<c@x>", "<d@x>"],
        "the unauthenticated message must never surface"
    );

    // Fixture 09b's asymmetry: get-by-id still surfaces it when the credential holds the permission.
    let permitted = KeyGrants::Restricted(ApiKeyPermissions {
        message_read: Some(true),
        label_unauthenticated_read: Some(true),
        ..Default::default()
    });
    let permitted_excluded = excluded_labels(&LabelAccess::by_id(&permitted));
    let visible =
        messages::get(&pool, &filter, &inbox, &MessageId::new("<b@x>"), &permitted_excluded)
            .await
            .unwrap();
    assert!(
        visible.is_some(),
        "get-by-id must surface it when the credential holds the permission"
    );

    // Without the permission, get-by-id masks it too (not_found — the caller renders that).
    let denied =
        KeyGrants::Restricted(ApiKeyPermissions { message_read: Some(true), ..Default::default() });
    let denied_excluded = excluded_labels(&LabelAccess::by_id(&denied));
    let masked = messages::get(&pool, &filter, &inbox, &MessageId::new("<b@x>"), &denied_excluded)
        .await
        .unwrap();
    assert!(masked.is_none());
}

/// Same scenario as [`restricted_label_rows_are_absent_from_a_paginated_walk_with_no_gap`], walked
/// in descending order — the ASC and DESC branches are two independent literal query strings (see
/// [`SortDirection`]'s docs), so nothing exercises the DESC exclusion/boundary unless a test asks
/// for it explicitly.
#[tokio::test]
async fn list_descending_also_excludes_restricted_labels_with_no_gap() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    let rows: [(&str, &[&str], &str); 4] = [
        ("<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
        ("<b@x>", &["received", "unauthenticated"], "2026-08-15T05:00:02.000Z"),
        ("<c@x>", &["sent"], "2026-08-15T05:00:03.000Z"),
        ("<d@x>", &["sent"], "2026-08-15T05:00:04.000Z"),
    ];
    for (id, labels, when) in rows {
        messages::insert(&pool, new_message(&inbox, &org, pod, thread_id, id, labels, when))
            .await
            .unwrap();
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);

    let query =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Descending, cursor };

    let page1 = messages::list(&pool, &filter, &excluded, query(None))
        .await
        .unwrap();
    assert_eq!(page1.items[0].message_id.as_str(), "<d@x>", "newest first, descending");
    let cursor1 =
        MessageCursor::decode(page1.next.as_deref().expect("more rows remain"), filter.inbox_id())
            .unwrap();

    let page2 = messages::list(&pool, &filter, &excluded, query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(
        page2.items[0].message_id.as_str(),
        "<c@x>",
        "walking backwards must skip the hidden row too, not surface it or leave a gap"
    );
    let cursor2 =
        MessageCursor::decode(page2.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();

    let page3 = messages::list(&pool, &filter, &excluded, query(Some(cursor2)))
        .await
        .unwrap();
    assert_eq!(page3.items[0].message_id.as_str(), "<a@x>");
    assert!(page3.next.is_none());
}

/// Isolates the `messages::list` inbox pin: two inboxes in the *same* pod, each with their own
/// message, and a request scoped to only one of them.
#[tokio::test]
async fn list_pins_inbox_within_the_same_pod() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox1 = support::seed_inbox(&pool, &org, pod, "i1").await;
    let inbox2 = support::seed_inbox(&pool, &org, pod, "i2").await;

    let t1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox1, &org, pod, t1, &["received"], "2026-08-15T05:00:00.000Z", "<i1@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox1, &org, pod, t1, "<i1@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod, t2, &["received"], "2026-08-15T05:00:00.000Z", "<i2@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox2, &org, pod, t2, "<i2@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox1);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = messages::list(
            &pool,
            &filter,
            &[],
            ListMessagesQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page
            .items
            .iter()
            .map(|m| m.message_id.as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            vec!["<i1@x>"],
            "inbox-scoped list must not see the sibling inbox's message ({direction:?})"
        );
    }
}

/// Isolates the `messages::get` organization pin: an org-scoped filter (no pod/inbox pinned, so
/// those NULL-sentinel checks pass trivially) must still refuse a message that lives in a
/// *different* organization, even though the caller names that message's real inbox_id directly.
#[tokio::test]
async fn get_pins_organization_even_when_the_named_inbox_belongs_to_another_org() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;

    let thread_a = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter_b = org_filter(&org_b);
    let leaked = messages::get(&pool, &filter_b, &inbox_a, &MessageId::new("<a@x>"), &[])
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "org B must not read org A's message by naming org A's inbox directly"
    );
}

/// Isolates the `messages::get` inbox pin: same org and pod, but the message actually lives under
/// a *different* inbox than the one named in the call.
#[tokio::test]
async fn get_pins_the_named_inbox_not_just_the_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let right_inbox = support::seed_inbox(&pool, &org, pod, "right").await;
    let wrong_inbox = support::seed_inbox(&pool, &org, pod, "wrong").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &right_inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<r@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &right_inbox,
            &org,
            pod,
            thread_id,
            "<r@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod);
    let leaked = messages::get(&pool, &filter, &wrong_inbox, &MessageId::new("<r@x>"), &[])
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "naming the wrong inbox must not surface a message that lives elsewhere"
    );
}

/// `messages::get`'s `inbox_id` *parameter* (not just `ScopeFilter`) must be folded to its
/// normalized form before comparison — fixture 18.
#[tokio::test]
async fn get_normalizes_a_case_variant_inbox_id_parameter() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mixed = format!("MixedGet-{}@Example.Test", support::unique_suffix());
    let inbox = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(mixed.clone()),
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap()
    .inbox_id;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<m@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<m@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod);
    let found = messages::get(&pool, &filter, &InboxId::new(mixed), &MessageId::new("<m@x>"), &[])
        .await
        .unwrap();
    assert!(
        found.is_some(),
        "a mixed-case inbox_id parameter must still resolve to the stored inbox"
    );
}

/// Isolates the `threads::list` organization pin, mirroring
/// [`list_never_leaks_across_organizations_even_at_the_org_mount`] for messages.
#[tokio::test]
async fn thread_list_never_leaks_across_organizations() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let (org_b, pod_b, inbox_b) = support::seed_org_pod_inbox(&pool).await;

    let thread_a = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    let thread_b = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_b,
            &org_b,
            pod_b,
            thread_b,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<b@x>",
        ),
    )
    .await
    .unwrap();

    let filter_a = org_filter(&org_a);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = threads::list(
            &pool,
            &filter_a,
            &[],
            ListThreadsQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page.items.iter().map(|t| t.thread_id).collect();
        assert_eq!(
            ids,
            vec![thread_a],
            "org A's thread list must not see org B's thread ({direction:?})"
        );
    }
}

/// Isolates the `threads::get_with_messages` organization pin on its own item lookup.
#[tokio::test]
async fn thread_get_with_messages_pins_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;

    let thread_a = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();

    let filter_b = org_filter(&org_b);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let leaked = threads::get_with_messages(&pool, &filter_b, thread_a, &access)
        .await
        .unwrap();
    assert!(leaked.is_none(), "org B must not read org A's thread by naming its id directly");
}

#[tokio::test]
async fn list_never_leaks_across_organizations_even_at_the_org_mount() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let (org_b, pod_b, inbox_b) = support::seed_org_pod_inbox(&pool).await;

    let thread_a = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_a,
            &org_a,
            pod_a,
            thread_a,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let thread_b = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_b,
            &org_b,
            pod_b,
            thread_b,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<b@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_b,
            &org_b,
            pod_b,
            thread_b,
            "<b@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    // The org mount pins nothing but organization_id — pod_id and inbox_id are both unpinned, so
    // this is the case where dropping the organization pin would actually change the result.
    let filter_a = org_filter(&org_a);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = messages::list(
            &pool,
            &filter_a,
            &[],
            ListMessagesQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page
            .items
            .iter()
            .map(|m| m.message_id.as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            vec!["<a@x>"],
            "org A's org-level list must not see org B's message ({direction:?})"
        );
    }
}

#[tokio::test]
async fn list_pins_pod_within_the_same_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod1 = support::seed_pod(&pool, &org).await;
    let pod2 = support::seed_pod(&pool, &org).await;
    let inbox1 = support::seed_inbox(&pool, &org, pod1, "p1").await;
    let inbox2 = support::seed_inbox(&pool, &org, pod2, "p2").await;

    let t1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox1, &org, pod1, t1, &["received"], "2026-08-15T05:00:00.000Z", "<p1@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox1, &org, pod1, t1, "<p1@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod2, t2, &["received"], "2026-08-15T05:00:00.000Z", "<p2@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox2, &org, pod2, t2, "<p2@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod1);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = messages::list(
            &pool,
            &filter,
            &[],
            ListMessagesQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page
            .items
            .iter()
            .map(|m| m.message_id.as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            vec!["<p1@x>"],
            "pod-scoped list must not see the sibling pod's message ({direction:?})"
        );
    }
}

/// A token replayed after the row it was minted from was deleted must still resume correctly —
/// the property that makes keyset pagination, not OFFSET, the schema decision.
#[tokio::test]
async fn a_page_token_survives_deletion_of_the_row_it_points_past() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    for (id, when) in [
        ("<a@x>", "2026-08-15T05:00:01.000Z"),
        ("<b@x>", "2026-08-15T05:00:02.000Z"),
        ("<c@x>", "2026-08-15T05:00:03.000Z"),
    ] {
        messages::insert(&pool, new_message(&inbox, &org, pod, thread_id, id, &["sent"], when))
            .await
            .unwrap();
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let page1 = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page1.items[0].message_id.as_str(), "<a@x>");
    let token = page1.next.clone().unwrap();

    // Delete the very row the resumed scan will land on next — proving resumption does not depend
    // on that row still existing.
    sqlx::query("DELETE FROM messages WHERE inbox_id = $1 AND message_id = $2")
        .bind(inbox.normalized().as_str())
        .bind("<b@x>")
        .execute(&pool)
        .await
        .unwrap();

    let cursor = MessageCursor::decode(&token, filter.inbox_id()).unwrap();
    let page2 = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: Some(cursor) },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page2
        .items
        .iter()
        .map(|m| m.message_id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["<c@x>"],
        "resuming past a deleted row must skip it cleanly: no error, no duplicate, no gap"
    );
}

#[tokio::test]
async fn a_cursor_minted_for_one_inbox_is_rejected_against_another() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "other").await;

    let cursor = MessageCursor {
        message_id: MessageId::new("<x@y>"),
        inbox_id: inbox_a,
        timestamp: Utc::now(),
    };
    let token = cursor.encode();

    let filter_b = inbox_filter(&org, pod, &inbox_b);
    let err = MessageCursor::decode(&token, filter_b.inbox_id()).unwrap_err();
    assert_eq!(err, PageTokenError::WrongScope);
}

/// Mirrors [`restricted_label_rows_are_absent_from_a_paginated_walk_with_no_gap`] for threads:
/// `limit: 1` forces a genuine multi-page walk across the hidden row, so the keyset comparison and
/// `limit + 1` fetch in `LIST_ASC_SQL` are actually exercised (the previous version of this test
/// used `limit: 10` against 3 rows and never triggered pagination at all, despite its name).
#[tokio::test]
async fn thread_list_excludes_restricted_labels_with_no_gap() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let rows: [(&str, &[&str], &str); 4] = [
        ("<a@x>", &["received"], "2026-08-15T05:00:01.000Z"),
        ("<b@x>", &["received", "trash"], "2026-08-15T05:00:02.000Z"),
        ("<c@x>", &["received"], "2026-08-15T05:00:03.000Z"),
        ("<d@x>", &["received"], "2026-08-15T05:00:04.000Z"),
    ];
    let mut thread_ids = Vec::new();
    for (last_message_id, labels, when) in rows {
        let thread_id = ThreadId::new_random();
        threads::insert(
            &pool,
            new_thread(&inbox, &org, pod, thread_id, labels, when, last_message_id),
        )
        .await
        .unwrap();
        thread_ids.push(thread_id);
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);

    let query = |cursor| ListThreadsQuery { limit: 1, direction: SortDirection::Ascending, cursor };

    let page1 = threads::list(&pool, &filter, &excluded, query(None))
        .await
        .unwrap();
    assert_eq!(page1.items[0].thread_id, thread_ids[0]);
    let cursor1 =
        ThreadCursor::decode(page1.next.as_deref().expect("more rows remain"), filter.inbox_id())
            .unwrap();

    let page2 = threads::list(&pool, &filter, &excluded, query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(
        page2.items[0].thread_id, thread_ids[2],
        "the trashed thread must be skipped outright, not returned as an empty page"
    );
    let cursor2 =
        ThreadCursor::decode(page2.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();

    let page3 = threads::list(&pool, &filter, &excluded, query(Some(cursor2)))
        .await
        .unwrap();
    assert_eq!(page3.items[0].thread_id, thread_ids[3]);
    assert!(
        page3.next.is_none(),
        "the last page must omit the token, never carry an empty-page token"
    );
}

/// Same scenario as [`thread_list_excludes_restricted_labels_with_no_gap`], walked in descending
/// order — mirrors [`list_descending_also_excludes_restricted_labels_with_no_gap`] for messages.
/// `threads::list`'s ASC and DESC branches are two independent literal query strings (see
/// [`SortDirection`]'s docs), so nothing exercises `LIST_DESC_SQL`'s org/pod/inbox pins, label
/// predicate, or keyset comparison unless a test asks for it explicitly.
#[tokio::test]
async fn thread_list_descending_also_excludes_restricted_labels_with_no_gap() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let rows: [(&str, &[&str], &str); 4] = [
        ("<a@x>", &["received"], "2026-08-15T05:00:01.000Z"),
        ("<b@x>", &["received", "unauthenticated"], "2026-08-15T05:00:02.000Z"),
        ("<c@x>", &["received"], "2026-08-15T05:00:03.000Z"),
        ("<d@x>", &["received"], "2026-08-15T05:00:04.000Z"),
    ];
    let mut thread_ids = Vec::new();
    for (last_message_id, labels, when) in rows {
        let thread_id = ThreadId::new_random();
        threads::insert(
            &pool,
            new_thread(&inbox, &org, pod, thread_id, labels, when, last_message_id),
        )
        .await
        .unwrap();
        thread_ids.push(thread_id);
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);

    let query =
        |cursor| ListThreadsQuery { limit: 1, direction: SortDirection::Descending, cursor };

    let page1 = threads::list(&pool, &filter, &excluded, query(None))
        .await
        .unwrap();
    assert_eq!(page1.items[0].thread_id, thread_ids[3], "newest first, descending");
    let cursor1 =
        ThreadCursor::decode(page1.next.as_deref().expect("more rows remain"), filter.inbox_id())
            .unwrap();

    let page2 = threads::list(&pool, &filter, &excluded, query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(
        page2.items[0].thread_id, thread_ids[2],
        "walking backwards must skip the hidden row too, not surface it or leave a gap"
    );
    let cursor2 =
        ThreadCursor::decode(page2.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();

    let page3 = threads::list(&pool, &filter, &excluded, query(Some(cursor2)))
        .await
        .unwrap();
    assert_eq!(page3.items[0].thread_id, thread_ids[0]);
    assert!(page3.next.is_none());
}

/// Isolates the `threads::list` inbox pin, mirroring [`list_pins_inbox_within_the_same_pod`] for
/// messages, in both directions.
#[tokio::test]
async fn thread_list_pins_inbox_within_the_same_pod() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox1 = support::seed_inbox(&pool, &org, pod, "i1").await;
    let inbox2 = support::seed_inbox(&pool, &org, pod, "i2").await;

    let t1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox1, &org, pod, t1, &["received"], "2026-08-15T05:00:00.000Z", "<i1@x>"),
    )
    .await
    .unwrap();
    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod, t2, &["received"], "2026-08-15T05:00:00.000Z", "<i2@x>"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox1);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = threads::list(
            &pool,
            &filter,
            &[],
            ListThreadsQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page.items.iter().map(|t| t.thread_id).collect();
        assert_eq!(
            ids,
            vec![t1],
            "inbox-scoped list must not see the sibling inbox's thread ({direction:?})"
        );
    }
}

/// Isolates the `threads::list` pod pin, mirroring [`list_pins_pod_within_the_same_organization`]
/// for messages, in both directions.
#[tokio::test]
async fn thread_list_pins_pod_within_the_same_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod1 = support::seed_pod(&pool, &org).await;
    let pod2 = support::seed_pod(&pool, &org).await;
    let inbox1 = support::seed_inbox(&pool, &org, pod1, "p1").await;
    let inbox2 = support::seed_inbox(&pool, &org, pod2, "p2").await;

    let t1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox1, &org, pod1, t1, &["received"], "2026-08-15T05:00:00.000Z", "<p1@x>"),
    )
    .await
    .unwrap();
    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod2, t2, &["received"], "2026-08-15T05:00:00.000Z", "<p2@x>"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod1);
    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let page = threads::list(
            &pool,
            &filter,
            &[],
            ListThreadsQuery { limit: 10, direction, cursor: None },
        )
        .await
        .unwrap();
        let ids: Vec<_> = page.items.iter().map(|t| t.thread_id).collect();
        assert_eq!(
            ids,
            vec![t1],
            "pod-scoped list must not see the sibling pod's thread ({direction:?})"
        );
    }
}

#[tokio::test]
async fn thread_get_with_messages_redacts_hidden_members_and_recomputes_aggregates() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:03.000Z",
            "<c@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod,
            thread_id,
            "<a@x>",
            &["received"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod,
            thread_id,
            "<b@x>",
            &["received", "spam"],
            "2026-08-15T05:00:02.000Z",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod,
            thread_id,
            "<c@x>",
            &["received"],
            "2026-08-15T05:00:03.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let grants =
        KeyGrants::Restricted(ApiKeyPermissions { message_read: Some(true), ..Default::default() });
    let access = LabelAccess::by_id(&grants);

    let thread = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap()
        .expect("two of three members are visible");
    let ids: Vec<_> = thread
        .messages
        .iter()
        .map(|m| m.item.message_id.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["<a@x>", "<c@x>"], "the spam member must be stripped");
    assert_eq!(
        thread.item.message_count, 2,
        "the aggregate must be recomputed, not left counting the hidden member"
    );
    assert_eq!(thread.item.last_message_id.as_str(), "<c@x>");
}

#[tokio::test]
async fn thread_get_with_messages_is_withheld_when_every_member_is_hidden() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received", "spam"],
            "2026-08-15T05:00:01.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod,
            thread_id,
            "<a@x>",
            &["received", "spam"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let grants =
        KeyGrants::Restricted(ApiKeyPermissions { message_read: Some(true), ..Default::default() });
    let access = LabelAccess::by_id(&grants);

    let thread = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap();
    assert!(thread.is_none(), "a thread with no visible member must mask as not_found");
}

// --- Get-path pins: org pin drops are already covered above (get_pins_organization_even_when...,
// thread_get_with_messages_pins_organization); the four tests below close the *pod*-pin gap on
// the same get-paths, plus the org pin specifically on THREAD_MESSAGES_SQL. ---
//
// An earlier version of this section reasoned that dropping these pod pins "changes nothing
// observable today, because inbox_id/thread_id are global primary keys" and treated them as
// unobservable, defence-in-depth-only. Mutation testing against the real database proved that
// reasoning wrong: it conflated *row uniqueness* (inbox_id/thread_id pin the query to at most one
// row) with *access control* (whether the caller's own claimed pod is allowed to read that row) —
// two different properties. A credential scoped to one pod can still name a sibling pod's real
// inbox/thread/message *id* directly (ids are not secret), and if the pod pin is dropped, the
// already-uniquely-identified row is returned anyway, regardless of which pod issued the request.
// The four tests below construct exactly that: same organization, wrong pod, a real id named
// directly. All four fail (as they must) the moment the corresponding pod pin is removed.

/// Isolates `messages::get`'s pod pin: same organization, but the credential's scope names a
/// *different* pod than the one the message actually lives in, while still naming the message's
/// real inbox_id and message_id directly.
#[tokio::test]
async fn get_pins_the_pod_not_just_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_real = support::seed_pod(&pool, &org).await;
    let pod_wrong = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod_real, "real").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod_real,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<r@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            pod_real,
            thread_id,
            "<r@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod_wrong);
    let leaked = messages::get(&pool, &filter, &inbox, &MessageId::new("<r@x>"), &[])
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "a pod-scoped credential must not read a message living in a sibling pod, even naming it \
         directly"
    );
}

/// Isolates `threads::get_with_messages`'s item-lookup pod pin (`GET_ITEM_SQL`), mirroring
/// [`get_pins_the_pod_not_just_the_organization`] for threads.
#[tokio::test]
async fn thread_get_with_messages_pins_the_pod_not_just_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_real = support::seed_pod(&pool, &org).await;
    let pod_wrong = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod_real, "real").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod_real,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<r@x>",
        ),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod_wrong);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let leaked = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "a pod-scoped credential must not read a thread living in a sibling pod, even naming it \
         directly"
    );
}

/// Isolates `threads::get_with_messages`'s messages sub-query pins (`THREAD_MESSAGES_SQL`) against
/// a *rogue row*: `messages.organization_id`/`pod_id` are independent foreign keys with no
/// composite constraint tying them to the referenced thread's own organization/pod, so nothing in
/// the schema stops a message row from carrying a `thread_id` that names one thread while its own
/// `organization_id`/`pod_id` name a different tenant. That divergence is exactly what these two
/// pins guard against — dropping either lets the rogue row leak into every reader of the real
/// thread. (Two tests, not one: each mutates only its own coordinate so a single dropped pin is
/// independently attributable.)
#[tokio::test]
async fn thread_get_with_messages_sub_query_pins_organization_against_a_mismatched_message_row() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;
    let pod_b = support::seed_pod(&pool, &org_b).await;
    let inbox_b = support::seed_inbox(&pool, &org_b, pod_b, "rogue").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_a,
            &org_a,
            pod_a,
            thread_id,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();
    // Correct thread_id, but organization_id/pod_id/inbox_id all name a different tenant.
    messages::insert(
        &pool,
        new_message(
            &inbox_b,
            &org_b,
            pod_b,
            thread_id,
            "<rogue@x>",
            &["sent"],
            "2026-08-15T05:00:02.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = org_filter(&org_a);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let thread = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap()
        .expect("org_a's own thread must still be found");
    let ids: Vec<_> = thread
        .messages
        .iter()
        .map(|m| m.item.message_id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["<a@x>"],
        "a message row whose organization_id diverges from the thread's own org must never \
         surface, even though its thread_id matches"
    );
}

/// Pod-pin sibling of
/// [`thread_get_with_messages_sub_query_pins_organization_against_a_mismatched_message_row`]: same
/// organization throughout, but the rogue row's pod_id/inbox_id name a different pod.
#[tokio::test]
async fn thread_get_with_messages_sub_query_pins_the_pod_against_a_mismatched_message_row() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_real = support::seed_pod(&pool, &org).await;
    let pod_wrong = support::seed_pod(&pool, &org).await;
    let inbox_real = support::seed_inbox(&pool, &org, pod_real, "real").await;
    let inbox_wrong = support::seed_inbox(&pool, &org, pod_wrong, "wrong").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_real,
            &org,
            pod_real,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_real,
            &org,
            pod_real,
            thread_id,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();
    // Correct organization_id, but pod_id/inbox_id name a sibling pod within that same org.
    messages::insert(
        &pool,
        new_message(
            &inbox_wrong,
            &org,
            pod_wrong,
            thread_id,
            "<rogue@x>",
            &["sent"],
            "2026-08-15T05:00:02.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod_real);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let thread = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap()
        .expect("the real thread must still be found");
    let ids: Vec<_> = thread
        .messages
        .iter()
        .map(|m| m.item.message_id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["<a@x>"],
        "a message row whose pod_id diverges from the thread's own pod must never surface, even \
         though its thread_id matches"
    );
}

// --- Global-uniqueness invariants: independent of the access-control tests above. inbox_id and
// thread_id are relied on elsewhere (e.g. inbox_id folding — fixture 18 — and thread membership)
// to never collide across organizations; these are tripwires for that schema assumption, not a
// claim that any pin above is redundant. ---

/// `inboxes.inbox_id` is a *global* primary key: unique across every organization, not
/// per-organization. If usernames are ever scoped per-organization instead, this test starts
/// failing — and every place in this module that assumes a directly-named inbox_id is unambiguous
/// (e.g. the `get_pins_*` tests above) would need re-auditing.
#[tokio::test]
async fn inbox_id_is_globally_unique_across_organizations() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org_a = support::seed_org(&pool).await;
    let org_b = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org_a).await;
    let pod_b = support::seed_pod(&pool, &org_b).await;
    let username = format!("global-{}@example.test", support::unique_suffix());

    let new = |org: &OrganizationId, pod: PodId| NewInbox {
        inbox_id: InboxId::new(username.clone()),
        organization_id: org.clone(),
        pod_id: pod,
        client_id: None,
        display_name: None,
        metadata: None,
    };

    let first = inboxes::create(&pool, new(&org_a, pod_a)).await;
    assert!(first.is_ok(), "seeding the first organization's inbox must succeed");

    let second = inboxes::create(&pool, new(&org_b, pod_b)).await;
    assert!(
        matches!(second, Err(StoreError::InboxAlreadyExists)),
        "inbox_id must collide globally, even across organizations"
    );
}

/// `threads.thread_id` is a global UUID primary key: not scoped or re-minted per organization.
#[tokio::test]
async fn thread_id_is_a_global_primary_key_across_organizations() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org_a,
            pod_a,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<a@x>",
        ),
    )
    .await
    .unwrap();

    // Reuse the SAME thread_id under a different organization/pod/inbox — this must violate the
    // primary key, proving thread_id cannot collide across organizations.
    let (org_b, pod_b, inbox_b) = support::seed_org_pod_inbox(&pool).await;
    let collision = threads::insert(
        &pool,
        new_thread(
            &inbox_b,
            &org_b,
            pod_b,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<b@x>",
        ),
    )
    .await;
    assert!(
        collision.is_err(),
        "thread_id must be a global primary key: reusing it under a different organization must \
         fail rather than silently succeed"
    );
}

// --- Insert-path normalization: every existing test seeds through `inboxes::create`, whose
// return value is already normalized, so `messages::insert` and `threads::insert` never see a raw
// mixed-case InboxId in the rest of the suite. These pass one directly. ---

#[tokio::test]
async fn message_insert_normalizes_a_mixed_case_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    let mixed_inbox = InboxId::new(inbox.as_str().to_uppercase());
    messages::insert(
        &pool,
        new_message(
            &mixed_inbox,
            &org,
            pod,
            thread_id,
            "<m@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let found = messages::get(&pool, &filter, &inbox, &MessageId::new("<m@x>"), &[])
        .await
        .unwrap();
    assert!(
        found.is_some(),
        "messages::insert must normalize a mixed-case inbox_id before storing, not just rely on \
         inboxes::create's own normalization"
    );
}

#[tokio::test]
async fn thread_insert_normalizes_a_mixed_case_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let mixed_inbox = InboxId::new(inbox.as_str().to_uppercase());
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &mixed_inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let page = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page.items.iter().map(|t| t.thread_id).collect();
    assert_eq!(
        ids,
        vec![thread_id],
        "threads::insert must normalize a mixed-case inbox_id before storing, so a \
         normalized-inbox-scoped list still finds it"
    );
}

/// Mirrors [`a_page_token_survives_deletion_of_the_row_it_points_past`] but for insertion: a row
/// landing in the still-unscanned region (a timestamp between the cursor and the next existing
/// row) after the token was minted must appear exactly once on the resumed page — neither skipped
/// nor duplicated.
#[tokio::test]
async fn a_page_token_is_unaffected_by_a_row_inserted_after_it_was_minted() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    for (id, when) in [
        ("<a@x>", "2026-08-15T05:00:01.000Z"),
        ("<b@x>", "2026-08-15T05:00:02.000Z"),
        ("<d@x>", "2026-08-15T05:00:04.000Z"),
    ] {
        messages::insert(&pool, new_message(&inbox, &org, pod, thread_id, id, &["sent"], when))
            .await
            .unwrap();
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let page1 = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page1.items[0].message_id.as_str(), "<a@x>");
    let token = page1.next.clone().unwrap();

    // Insert a row that sorts strictly between the cursor position (<a@x>) and the next existing
    // row (<b@x>), after the token was already minted.
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<a2@x>", &["sent"], "2026-08-15T05:00:01.500Z"),
    )
    .await
    .unwrap();

    let cursor = MessageCursor::decode(&token, filter.inbox_id()).unwrap();
    let page2 = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: Some(cursor) },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page2
        .items
        .iter()
        .map(|m| m.message_id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["<a2@x>", "<b@x>", "<d@x>"],
        "a row inserted into the still-unscanned region after the token was minted must appear \
         exactly once on the resumed page, never skipped or duplicated"
    );
}
