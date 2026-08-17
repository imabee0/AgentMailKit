//! Message/thread read-path tests: the two security rules (restricted-label predicate pushdown,
//! scope pinning), keyset pagination boundaries against a real database, and the assigned
//! `message_id` special-character round trip.

mod support;

use amk_core::labels::{excluded_labels, IncludeFlags, LabelAccess};
use amk_core::scope::{Mount, Resolved, Scope, ScopeFilter};
use amk_store::api_keys::{self, KeyScope, NewApiKey};
use amk_store::inboxes::{self, NewInbox};
use amk_store::messages::{self, ListMessagesQuery, NewMessage};
use amk_store::pagination::{MessageCursor, SortDirection, ThreadCursor};
use amk_store::pods;
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

/// `messages::get`'s `inbox_id` *parameter* must not be able to widen `filter.inbox_id()`'s own
/// pin: the scope is pinned to `inbox_a`, but the caller's parameter names `inbox_b` (a real
/// message) directly. Neither may win over the other — the row must satisfy both, and no row can,
/// so the fetch returns nothing. `Scope::resolve` happens to keep the two equal today, but A2's
/// fix does not depend on that: the query itself binds and checks both.
#[tokio::test]
async fn get_does_not_let_the_inbox_id_parameter_widen_the_scopes_own_pin() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox_a = support::seed_inbox(&pool, &org, pod, "a").await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "b").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_b,
            &org,
            pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<b@x>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox_b, &org, pod, thread_id, "<b@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox_a);
    let leaked = messages::get(&pool, &filter, &inbox_b, &MessageId::new("<b@x>"), &[])
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "an inbox_id parameter naming a DIFFERENT inbox than the scope's own pin must not widen \
         the scope"
    );
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

// --- Round 3: threads::get_with_messages's inbox pin (A1's fix), identity of the returned
// thread, and sibling-thread isolation of its messages sub-query. ---

/// Isolates `threads::get_with_messages`'s item-lookup inbox pin (`GET_ITEM_SQL` already had
/// this pin; nothing tested it before now): same org and pod, but the thread actually lives in a
/// *different* inbox than the one named in the inbox-scoped filter.
#[tokio::test]
async fn thread_get_with_messages_pins_the_inbox_not_just_the_scope() {
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

    let filter = inbox_filter(&org, pod, &wrong_inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let leaked = threads::get_with_messages(&pool, &filter, thread_id, &access)
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "an inbox-scoped credential must not read a thread living in a sibling inbox, even \
         naming it directly"
    );
}

/// Isolates `THREAD_MESSAGES_SQL`'s inbox pin (added by A1: the sub-query previously pinned only
/// organization and pod) against a rogue row: correct organization_id/pod_id, but inbox_id names
/// a sibling inbox within the same pod, while its thread_id still matches the real thread.
#[tokio::test]
async fn thread_get_with_messages_sub_query_pins_the_inbox_against_a_mismatched_message_row() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox_real = support::seed_inbox(&pool, &org, pod, "real").await;
    let inbox_wrong = support::seed_inbox(&pool, &org, pod, "wrong").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_real,
            &org,
            pod,
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
            pod,
            thread_id,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();
    // Correct organization_id/pod_id, but inbox_id names a sibling inbox within the same pod.
    messages::insert(
        &pool,
        new_message(
            &inbox_wrong,
            &org,
            pod,
            thread_id,
            "<rogue@x>",
            &["sent"],
            "2026-08-15T05:00:02.000Z",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox_real);
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
        "a message row whose inbox_id diverges from the thread's own inbox must never surface, \
         even though its thread_id matches"
    );
}

/// `threads::get_with_messages` must return the *specific* thread requested, not merely a thread
/// somewhere in scope: two threads in the same inbox, request the second one, assert the item's
/// own `thread_id` is the one requested.
#[tokio::test]
async fn thread_get_with_messages_returns_the_requested_thread_not_any_thread_in_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let thread_1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_1,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<one@x>",
        ),
    )
    .await
    .unwrap();
    let thread_2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            thread_2,
            &["received"],
            "2026-08-15T05:00:01.000Z",
            "<two@x>",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let thread = threads::get_with_messages(&pool, &filter, thread_2, &access)
        .await
        .unwrap()
        .expect("thread_2 exists");
    assert_eq!(
        thread.item.thread_id, thread_2,
        "must return the specific thread requested, not merely a thread in scope"
    );
}

/// `THREAD_MESSAGES_SQL`'s `thread_id` predicate must exclude a sibling thread's messages, not
/// return every message in scope as this thread's membership.
#[tokio::test]
async fn thread_get_with_messages_sub_query_excludes_a_sibling_threads_messages() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let thread_1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, thread_1, &["received"], "2026-08-15T05:00:00.000Z", "<a@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_1, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let thread_2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, thread_2, &["received"], "2026-08-15T05:00:02.000Z", "<b@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_2, "<b@x>", &["sent"], "2026-08-15T05:00:03.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    let thread = threads::get_with_messages(&pool, &filter, thread_1, &access)
        .await
        .unwrap()
        .expect("thread_1 exists");
    let ids: Vec<_> = thread
        .messages
        .iter()
        .map(|m| m.item.message_id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["<a@x>"],
        "a sibling thread's message must never surface as this thread's membership"
    );
}

// --- Round 3: A4 regression (the messages keyset now includes inbox_id) and the general
// timestamp-tiebreak gap the review named — every prior test used distinct timestamps, so the
// `ORDER BY`'s final tiebreak column was never exercised by an actual walk across a tie. ---

