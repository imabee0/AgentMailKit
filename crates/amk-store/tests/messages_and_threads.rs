//! Message/thread read-path tests: the two security rules (restricted-label predicate pushdown,
//! scope pinning), keyset pagination boundaries against a real database, and the assigned
//! `message_id` special-character round trip.

mod support;

use amk_core::labels::{excluded_labels, IncludeFlags, LabelAccess};
use amk_core::scope::{Mount, Resolved, Scope, ScopeFilter};
use amk_store::messages::{self, ListMessagesQuery, NewMessage};
use amk_store::pagination::{MessageCursor, SortDirection};
use amk_store::threads::{self, ListThreadsQuery, NewThread};
use amk_store::PageTokenError;
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
    let page = messages::list(
        &pool,
        &filter_a,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page
        .items
        .iter()
        .map(|m| m.message_id.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["<a@x>"], "org A's org-level list must not see org B's message");
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
    let page = messages::list(
        &pool,
        &filter,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page
        .items
        .iter()
        .map(|m| m.message_id.as_str().to_string())
        .collect();
    assert_eq!(ids, vec!["<p1@x>"], "pod-scoped list must not see the sibling pod's message");
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

#[tokio::test]
async fn thread_list_excludes_restricted_labels_with_no_gap() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let visible1 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, visible1, &["received"], "2026-08-15T05:00:01.000Z", "<a@x>"),
    )
    .await
    .unwrap();
    let hidden = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(
            &inbox,
            &org,
            pod,
            hidden,
            &["received", "trash"],
            "2026-08-15T05:00:02.000Z",
            "<b@x>",
        ),
    )
    .await
    .unwrap();
    let visible2 = ThreadId::new_random();
    threads::insert(
        &pool,
        new_thread(&inbox, &org, pod, visible2, &["received"], "2026-08-15T05:00:03.000Z", "<c@x>"),
    )
    .await
    .unwrap();

    let filter = inbox_filter(&org, pod, &inbox);
    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);

    let page = threads::list(
        &pool,
        &filter,
        &excluded,
        ListThreadsQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    let ids: Vec<_> = page.items.iter().map(|t| t.thread_id).collect();
    assert_eq!(ids, vec![visible1, visible2], "the trashed thread must never surface");
    assert!(page.next.is_none());
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