/// The regression A4 fixes: two different inboxes (same org/pod) each holding a message with the
/// *same* Message-ID at the *same* millisecond — legal, since a Message-ID is only guaranteed
/// unique within one inbox (0005's header comment; one message addressed to two of an org's own
/// addresses is an ordinary case). At the pod mount, `inbox_id` is unpinned, so without it in the
/// keyset tiebreak `(timestamp, message_id)` is not a total order and a `limit: 1` walk can drop
/// one of the two rows silently. Both must be seen, exactly once each.
#[tokio::test]
async fn list_at_the_pod_mount_uses_inbox_id_to_break_a_timestamp_tie_across_inboxes() {
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
        new_thread(&inbox1, &org, pod, t1, &["received"], "2026-08-15T05:00:00.000Z", "<shared@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox1, &org, pod, t1, "<shared@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod, t2, &["received"], "2026-08-15T05:00:00.000Z", "<shared@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox2, &org, pod, t2, "<shared@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod);
    let query =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor };
    let page1 = messages::list(&pool, &filter, &[], query(None))
        .await
        .unwrap();
    assert_eq!(page1.items.len(), 1);
    let cursor1 = MessageCursor::decode(
        page1.next.as_deref().expect("second row remains"),
        filter.inbox_id(),
    )
    .unwrap();
    let page2 = messages::list(&pool, &filter, &[], query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(page2.items.len(), 1);
    assert!(page2.next.is_none(), "the last page must omit the token");

    let seen: std::collections::BTreeSet<_> = [&page1, &page2]
        .iter()
        .flat_map(|p| p.items.iter().map(|m| m.inbox_id.clone()))
        .collect();
    assert_eq!(
        seen.len(),
        2,
        "both same-timestamp, same-message_id rows from different inboxes must be seen exactly \
         once each, not dropped or duplicated"
    );
}

/// Descending sibling of [`list_at_the_pod_mount_uses_inbox_id_to_break_a_timestamp_tie_across_inboxes`]:
/// `LIST_ASC_SQL` and `LIST_DESC_SQL` are two independent literal query strings, so an ASC-only
/// regression test cannot prove A4's fix reached the DESC branch too.
#[tokio::test]
async fn list_at_the_pod_mount_uses_inbox_id_to_break_a_timestamp_tie_across_inboxes_descending() {
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
        new_thread(&inbox1, &org, pod, t1, &["received"], "2026-08-15T05:00:00.000Z", "<shared@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox1, &org, pod, t1, "<shared@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox2, &org, pod, t2, &["received"], "2026-08-15T05:00:00.000Z", "<shared@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox2, &org, pod, t2, "<shared@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod);
    let query =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Descending, cursor };
    let page1 = messages::list(&pool, &filter, &[], query(None))
        .await
        .unwrap();
    assert_eq!(page1.items.len(), 1);
    let cursor1 = MessageCursor::decode(
        page1.next.as_deref().expect("second row remains"),
        filter.inbox_id(),
    )
    .unwrap();
    let page2 = messages::list(&pool, &filter, &[], query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(page2.items.len(), 1);
    assert!(page2.next.is_none(), "the last page must omit the token");

    let seen: std::collections::BTreeSet<_> = [&page1, &page2]
        .iter()
        .flat_map(|p| p.items.iter().map(|m| m.inbox_id.clone()))
        .collect();
    assert_eq!(
        seen.len(),
        2,
        "both same-timestamp, same-message_id rows from different inboxes must be seen exactly \
         once each, not dropped or duplicated, descending"
    );
}

/// The general tiebreak gap: two messages in the *same* inbox at the exact same timestamp — every
/// other test in this file uses distinct timestamps, so `message_id` (the keyset's final
/// tiebreaker) was never exercised by an actual walk across a tie.
#[tokio::test]
async fn list_breaks_a_timestamp_tie_by_message_id() {
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

    // Inserted out of message_id order, at the exact same timestamp: the ORDER BY, not insertion
    // order, must decide the walk.
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<b@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let query =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor };
    let page1 = messages::list(&pool, &filter, &[], query(None))
        .await
        .unwrap();
    assert_eq!(page1.items[0].message_id.as_str(), "<a@x>", "message_id breaks a timestamp tie");
    let cursor1 =
        MessageCursor::decode(page1.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();
    let page2 = messages::list(&pool, &filter, &[], query(Some(cursor1)))
        .await
        .unwrap();
    assert_eq!(page2.items[0].message_id.as_str(), "<b@x>");
    assert!(page2.next.is_none());
}

/// Thread-side sibling of [`list_breaks_a_timestamp_tie_by_message_id`]: `thread_id` is a random
/// UUID (no predictable ordering), so this proves the walk sees both same-timestamp threads
/// exactly once each rather than asserting a specific order.
#[tokio::test]
async fn thread_list_breaks_a_timestamp_tie_without_dropping_or_duplicating() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let t1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, t1, &["received"], "2026-08-15T05:00:01.000Z", "<a@x>"),
    )
    .await
    .unwrap();
    let t2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, t2, &["received"], "2026-08-15T05:00:01.000Z", "<b@x>"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let query = |cursor| ListThreadsQuery { limit: 1, direction: SortDirection::Ascending, cursor };
    let page1 = threads::list(&pool, &filter, &[], query(None))
        .await
        .unwrap();
    let first = page1.items[0].thread_id;
    let cursor1 =
        ThreadCursor::decode(page1.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();
    let page2 = threads::list(&pool, &filter, &[], query(Some(cursor1)))
        .await
        .unwrap();
    let second = page2.items[0].thread_id;
    assert!(page2.next.is_none());
    assert_ne!(first, second, "must not return the same thread twice");

    let mut seen = [first, second];
    seen.sort();
    let mut expected = [t1, t2];
    expected.sort();
    assert_eq!(
        seen, expected,
        "both same-timestamp threads must be seen exactly once each across the walk"
    );
}

// --- Round 3: limit: 2 somewhere — every prior walking test used limit: 1, where `first()` and
// `last()` on a one-item page are the same expression, so anchoring the next token on the wrong
// end of the page survives every one of them. ---

#[tokio::test]
async fn list_with_limit_two_anchors_the_next_token_on_the_last_item_not_the_first() {
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
        ListMessagesQuery { limit: 2, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(
        page1
            .items
            .iter()
            .map(|m| m.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["<a@x>", "<b@x>"]
    );
    let cursor =
        MessageCursor::decode(page1.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();
    assert_eq!(
        cursor.message_id.as_str(),
        "<b@x>",
        "the next token must anchor on the LAST item of the page (<b@x>), not the first (<a@x>)"
    );

    let page2 = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 2, direction: SortDirection::Ascending, cursor: Some(cursor) },
    )
    .await
    .unwrap();
    assert_eq!(
        page2
            .items
            .iter()
            .map(|m| m.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["<c@x>"]
    );
    assert!(page2.next.is_none());
}

#[tokio::test]
async fn thread_list_with_limit_two_anchors_the_next_token_on_the_last_item_not_the_first() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let mut ids = Vec::new();
    for (n, when) in [
        "2026-08-15T05:00:01.000Z",
        "2026-08-15T05:00:02.000Z",
        "2026-08-15T05:00:03.000Z",
    ]
    .into_iter()
    .enumerate()
    {
        let thread_id = ThreadId::new_random();
        let last_message_id = format!("<t{n}@x>");
        threads::insert(
            &pool,
            new_thread(&inbox, &org, pod, thread_id, &["received"], when, &last_message_id),
        )
        .await
        .unwrap();
        ids.push(thread_id);
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let page1 = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: 2, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(
        page1.items.iter().map(|t| t.thread_id).collect::<Vec<_>>(),
        vec![ids[0], ids[1]]
    );
    let cursor =
        ThreadCursor::decode(page1.next.as_deref().expect("one row remains"), filter.inbox_id())
            .unwrap();
    assert_eq!(
        cursor.thread_id, ids[1],
        "the next token must anchor on the LAST item of the page, not the first"
    );

    let page2 = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: 2, direction: SortDirection::Ascending, cursor: Some(cursor) },
    )
    .await
    .unwrap();
    assert_eq!(page2.items.iter().map(|t| t.thread_id).collect::<Vec<_>>(), vec![ids[2]]);
    assert!(page2.next.is_none());
}

// --- Round 3: A3's limit: 0 guard. ---

#[tokio::test]
async fn list_with_limit_zero_returns_an_empty_page_without_panicking() {
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
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let page = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 0, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items, Vec::new());
    assert!(page.next.is_none(), "a zero-limit page has no row to anchor a cursor on");
}

#[tokio::test]
async fn thread_list_with_limit_zero_returns_an_empty_page_without_panicking() {
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

    let filter = inbox_filter(&org, pod, &inbox);
    let page = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: 0, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items, Vec::new());
    assert!(page.next.is_none());
}

// --- Round 4, R1: `fetch_limit = query.limit as i64 + 1` overflowed at the top of u64's range —
// `limit: u64::MAX` wrapped `as i64` to -1, so +1 produced `LIMIT 0` (a real mailbox reading as
// empty, indistinguishable from actually being empty); `limit: i64::MAX as u64` overflowed `i64`
// on the `+1` and panicked. `query.limit` is an unclamped `u64` all the way from the wire
// (`amk_types::page::ListParams.limit: Option<u64>`), so both extremes are reachable. ---

#[tokio::test]
async fn list_with_u64_max_limit_returns_every_visible_row_not_an_empty_page() {
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
    let page = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: u64::MAX, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(
        page.items.len(),
        3,
        "a mailbox holding three rows must not answer empty for limit: u64::MAX"
    );
    assert!(page.next.is_none(), "every row fit on one page");
}

#[tokio::test]
async fn list_with_i64_max_as_u64_limit_returns_every_visible_row_without_panicking() {
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
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let page = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery {
            limit: i64::MAX as u64,
            direction: SortDirection::Ascending,
            cursor: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1, "must not panic and must not answer empty");
}

#[tokio::test]
async fn thread_list_with_u64_max_limit_returns_every_visible_row_not_an_empty_page() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    for (id, when) in [
        ("<a@x>", "2026-08-15T05:00:01.000Z"),
        ("<b@x>", "2026-08-15T05:00:02.000Z"),
        ("<c@x>", "2026-08-15T05:00:03.000Z"),
    ] {
        let thread_id = ThreadId::new_random();
        threads::insert(&pool, new_thread(&inbox, &org, pod, thread_id, &["received"], when, id))
            .await
            .unwrap();
    }

    let filter = inbox_filter(&org, pod, &inbox);
    let page = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: u64::MAX, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3, "three threads must not answer empty for limit: u64::MAX");
    assert!(page.next.is_none());
}

#[tokio::test]
async fn thread_list_with_i64_max_as_u64_limit_returns_every_visible_row_without_panicking() {
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
            "<a@x>",
        ),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let page = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery {
            limit: i64::MAX as u64,
            direction: SortDirection::Ascending,
            cursor: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1, "must not panic and must not answer empty");
}

// --- Round 4, R2: the round-3 three-column `ORDER BY` had no test that could distinguish it from
// a mismatched or reordered one — every existing tie test varies exactly one tiebreak column, so
// `(timestamp, inbox_id, message_id)` and `(timestamp, message_id, inbox_id)` reduce to the same
// comparison. This probe varies BOTH at once, so the two orderings genuinely disagree on which row
// comes first: inbox "aaa…" holds the lexicographically-LATE message_id `<z@x>`, inbox "bbb…"
// holds the lexicographically-EARLY `<a@x>`, both at the identical millisecond. Walked at the pod
// mount (inbox_id unpinned) with limit: 1 in both directions, asserting the exact order the SQL
// declares — not just "both seen once", since a dropped-vs-reordered row are different failure
// shapes worth telling apart in the assertion. ---

#[tokio::test]
async fn list_at_the_pod_mount_agrees_with_order_by_when_both_tiebreak_columns_differ() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox_a = support::seed_inbox(&pool, &org, pod, "aaa").await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "bbb").await;
    assert!(
        inbox_a.as_str() < inbox_b.as_str(),
        "seed ordering assumption: local part alone decides it, regardless of the random suffix"
    );

    let t_a = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox_a, &org, pod, t_a, &["received"], "2026-08-15T05:00:00.000Z", "<z@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox_a, &org, pod, t_a, "<z@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();
    let t_b = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox_b, &org, pod, t_b, &["received"], "2026-08-15T05:00:00.000Z", "<a@x>"),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(&inbox_b, &org, pod, t_b, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();

    let filter = pod_filter(&org, pod);

    // Ascending: (timestamp, inbox_id, message_id) puts inbox_a's row first, since "aaa…" <
    // "bbb…" dominates the tie regardless of message_id. A tiebreak that instead sorted by
    // message_id first (`<a@x>` < `<z@x>`) would put inbox_b's row first — the disagreement this
    // test exists to catch.
    let asc = |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Ascending, cursor };
    let asc_p1 = messages::list(&pool, &filter, &[], asc(None))
        .await
        .unwrap();
    assert_eq!(asc_p1.items.len(), 1);
    assert_eq!(
        asc_p1.items[0].inbox_id, inbox_a,
        "ascending: inbox_id, not message_id, must decide the first tiebreak"
    );
    let asc_cursor = MessageCursor::decode(
        asc_p1.next.as_deref().expect("a second row remains"),
        filter.inbox_id(),
    )
    .unwrap();
    let asc_p2 = messages::list(&pool, &filter, &[], asc(Some(asc_cursor)))
        .await
        .unwrap();
    assert_eq!(
        asc_p2.items.len(),
        1,
        "ascending: the second row must not be silently dropped by an ORDER BY that disagrees \
         with the WHERE clause's resumption predicate"
    );
    assert_eq!(asc_p2.items[0].inbox_id, inbox_b);
    assert!(asc_p2.next.is_none());

    // Descending: the mirror image — inbox_b ("bbb…") sorts first descending.
    let desc =
        |cursor| ListMessagesQuery { limit: 1, direction: SortDirection::Descending, cursor };
    let desc_p1 = messages::list(&pool, &filter, &[], desc(None))
        .await
        .unwrap();
    assert_eq!(desc_p1.items.len(), 1);
    assert_eq!(desc_p1.items[0].inbox_id, inbox_b);
    let desc_cursor = MessageCursor::decode(
        desc_p1.next.as_deref().expect("a second row remains"),
        filter.inbox_id(),
    )
    .unwrap();
    let desc_p2 = messages::list(&pool, &filter, &[], desc(Some(desc_cursor)))
        .await
        .unwrap();
    assert_eq!(
        desc_p2.items.len(),
        1,
        "descending: the second row must not be silently dropped"
    );
    assert_eq!(desc_p2.items[0].inbox_id, inbox_a);
    assert!(desc_p2.next.is_none());
}

// ---- hostile bytes reaching SQL (`.claude/contracts/amk-store-id-safety.md`) --------------------
//
// `InboxId::new`/`MessageId::new` are infallible, so a NUL-bearing id can reach these functions
// regardless of caller discipline — via an explicit parameter, or via `ScopeFilter`'s own pin
// (`filter.inbox_id()`), which every query below also binds. Unguarded, either would fail at
// Postgres parameter encoding (SQLSTATE 22021): a `StoreError::Database`, not the uniform
// not-found every other unresolvable id produces — exactly the side channel the masking rule
// forbids. Each guarded value gets its own direct test, not one shared helper: the previous
// dispatch's fifth review round found a regression test that guarded one call site while a
// sibling with the identical shape stayed open, and a mutant left the suite green while an
// uppercase id really deleted a row.

/// `messages::get`, one of the five call paths the dispatch contract names: a NUL-bearing
/// `inbox_id` *parameter* must return `Ok(None)`, never `Err(StoreError::Database(_))`.
#[tokio::test]
async fn message_get_with_a_nul_byte_in_the_inbox_id_parameter_is_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let filter = org_filter(&org);
    let hostile_inbox = InboxId::new("abc\0def@x");

    let result = messages::get(&pool, &filter, &hostile_inbox, &MessageId::new("<a@x>"), &[]).await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing inbox_id parameter must mask as not-found, not error: {result:?}"
    );
}

/// Sibling of the above for `messages::get`'s other free-text parameter.
#[tokio::test]
async fn message_get_with_a_nul_byte_in_the_message_id_parameter_is_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let filter = inbox_filter(&org, pod, &inbox);
    let hostile_message_id = MessageId::new("<a\0b@x>");

    let result = messages::get(&pool, &filter, &inbox, &hostile_message_id, &[]).await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing message_id parameter must mask as not-found, not error: {result:?}"
    );
}

/// Sibling of the two tests above for `messages::get`'s third bound value: the `ScopeFilter`'s
/// own pin, which the parameter checks above do not exercise (they pair a clean pin with a
/// hostile parameter; this pairs a clean parameter with a hostile pin).
#[tokio::test]
async fn message_get_with_a_nul_byte_in_the_scope_filters_inbox_id_pin_is_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let filter = inbox_filter(&org, pod, &InboxId::new("abc\0def@x"));

    let result =
        messages::get(&pool, &filter, &InboxId::new("clean@x"), &MessageId::new("<a@x>"), &[])
            .await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing ScopeFilter inbox_id pin must mask as not-found, not error: {result:?}"
    );
}

/// `messages::list` is not one of the five call paths the panel names, but it is structurally
/// identical to `threads::list` below — the same `filter.inbox_id()` bind, the same risk — and
/// leaving it unguarded would be exactly the sibling gap the previous dispatch's review round
/// found, just one module over.
#[tokio::test]
async fn message_list_with_a_nul_byte_in_the_scope_filters_inbox_id_pin_returns_an_empty_page() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let filter = inbox_filter(&org, pod, &InboxId::new("abc\0def@x"));

    let result = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await;
    match result {
        Ok(page) => {
            assert!(page.items.is_empty(), "a hostile pin must yield no rows");
            assert!(page.next.is_none(), "a hostile pin must yield no page token either");
        }
        Err(e) => panic!("a NUL-bearing ScopeFilter inbox_id pin must not error: {e:?}"),
    }
}

/// `messages::list`'s cursor: a hand-built `MessageCursor` bypasses `MessageCursor::decode`
/// entirely — its fields are `pub` and `::new()` is infallible by decision, so nothing at the
/// type level guarantees every cursor `list` receives went through `decode`. Deliberately a
/// *different* answer from the pin guard above (see that guard's own comment): a page token is
/// not a resource, so there is nothing to mask, and a hostile token gets the typed
/// `PageTokenError` the wire layer already knows how to render rather than an empty page that
/// would silently truncate pagination. Covers both free-text fields the cursor carries.
#[tokio::test]
async fn list_rejects_a_nul_byte_in_a_hand_built_cursor() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let filter = inbox_filter(&org, pod, &inbox);
    let ts = Utc::now();

    let hostile_inbox = MessageCursor {
        message_id: MessageId::new("<clean@x>"),
        inbox_id: InboxId::new("abc\0def@x"),
        timestamp: ts,
    };
    let result = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery {
            limit: 10,
            direction: SortDirection::Ascending,
            cursor: Some(hostile_inbox),
        },
    )
    .await;
    assert!(
        matches!(
            result,
            Err(StoreError::InvalidPageToken(PageTokenError::ForbiddenByte("cursor.inbox_id")))
        ),
        "a hand-built cursor with a NUL-bearing inbox_id must be a typed error, not an empty page \
         or a raw database error: {result:?}"
    );

    let hostile_message_id = MessageCursor {
        message_id: MessageId::new("<z\0z@x>"),
        inbox_id: inbox.clone(),
        timestamp: ts,
    };
    let result = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery {
            limit: 10,
            direction: SortDirection::Ascending,
            cursor: Some(hostile_message_id),
        },
    )
    .await;
    assert!(
        matches!(
            result,
            Err(StoreError::InvalidPageToken(PageTokenError::ForbiddenByte("cursor.message_id")))
        ),
        "a hand-built cursor with a NUL-bearing message_id must be a typed error, not an empty \
         page or a raw database error: {result:?}"
    );
}

/// `threads::get_with_messages`, one of the five named call paths: `thread_id` is a UUID and
/// cannot carry a NUL, so the only free-text value this function binds is the `ScopeFilter`'s own
/// `inbox_id` pin.
#[tokio::test]
async fn thread_get_with_messages_with_a_nul_byte_in_the_scope_filters_inbox_id_pin_is_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let filter = inbox_filter(&org, pod, &InboxId::new("abc\0def@x"));
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);

    let result = threads::get_with_messages(&pool, &filter, ThreadId::new_random(), &access).await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing ScopeFilter inbox_id pin must mask as not-found, not error: {result:?}"
    );
}

/// `threads::list`, one of the five named call paths.
#[tokio::test]
async fn thread_list_with_a_nul_byte_in_the_scope_filters_inbox_id_pin_returns_an_empty_page() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let filter = inbox_filter(&org, pod, &InboxId::new("abc\0def@x"));

    let result = threads::list(
        &pool,
        &filter,
        &[],
        ListThreadsQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await;
    match result {
        Ok(page) => {
            assert!(page.items.is_empty(), "a hostile pin must yield no rows");
            assert!(page.next.is_none(), "a hostile pin must yield no page token either");
        }
        Err(e) => panic!("a NUL-bearing ScopeFilter inbox_id pin must not error: {e:?}"),
    }
}

// ---- the third door: insert paths (`.claude/contracts/amk-store-id-safety.md`, rewritten) -------
//
// `messages::insert`/`threads::insert` are not reached through a path segment or a page token —
// `amk-ingest` will call `messages::insert` with a `MessageId` parsed straight out of hostile
// MIME, and `amk-import` will call the same functions with values read from Stalwart. Neither of
// the two wire doors covers either caller, so `amk-store` must be total on its own: a public
// function that 500s on a byte its parameter type permits is a defect in that function, not its
// caller. There is no not-found to fall back to on an insert, so the guard is a rejection
// (`StoreError::InvalidValue`), not the `Ok`/empty-page treatment the lookups get — and it names
// the field, one distinct `&'static str` per bound value, tested independently per field so a
// mutant deleting one guard cannot hide behind a sibling's still-passing test.

/// `messages::insert`, first of its three guarded fields.
#[tokio::test]
async fn message_insert_rejects_a_nul_byte_in_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let result = messages::insert(
        &pool,
        NewMessage {
            inbox_id: InboxId::new("abc\0def@x"),
            ..new_message(
                &inbox,
                &org,
                pod,
                thread_id,
                "<a@x>",
                &["sent"],
                "2026-08-15T05:00:01.000Z",
            )
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("inbox_id"))),
        "a NUL-bearing inbox_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `messages::insert`, second guarded field — tested independently of the first: the two live in
/// one function but must fail on their own bytes, not on each other's.
#[tokio::test]
async fn message_insert_rejects_a_nul_byte_in_message_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let result = messages::insert(
        &pool,
        NewMessage {
            message_id: MessageId::new("<a\0b@x>"),
            ..new_message(
                &inbox,
                &org,
                pod,
                thread_id,
                "<a@x>",
                &["sent"],
                "2026-08-15T05:00:01.000Z",
            )
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("message_id"))),
        "a NUL-bearing message_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `messages::insert`, third guarded field: `in_reply_to` is the one header-derived, optional id
/// on this path — a header a hostile MIME message controls directly, per the contract's own P2
/// citation for why this door exists at all.
#[tokio::test]
async fn message_insert_rejects_a_nul_byte_in_in_reply_to() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let mut new =
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z");
    new.in_reply_to = Some(MessageId::new("<z\0z@x>"));

    let result = messages::insert(&pool, new).await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("in_reply_to"))),
        "a NUL-bearing in_reply_to must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `messages::insert`, fourth guarded field: `references` is the only other `MessageId`-typed
/// value on this struct — same type, same struct, same statement, same linkage role as
/// `in_reply_to` above, so it gets the identical guard. The hostile byte sits in the **second**
/// element, deliberately not the first: a guard written as `.first().is_some_and(...)` instead of
/// `.any(...)` would pass this test's first element and still 500 on the second.
#[tokio::test]
async fn message_insert_rejects_a_nul_byte_in_a_non_first_references_element() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let mut new =
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z");
    new.references = Some(vec![
        MessageId::new("<clean@x>"),
        MessageId::new("<z\0z@x>"),
        MessageId::new("<also-clean@x>"),
    ]);

    let result = messages::insert(&pool, new).await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("references"))),
        "a NUL-bearing references element must be a typed InvalidValue, not a raw database error: \
         {result:?}"
    );
}

/// Positive-path counterpart to `message_insert_rejects_a_nul_byte_in_in_reply_to`: a *clean*
/// `in_reply_to` must not be rejected, and the stored value must round-trip through
/// `messages::get` byte-for-byte unchanged. This is the coverage an over-broad guard needs to be
/// caught by: every other test touching this field passes a *hostile* value and expects `Err`, so
/// widening `.is_some_and(pred)` to `.is_some()` — rejecting every reply, clean ones included —
/// would have left the whole suite green without this test.
#[tokio::test]
async fn message_insert_with_a_clean_in_reply_to_succeeds_and_round_trips() {
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

    let mut new = new_message(
        &inbox,
        &org,
        pod,
        thread_id,
        "<reply@x>",
        &["sent"],
        "2026-08-15T05:00:01.000Z",
    );
    new.in_reply_to = Some(MessageId::new("<parent@x>"));

    messages::insert(&pool, new)
        .await
        .expect("a clean in_reply_to must not be rejected");

    let filter = inbox_filter(&org, pod, &inbox);
    let fetched = messages::get(&pool, &filter, &inbox, &MessageId::new("<reply@x>"), &[])
        .await
        .unwrap()
        .expect("must round trip");
    assert_eq!(
        fetched.item.in_reply_to,
        Some(MessageId::new("<parent@x>")),
        "the clean in_reply_to must be stored and read back unchanged, not stripped or nulled"
    );
}

/// Positive-path counterpart to `message_insert_rejects_a_nul_byte_in_a_non_first_references_element`
/// — same reasoning as the `in_reply_to` test above, for the sibling field.
#[tokio::test]
async fn message_insert_with_clean_references_succeeds_and_round_trips() {
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

    let mut new = new_message(
        &inbox,
        &org,
        pod,
        thread_id,
        "<reply2@x>",
        &["sent"],
        "2026-08-15T05:00:01.000Z",
    );
    new.references = Some(vec![MessageId::new("<a@x>"), MessageId::new("<b@x>")]);

    messages::insert(&pool, new)
        .await
        .expect("clean references must not be rejected");

    let filter = inbox_filter(&org, pod, &inbox);
    let fetched = messages::get(&pool, &filter, &inbox, &MessageId::new("<reply2@x>"), &[])
        .await
        .unwrap()
        .expect("must round trip");
    assert_eq!(
        fetched.item.references,
        Some(vec![MessageId::new("<a@x>"), MessageId::new("<b@x>")]),
        "the clean references list must be stored and read back unchanged, not stripped or nulled"
    );
}

/// `threads::insert`, first of its two guarded fields.
#[tokio::test]
async fn thread_insert_rejects_a_nul_byte_in_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let result = threads::insert(
        &pool,
        NewThread {
            inbox_id: InboxId::new("abc\0def@x"),
            ..new_thread(
                &inbox,
                &org,
                pod,
                thread_id,
                &["received"],
                "2026-08-15T05:00:00.000Z",
                "<seed>",
            )
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("inbox_id"))),
        "a NUL-bearing inbox_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `threads::insert`, second guarded field — tested independently of the first, same reasoning as
/// the two `messages::insert` field tests above.
#[tokio::test]
async fn thread_insert_rejects_a_nul_byte_in_last_message_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let thread_id = ThreadId::new_random();

    let result = threads::insert(
        &pool,
        NewThread {
            last_message_id: MessageId::new("<z\0z@x>"),
            ..new_thread(
                &inbox,
                &org,
                pod,
                thread_id,
                &["received"],
                "2026-08-15T05:00:00.000Z",
                "<seed>",
            )
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("last_message_id"))),
        "a NUL-bearing last_message_id must be a typed InvalidValue, not a raw database error: \
         {result:?}"
    );
}

// ---- `pods::delete`'s four-name constraint match, one referencing table at a time ------------
//
// `is_pod_reference_violation` matches exactly four constraint names
// (`inboxes_pod_id_fkey`/`threads_pod_id_fkey`/`messages_pod_id_fkey`/`api_keys_pod_id_fkey`), and
// a single test that only ever trips one of them would survive three of the four names being
// deleted from the `matches!` pattern. `tests/api_keys.rs`'s
// `deleting_a_pod_that_owns_keys_is_rejected_by_the_declared_fk_behaviour` pins the api-key name;
// `tests/control_plane.rs`'s pod/inbox tests pin the inbox name (fixture 22's own scenario). These
// two pin `threads_pod_id_fkey`/`messages_pod_id_fkey` — deliberately via a thread/message whose
// own `inbox_id` belongs to a *different* pod, so only the one constraint under test can possibly
// fire (nothing here ties `threads.pod_id`/`messages.pod_id` to their `inbox_id`'s own pod at the
// schema level, which is what makes this isolation possible).

#[tokio::test]
async fn deleting_a_pod_referenced_only_by_a_threads_pod_id_is_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let target_pod = support::seed_pod(&pool, &org).await;
    let owner_pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, owner_pod, "owner").await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            target_pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();

    let result = pods::delete(&pool, &org, target_pod).await;
    assert!(
        matches!(result, Err(StoreError::PodNotEmpty)),
        "a pod referenced only by threads_pod_id_fkey must still be refused: {result:?}"
    );
    assert!(
        pods::get(&pool, &org, target_pod).await.unwrap().is_some(),
        "the pod must survive the rejected delete"
    );
}

#[tokio::test]
async fn deleting_a_pod_referenced_only_by_a_messages_pod_id_is_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let target_pod = support::seed_pod(&pool, &org).await;
    let owner_pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, owner_pod, "owner").await;

    // The thread this message belongs to is pinned to owner_pod, not target_pod — otherwise this
    // test would also trip threads_pod_id_fkey and no longer isolate the constraint under test.
    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            owner_pod,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox,
            &org,
            target_pod,
            thread_id,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();

    let result = pods::delete(&pool, &org, target_pod).await;
    assert!(
        matches!(result, Err(StoreError::PodNotEmpty)),
        "a pod referenced only by messages_pod_id_fkey must still be refused: {result:?}"
    );
    assert!(
        pods::get(&pool, &org, target_pod).await.unwrap().is_some(),
        "the pod must survive the rejected delete"
    );
}

// ---- migration 0008: `inboxes::delete` cascades ------------------------------------------------

/// The test that settles the cascade set (migration 0008's own `[TESTED]` note): an inbox holding
/// a thread, a message AND an inbox-scoped api key must delete cleanly, with all four rows gone
/// afterwards — `Ok(true)`, not the `PodNotEmpty`-style refusal `pods::delete` gives for the
/// symmetric case.
#[tokio::test]
async fn inbox_delete_cascades_through_its_thread_message_and_inbox_scoped_key() {
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
    messages::insert(
        &pool,
        new_message(&inbox, &org, pod, thread_id, "<a@x>", &["sent"], "2026-08-15T05:00:01.000Z"),
    )
    .await
    .unwrap();
    let key = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox.clone()),
            name: "inbox-scoped".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    assert!(inboxes::delete(&pool, &org, None, &inbox).await.unwrap());

    assert!(
        inboxes::get(&pool, &org, None, &inbox)
            .await
            .unwrap()
            .is_none(),
        "the inbox itself must be gone"
    );
    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    assert!(
        threads::get_with_messages(&pool, &filter, thread_id, &access)
            .await
            .unwrap()
            .is_none(),
        "the thread must be gone"
    );
    assert!(
        messages::get(&pool, &filter, &inbox, &MessageId::new("<a@x>"), &[])
            .await
            .unwrap()
            .is_none(),
        "the message must be gone"
    );
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Inbox(inbox), &key.api_key_id)
            .await
            .unwrap()
            .is_none(),
        "the inbox-scoped api key must be gone"
    );
}

/// The worst possible version of a cascade defect: a scope miss that still cascades. `inbox_a`
/// belongs to `pod_a`; deleting it through `pod_b`'s scope must be a no-op, and every row it would
/// otherwise have taken with it — including an inbox-scoped api key — must survive untouched.
#[tokio::test]
async fn inbox_delete_scope_miss_across_pods_cascades_nothing() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let pod_b = support::seed_pod(&pool, &org).await;

    let thread_id = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox_a,
            &org,
            pod_a,
            thread_id,
            &["received"],
            "2026-08-15T05:00:00.000Z",
            "<seed>",
        ),
    )
    .await
    .unwrap();
    messages::insert(
        &pool,
        new_message(
            &inbox_a,
            &org,
            pod_a,
            thread_id,
            "<a@x>",
            &["sent"],
            "2026-08-15T05:00:01.000Z",
        ),
    )
    .await
    .unwrap();
    let key = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-scoped".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let result = inboxes::delete(&pool, &org, Some(pod_b), &inbox_a).await;
    assert!(
        matches!(result, Ok(false)),
        "deleting inbox_a through pod_b's scope must be a no-op, not an error: {result:?}"
    );

    assert!(
        inboxes::get(&pool, &org, None, &inbox_a)
            .await
            .unwrap()
            .is_some(),
        "inbox_a must survive the cross-pod delete attempt"
    );
    let filter = inbox_filter(&org, pod_a, &inbox_a);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::by_id(&grants);
    assert!(
        threads::get_with_messages(&pool, &filter, thread_id, &access)
            .await
            .unwrap()
            .is_some(),
        "the thread must survive"
    );
    assert!(
        messages::get(&pool, &filter, &inbox_a, &MessageId::new("<a@x>"), &[])
            .await
            .unwrap()
            .is_some(),
        "the message must survive"
    );
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Inbox(inbox_a), &key.api_key_id)
            .await
            .unwrap()
            .is_some(),
        "the inbox-scoped api key must survive — a scope miss must never cascade"
    );
}

// ---- label mutation and delete -----------------------------------------------------------------
// `[SPEC:.claude/contracts/amk-store-mail-mutations.md]`. The system-label gate is NOT tested here:
// it is a request-boundary rule (`amk_core::labels::system_label_violations`), and this crate
// deliberately applies whatever it is handed so the ingest pipeline can set its own labels.

/// Seeds one thread and one message in it, returning both ids.
async fn seed_one(
    pool: &sqlx::PgPool,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
    labels: &[&str],
) -> (ThreadId, MessageId) {
    let thread_id = ThreadId::new_random();
    let mid = format!("<mut-{}@example.test>", support::unique_suffix());
    threads::insert(
        pool,
        new_thread(inbox, org, pod, thread_id, labels, "2026-08-15T06:00:00.000Z", &mid),
    )
    .await
    .unwrap();
    messages::insert(
        pool,
        new_message(inbox, org, pod, thread_id, &mid, labels, "2026-08-15T06:00:01.000Z"),
    )
    .await
    .unwrap();
    (thread_id, MessageId::new(mid))
}

/// Edge cases 1 and 2 together, because they are the two halves of `apply_mutation`'s contract:
/// an already-present label is not duplicated and existing order is preserved, and a label named
/// in BOTH `add` and `remove` ends up absent (`reference/fixtures/20-search-and-label-precedence.txt`
/// C, `[TESTED]` live on a message).
#[tokio::test]
async fn a_message_label_mutation_dedupes_preserves_order_and_lets_remove_win() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (_t, mid) = seed_one(&pool, &org, pod, &inbox, &["received", "unread"]).await;
    let filter = org_filter(&org);

    let after = messages::update(
        &pool,
        &filter,
        &inbox,
        &mid,
        &["unread".to_string(), "tag".to_string()],
        &[],
    )
    .await
    .unwrap()
    .expect("the row is in scope");
    assert_eq!(
        after,
        vec![
            "received".to_string(),
            "unread".to_string(),
            "tag".to_string()
        ],
        "an already-present label is not duplicated and existing order is preserved"
    );

    let conflicted = messages::update(
        &pool,
        &filter,
        &inbox,
        &mid,
        &["both".to_string()],
        &["both".to_string()],
    )
    .await
    .unwrap()
    .expect("still in scope");
    assert!(
        !conflicted.contains(&"both".to_string()),
        "remove wins over add: {conflicted:?}"
    );
}

/// Edge case 3: an empty mutation is a no-op that still reports the current labels.
#[tokio::test]
async fn an_empty_mutation_returns_the_current_labels_without_rewriting_them() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (tid, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    let filter = org_filter(&org);

    let m = messages::update(&pool, &filter, &inbox, &mid, &[], &[])
        .await
        .unwrap();
    assert_eq!(m, Some(vec!["received".to_string()]));
    let t = threads::update(&pool, &filter, tid, &[], &[])
        .await
        .unwrap();
    assert_eq!(t, Some(vec!["received".to_string()]));
}

/// Edge case 4, both halves. The return value alone proves nothing — a buggy implementation could
/// return `None` and still have written the row, so the row is re-read afterwards.
#[tokio::test]
async fn a_row_outside_the_window_is_not_found_and_is_left_unchanged() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let other_pod = support::seed_pod(&pool, &org).await;
    let (tid, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    // A window pinned to a DIFFERENT pod: the row exists, but not in this window.
    let foreign = pod_filter(&org, other_pod);

    assert_eq!(
        messages::update(&pool, &foreign, &inbox, &mid, &["x".to_string()], &[])
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        threads::update(&pool, &foreign, tid, &["x".to_string()], &[])
            .await
            .unwrap(),
        None
    );
    assert!(!messages::delete(&pool, &foreign, &inbox, &mid)
        .await
        .unwrap());
    assert!(!threads::delete(&pool, &foreign, tid).await.unwrap());

    // The half that matters: nothing was written, and nothing was deleted.
    let visible = org_filter(&org);
    assert_eq!(
        messages::update(&pool, &visible, &inbox, &mid, &[], &[])
            .await
            .unwrap(),
        Some(vec!["received".to_string()]),
        "the out-of-window update must not have touched the row"
    );
    assert_eq!(
        threads::update(&pool, &visible, tid, &[], &[])
            .await
            .unwrap(),
        Some(vec!["received".to_string()]),
    );
}

/// Edge case 5: three independent NUL guards, asserted separately. A single guard passes one of
/// these and fails the other two — which is exactly why `messages::get`'s comment says so.
#[tokio::test]
async fn a_nul_byte_in_any_bound_value_masks_as_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (_t, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    let clean = org_filter(&org);

    let nul_inbox = InboxId::new("bad\0@example.test");
    assert_eq!(
        messages::update(&pool, &clean, &nul_inbox, &mid, &[], &[])
            .await
            .unwrap(),
        None,
        "NUL in inbox_id"
    );

    let nul_message = MessageId::new("<bad\0@example.test>");
    assert_eq!(
        messages::update(&pool, &clean, &inbox, &nul_message, &[], &[])
            .await
            .unwrap(),
        None,
        "NUL in message_id"
    );

    let nul_pin = inbox_filter(&org, pod, &InboxId::new("pin\0@example.test"));
    assert_eq!(
        messages::update(&pool, &nul_pin, &inbox, &mid, &[], &[])
            .await
            .unwrap(),
        None,
        "NUL in the filter's own inbox pin"
    );
    assert!(!messages::delete(&pool, &nul_pin, &inbox, &mid)
        .await
        .unwrap());
}

/// Edge case 6.
#[tokio::test]
async fn delete_returns_true_once_and_false_afterwards() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (_t, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    let filter = org_filter(&org);

    assert!(messages::delete(&pool, &filter, &inbox, &mid)
        .await
        .unwrap());
    assert!(!messages::delete(&pool, &filter, &inbox, &mid)
        .await
        .unwrap());
    assert_eq!(
        messages::update(&pool, &filter, &inbox, &mid, &[], &[])
            .await
            .unwrap(),
        None
    );
}

/// Edge case 7: deleting a thread takes its messages with it. This is the SCHEMA's answer —
/// `messages_thread_id_fkey` is `ON DELETE CASCADE` per `0008_inbox_delete_cascades.sql` — so this
/// records the observed behaviour rather than asserting a rule no fixture states.
#[tokio::test]
async fn deleting_a_thread_cascades_to_its_messages_rather_than_orphaning_them() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (tid, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    let filter = org_filter(&org);

    assert!(threads::delete(&pool, &filter, tid).await.unwrap());
    assert_eq!(
        messages::update(&pool, &filter, &inbox, &mid, &[], &[])
            .await
            .unwrap(),
        None,
        "the message went with its thread — no orphan left readable"
    );
    assert!(!threads::delete(&pool, &filter, tid).await.unwrap());
}

/// Edge case 8: two concurrent PATCHes must both survive. Without the `FOR UPDATE` read inside a
/// transaction, both tasks read the same starting labels and the second write erases the first —
/// the lost update this implementation is shaped to prevent.
#[tokio::test]
async fn concurrent_label_additions_do_not_lose_one_another() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let (_t, mid) = seed_one(&pool, &org, pod, &inbox, &["received"]).await;
    let filter = org_filter(&org);

    let alpha = ["alpha".to_string()];
    let beta = ["beta".to_string()];
    let (a, b) = tokio::join!(
        messages::update(&pool, &filter, &inbox, &mid, &alpha, &[]),
        messages::update(&pool, &filter, &inbox, &mid, &beta, &[]),
    );
    a.unwrap().expect("in scope");
    b.unwrap().expect("in scope");

    let final_labels = messages::update(&pool, &filter, &inbox, &mid, &[], &[])
        .await
        .unwrap()
        .expect("in scope");
    assert!(final_labels.contains(&"alpha".to_string()), "lost update: {final_labels:?}");
    assert!(final_labels.contains(&"beta".to_string()), "lost update: {final_labels:?}");
}
