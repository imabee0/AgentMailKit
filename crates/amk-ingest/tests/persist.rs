//! Persist / `accept` cases. Every assertion is a store row or a 554 reject with an empty store.

mod support;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Instant;

use amk_core::labels::{excluded_labels, IncludeFlags, LabelAccess};
use amk_core::scope::{Mount, Resolved, Scope};
use amk_ingest::{accept, AcceptRequest, Authenticator, Delivery, Envelope, IngestError};
use amk_store::messages::{self, ListMessagesQuery};
use amk_store::pagination::SortDirection;
use amk_types::api_key::KeyGrants;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId};
use amk_types::message::labels;
use sqlx::PgPool;
use support::{
    mid, seed_inbox_at, seed_org, seed_org_pod_inbox, seed_pod, unique_suffix, MimeSpec,
};

const CAP: usize = 64 * 1024;

fn dest(org: &OrganizationId, pod: PodId, inbox: &InboxId) -> Delivery {
    Delivery { organization_id: org.clone(), pod_id: pod, inbox_id: inbox.clone() }
}

fn envelope(mail_from: &str) -> Envelope {
    Envelope {
        mail_from: mail_from.into(),
        client_ip: IpAddr::from([192, 0, 2, 1]),
        ehlo_host: "client.test".into(),
    }
}

fn filter(org: &OrganizationId, pod: PodId, inbox: &InboxId) -> amk_core::scope::ScopeFilter {
    let scope = Scope::Inbox { organization_id: org.clone(), pod_id: pod, inbox_id: inbox.clone() };
    match scope.resolve(&Mount::Organization).unwrap() {
        Resolved::Ready(f) => f,
        Resolved::Probe(_) => panic!("expected Ready"),
    }
}

async fn go(
    pool: &PgPool,
    auth: &Authenticator,
    raw: &[u8],
    mail_from: &str,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
) -> Result<amk_ingest::Accepted, IngestError> {
    match accept(
        pool,
        auth,
        AcceptRequest {
            raw,
            envelope: envelope(mail_from),
            dest: dest(org, pod, inbox),
            max_message_bytes: CAP,
        },
    )
    .await?
    {
        Some(accepted) => Ok(accepted),
        None => panic!("expected a stored message, got SPF-hardfail discard"),
    }
}

async fn go_opt(
    pool: &PgPool,
    auth: &Authenticator,
    raw: &[u8],
    mail_from: &str,
    org: &OrganizationId,
    pod: PodId,
    inbox: &InboxId,
) -> Result<Option<amk_ingest::Accepted>, IngestError> {
    accept(
        pool,
        auth,
        AcceptRequest {
            raw,
            envelope: envelope(mail_from),
            dest: dest(org, pod, inbox),
            max_message_bytes: CAP,
        },
    )
    .await
}

fn assert_554(err: IngestError) {
    match err {
        IngestError::Rejected { code, .. } => assert_eq!(code, 554),
        other => panic!("expected 554 reject, got {other:?}"),
    }
}

/// Case 5 / mutant 3: SPF=none + no DKIM → stored labels include `unauthenticated`;
/// list without include flags has count 0; GET-by-id returns the row.
#[tokio::test]
async fn spf_none_no_dkim_is_stored_unauthenticated_and_hidden_from_list() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let id = mid("unauth");
    let raw =
        MimeSpec::simple("Alice Probe <alice@probe.test>", inbox.as_str(), "hello", &id, "body")
            .render();

    go(&pool, &auth, &raw, "alice@probe.test", &org, pod, &inbox)
        .await
        .unwrap();

    let f = filter(&org, pod, &inbox);
    let got = messages::get(&pool, &f, &inbox, &MessageId::new(&id), &[])
        .await
        .unwrap()
        .expect("GET-by-id");
    let set: BTreeSet<&str> = got.item.labels.iter().map(String::as_str).collect();
    assert_eq!(
        set,
        BTreeSet::from([labels::RECEIVED, labels::UNREAD, labels::UNAUTHENTICATED]),
        "09b branch 1 membership"
    );

    let grants = KeyGrants::Unrestricted;
    let access = LabelAccess::list(&grants, IncludeFlags::NONE);
    let excluded = excluded_labels(&access);
    let page = messages::list(
        &pool,
        &f,
        &excluded,
        ListMessagesQuery { limit: 50, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 0, "list without include flags hides unauthenticated");

    let text = got.text.as_deref().expect("text");
    assert!(text.ends_with('\n'), "text keeps trailing newline, got {text:?}");
    let without_nl = text
        .strip_suffix('\n')
        .and_then(|s| s.strip_suffix('\r').or(Some(s)));
    assert_eq!(
        got.extracted_text.as_deref(),
        without_nl,
        "extracted_text is text minus the trailing newline"
    );
    assert!(got.extracted_html.is_none(), "no HTML → omit extracted_html");
    assert!(
        got.item
            .preview
            .as_deref()
            .is_some_and(|p| p.ends_with('\n')),
        "preview keeps trailing newline"
    );
}

/// 09b branch 2: SPF hardfail → DATA/accept 250-equivalent, store nothing. No `spam`.
#[tokio::test]
async fn spf_hardfail_is_accepted_and_stores_nothing() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::spf_fail();
    let id = mid("hardfail");
    let raw =
        MimeSpec::simple("Probe <probe@example.net>", inbox.as_str(), "hf", &id, "x").render();

    let out = go_opt(&pool, &auth, &raw, "probe@example.net", &org, pod, &inbox)
        .await
        .expect("hardfail is not a 5xx");
    assert!(out.is_none(), "hardfail stores nothing");

    let f = filter(&org, pod, &inbox);
    assert!(
        messages::get(&pool, &f, &inbox, &MessageId::new(&id), &[])
            .await
            .unwrap()
            .is_none(),
        "GET-by-id empty"
    );
    let page = messages::list(
        &pool,
        &f,
        &[],
        ListMessagesQuery { limit: 50, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 0, "list empty");
}

/// Case 6: unbracketed parent In-Reply-To joins the parent thread; structured
/// `in_reply_to` is re-bracketed; `headers.In-Reply-To` stays bare.
#[tokio::test]
async fn unbracketed_in_reply_to_joins_and_preserves_wire_header() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let sfx = unique_suffix();
    let root_inner = format!("amkc3-root-{sfx}@appsynergy.io");
    let root_id = format!("<{root_inner}>");
    let bare_id = format!("<amkc3-bare-{sfx}@appsynergy.io>");

    let root =
        MimeSpec::simple("C3 Root <c3root@probe.test>", inbox.as_str(), "root", &root_id, "root")
            .render();
    let parent = go(&pool, &auth, &root, "c3root@probe.test", &org, pod, &inbox)
        .await
        .unwrap();

    let mut reply = MimeSpec::simple(
        "C3 Bare <c3bare@probe.test>",
        inbox.as_str(),
        "bare",
        &bare_id,
        "bare body",
    );
    reply.in_reply_to = Some(root_inner.clone());
    let accepted = go(&pool, &auth, &reply.render(), "c3bare@probe.test", &org, pod, &inbox)
        .await
        .unwrap();

    let f = filter(&org, pod, &inbox);
    let child = messages::get(&pool, &f, &inbox, &MessageId::new(&bare_id), &[])
        .await
        .unwrap()
        .expect("child");
    assert_eq!(child.item.thread_id, parent.thread_id);
    assert_eq!(accepted.thread_id, parent.thread_id);
    assert_eq!(
        child.item.in_reply_to.as_ref().map(MessageId::as_str),
        Some(root_id.as_str()),
        "structured in_reply_to is re-bracketed"
    );
    let wire = child
        .item
        .headers
        .as_ref()
        .and_then(|h| h.get("In-Reply-To"))
        .map(String::as_str);
    assert_eq!(wire, Some(root_inner.as_str()), "headers.In-Reply-To stays the bare wire value");
    let keys: BTreeSet<&str> = child
        .item
        .headers
        .as_ref()
        .map(|h| h.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(keys, BTreeSet::from(["In-Reply-To"]), "headers map is only In-Reply-To");
}

/// Case 7: subject is not a grouping key; empty → None; `Re:` stored; 10 KB kept;
/// RFC 2047 is 250 + Some; homoglyphs do not merge.
#[tokio::test]
async fn subjects_do_not_thread_and_normalize_as_specified() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let f = filter(&org, pod, &inbox);

    let a = mid("subj-a");
    let b = mid("subj-b");
    let same = "identical subject";
    let ta = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), same, &a, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let tb = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), same, &b, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    assert_ne!(ta.thread_id, tb.thread_id, "identical subject, no linkage → two threads");

    let empty_id = mid("empty");
    let mut empty = MimeSpec::simple("a@probe.test", inbox.as_str(), "", &empty_id, "x");
    empty.subject = Some(String::new());
    go(&pool, &auth, &empty.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    let empty_row = messages::get(&pool, &f, &inbox, &MessageId::new(&empty_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty_row.item.subject, None, "empty subject is None, not \"\"");

    let re_id = mid("re-only");
    let re = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "Re:", &re_id, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let re_row = messages::get(&pool, &f, &inbox, &MessageId::new(&re_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(re_row.item.subject.as_deref(), Some("Re:"));
    assert_ne!(re.thread_id, ta.thread_id);

    let strip_id = mid("ws");
    go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "foo   ", &strip_id, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let strip_row = messages::get(&pool, &f, &inbox, &MessageId::new(&strip_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        strip_row.item.subject.as_deref(),
        Some("foo"),
        "R5: trailing subject whitespace is stripped"
    );

    let long_id = mid("long");
    let long_subj = "x".repeat(10 * 1024);
    go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), &long_subj, &long_id, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let long_row = messages::get(&pool, &f, &inbox, &MessageId::new(&long_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(long_row.item.subject.as_deref(), Some(long_subj.as_str()));

    let enc_id = mid("rfc2047");
    let enc = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "=?UTF-8?Q?Caf=C3=A9?=", &enc_id, "x")
            .render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let enc_row = messages::get(&pool, &f, &inbox, &MessageId::new(&enc_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert!(enc.thread_id != ta.thread_id);
    assert!(enc_row.item.subject.is_some(), "RFC 2047 subject is Some");

    let h1 = mid("homo-1");
    let h2 = mid("homo-2");
    let t1 = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "paypal", &h1, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let t2 = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "раypal", &h2, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    assert_ne!(t1.thread_id, t2.thread_id, "homoglyph subjects do not merge");
}

/// Case 8: In-Reply-To naming nothing this inbox holds → new thread_id.
#[tokio::test]
async fn in_reply_to_unknown_id_opens_a_new_thread() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let root = mid("known");
    let orphan = mid("orphan");
    let parent = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "root", &root, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();

    let mut spec = MimeSpec::simple("a@probe.test", inbox.as_str(), "ghost", &orphan, "x");
    spec.in_reply_to = Some("<never-seen@elsewhere.test>".into());
    let child = go(&pool, &auth, &spec.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    assert_ne!(child.thread_id, parent.thread_id);
}

/// Case 9: self-only References open a new thread; 500-entry References finish
/// in bounded time and store a thread_id.
#[tokio::test]
async fn self_references_and_long_references_chain() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();

    let parent_id = mid("pref");
    let child_id = mid("cref");
    let parent = go(
        &pool,
        &auth,
        &MimeSpec::simple("a@probe.test", inbox.as_str(), "parent", &parent_id, "x").render(),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let mut child = MimeSpec::simple("a@probe.test", inbox.as_str(), "child", &child_id, "x");
    child.references = Some(format!("{child_id} {parent_id}"));
    let joined = go(&pool, &auth, &child.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    let f = filter(&org, pod, &inbox);
    let child_row = messages::get(&pool, &f, &inbox, &MessageId::new(&child_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        child_row.item.thread_id, parent.thread_id,
        "References naming self and parent still join the parent"
    );
    assert_eq!(joined.thread_id, parent.thread_id);

    let long_id = mid("refs500");
    let refs: String = (0..500)
        .map(|i| format!("<filler-{i}@nowhere.test> "))
        .collect();
    let mut long = MimeSpec::simple("a@probe.test", inbox.as_str(), "long", &long_id, "x");
    long.references = Some(refs);
    let start = Instant::now();
    let stored = go(&pool, &auth, &long.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    assert!(
        start.elapsed().as_secs() < 15,
        "500-entry References must not hang (elapsed {:?})",
        start.elapsed()
    );
    let row = messages::get(&pool, &f, &inbox, &MessageId::new(&long_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.item.thread_id, stored.thread_id);
}

/// Case 10: same Message-ID to a second inbox is stored there with that inbox's thread_id.
#[tokio::test]
async fn same_message_id_in_a_second_inbox_is_that_inboxs_own_thread() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = seed_org(&pool).await;
    let pod = seed_pod(&pool, &org).await;
    let a = seed_inbox_at(&pool, &org, pod, &format!("a-{}@local.test", unique_suffix())).await;
    let b = seed_inbox_at(&pool, &org, pod, &format!("b-{}@local.test", unique_suffix())).await;
    let auth = Authenticator::unresolved_is_none();
    let id = mid("shared");
    let raw = MimeSpec::simple("eve@probe.test", a.as_str(), "e", &id, "x").render();

    let first = go(&pool, &auth, &raw, "eve@probe.test", &org, pod, &a)
        .await
        .unwrap();
    let second = go(&pool, &auth, &raw, "eve@probe.test", &org, pod, &b)
        .await
        .unwrap();
    assert_ne!(first.thread_id, second.thread_id);
    assert_eq!(first.inbox_id, a);
    assert_eq!(second.inbox_id, b);

    let fa = filter(&org, pod, &a);
    let fb = filter(&org, pod, &b);
    assert!(messages::get(&pool, &fa, &a, &MessageId::new(&id), &[])
        .await
        .unwrap()
        .is_some());
    assert!(messages::get(&pool, &fb, &b, &MessageId::new(&id), &[])
        .await
        .unwrap()
        .is_some());
}

/// Case 11: missing Message-ID → 554, store empty. Duplicate in the same inbox →
/// 554, original row unchanged.
#[tokio::test]
async fn missing_and_duplicate_message_id_are_554_and_leave_store() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let f = filter(&org, pod, &inbox);

    let mut missing = MimeSpec::simple("a@probe.test", inbox.as_str(), "x", "<unused@x>", "x");
    missing.message_id = None;
    assert_554(
        go(&pool, &auth, &missing.render(), "a@probe.test", &org, pod, &inbox)
            .await
            .unwrap_err(),
    );
    let listed = messages::list(
        &pool,
        &f,
        &[],
        ListMessagesQuery { limit: 10, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert!(listed.items.is_empty());

    let id = mid("dup");
    let first = MimeSpec::simple("a@probe.test", inbox.as_str(), "original", &id, "first");
    go(&pool, &auth, &first.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    let mut second = MimeSpec::simple("b@probe.test", inbox.as_str(), "changed", &id, "second");
    second.from = "b@probe.test".into();
    assert_554(
        go(&pool, &auth, &second.render(), "b@probe.test", &org, pod, &inbox)
            .await
            .unwrap_err(),
    );
    let row = messages::get(&pool, &f, &inbox, &MessageId::new(&id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.item.subject.as_deref(), Some("original"));
    assert_eq!(row.item.from, "a@probe.test");
}

/// Case 12: stored `from` is the header From; missing To → `[]`; multiple From → 554.
#[tokio::test]
async fn envelope_from_is_not_stored_missing_to_is_empty_multiple_from_is_554() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::envelope_spf_pass("pass.test");
    let f = filter(&org, pod, &inbox);

    let id = mid("env");
    let raw =
        MimeSpec::simple("Alice Probe <alice@pass.test>", inbox.as_str(), "s", &id, "x").render();
    go(&pool, &auth, &raw, "other@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    let row = messages::get(&pool, &f, &inbox, &MessageId::new(&id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.item.from, "Alice Probe <alice@pass.test>");
    let set: BTreeSet<&str> = row.item.labels.iter().map(String::as_str).collect();
    assert!(
        set.contains(labels::UNAUTHENTICATED),
        "SPF follows envelope MAIL FROM (probe.test = none), not header From (pass.test)"
    );

    let no_to_id = mid("noto");
    let mut no_to = MimeSpec::simple("a@probe.test", inbox.as_str(), "s", &no_to_id, "x");
    no_to.to = None;
    go(&pool, &auth, &no_to.render(), "a@probe.test", &org, pod, &inbox)
        .await
        .unwrap();
    let row = messages::get(&pool, &f, &inbox, &MessageId::new(&no_to_id), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.item.to, Vec::<String>::new(), "missing To is [] not RCPT");

    let multi_id = mid("multifrom");
    let mut multi = MimeSpec::simple("a@probe.test", inbox.as_str(), "s", &multi_id, "x");
    multi.extra_headers.push("From: second@probe.test".into());
    assert_554(
        go(&pool, &auth, &multi.render(), "a@probe.test", &org, pod, &inbox)
            .await
            .unwrap_err(),
    );
    assert!(messages::get(&pool, &f, &inbox, &MessageId::new(&multi_id), &[])
        .await
        .unwrap()
        .is_none());
}

/// Case 13: CR/LF in parsed From, To, or Subject → 554, store empty. One test per field.
#[tokio::test]
async fn crlf_in_from_is_554() {
    crlf_field("From", |s| {
        s.from = "=?utf-8?q?evil=0D=0Ainjected?= <a@probe.test>".into();
    })
    .await;
}

#[tokio::test]
async fn crlf_in_to_is_554() {
    crlf_field("To", |s| {
        s.to = Some("=?utf-8?q?evil=0D=0Ainjected?= <a@local.test>".into());
    })
    .await;
}

#[tokio::test]
async fn crlf_in_subject_is_554() {
    crlf_field("Subject", |s| {
        s.subject = Some("=?utf-8?q?evil=0D=0Ainjected?=".into());
    })
    .await;
}

async fn crlf_field(field: &str, tweak: impl FnOnce(&mut MimeSpec)) {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let id = mid(&format!("crlf-{}", field.to_ascii_lowercase()));
    let mut spec = MimeSpec::simple("a@probe.test", inbox.as_str(), "s", &id, "x");
    tweak(&mut spec);
    assert_554(
        go(&pool, &auth, &spec.render(), "a@probe.test", &org, pod, &inbox)
            .await
            .unwrap_err(),
    );
    let f = filter(&org, pod, &inbox);
    assert!(
        messages::get(&pool, &f, &inbox, &MessageId::new(&id), &[])
            .await
            .unwrap()
            .is_none(),
        "{field}: store must stay empty"
    );
}

/// Case 14: hostile MIME is 554 and stores nothing.
#[tokio::test]
async fn hostile_mime_is_554_and_store_empty() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let f = filter(&org, pod, &inbox);

    let cases: &[(&str, Vec<u8>)] = &[
        ("unterminated-boundary", unterminated_boundary(inbox.as_str())),
        ("8bit-header", eight_bit_header(inbox.as_str())),
        ("missing-content-type", missing_content_type(inbox.as_str())),
        ("conflicting-cte", conflicting_cte(inbox.as_str())),
        ("nested-bomb", nested_multipart_bomb(inbox.as_str())),
    ];
    for (name, raw) in cases {
        let err = go(&pool, &auth, raw, "a@probe.test", &org, pod, &inbox)
            .await
            .expect_err(name);
        assert_554(err);
    }
    let listed = messages::list(
        &pool,
        &f,
        &[],
        ListMessagesQuery { limit: 50, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert!(listed.items.is_empty(), "hostile MIME must store nothing");
}

/// Case 15: traversal / NUL filename → 554; 200-char name is stored exactly.
#[tokio::test]
async fn attachment_filename_traversal_and_nul_rejected_200_char_kept() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = seed_org_pod_inbox(&pool).await;
    let auth = Authenticator::unresolved_is_none();
    let f = filter(&org, pod, &inbox);

    let trav = mid("trav");
    assert_554(
        go(
            &pool,
            &auth,
            &attachment_message(&trav, inbox.as_str(), "../../etc/passwd"),
            "a@probe.test",
            &org,
            pod,
            &inbox,
        )
        .await
        .unwrap_err(),
    );
    assert!(messages::get(&pool, &f, &inbox, &MessageId::new(&trav), &[])
        .await
        .unwrap()
        .is_none());

    let nul = mid("nul");
    assert_554(
        go(
            &pool,
            &auth,
            &attachment_message(&nul, inbox.as_str(), "ok\0bad.bin"),
            "a@probe.test",
            &org,
            pod,
            &inbox,
        )
        .await
        .unwrap_err(),
    );
    assert!(messages::get(&pool, &f, &inbox, &MessageId::new(&nul), &[])
        .await
        .unwrap()
        .is_none());

    let long_name: String = "n".repeat(200);
    let long_id = mid("fn200");
    go(
        &pool,
        &auth,
        &attachment_message(&long_id, inbox.as_str(), &long_name),
        "a@probe.test",
        &org,
        pod,
        &inbox,
    )
    .await
    .unwrap();
    let row = messages::get(&pool, &f, &inbox, &MessageId::new(&long_id), &[])
        .await
        .unwrap()
        .unwrap();
    let filename = row
        .item
        .attachments
        .as_ref()
        .and_then(|a| a.first())
        .and_then(|a| a.filename.as_deref());
    assert_eq!(filename, Some(long_name.as_str()));
}

fn unterminated_boundary(to: &str) -> Vec<u8> {
    format!(
        "From: a@probe.test\r\nTo: {to}\r\nSubject: u\r\nMessage-ID: {}\r\n\
         Content-Type: multipart/mixed; boundary=foo\r\n\r\n--foo\r\n\
         Content-Type: text/plain\r\n\r\nhi\r\n",
        mid("ut")
    )
    .into_bytes()
}

fn eight_bit_header(to: &str) -> Vec<u8> {
    let mut raw = format!("From: a@probe.test\r\nTo: {to}\r\nSubject: ").into_bytes();
    raw.push(0xFF);
    raw.extend_from_slice(
        format!("\r\nMessage-ID: {}\r\nContent-Type: text/plain\r\n\r\nbody\r\n", mid("8bit"))
            .as_bytes(),
    );
    raw
}

fn missing_content_type(to: &str) -> Vec<u8> {
    format!(
        "From: a@probe.test\r\nTo: {to}\r\nSubject: s\r\nMessage-ID: {}\r\n\r\nbody\r\n",
        mid("noct")
    )
    .into_bytes()
}

fn conflicting_cte(to: &str) -> Vec<u8> {
    format!(
        "From: a@probe.test\r\nTo: {to}\r\nSubject: s\r\nMessage-ID: {}\r\n\
         Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\
         Content-Transfer-Encoding: quoted-printable\r\n\r\nYQ==\r\n",
        mid("cte")
    )
    .into_bytes()
}

fn nested_multipart_bomb(to: &str) -> Vec<u8> {
    let mut s = format!(
        "From: a@probe.test\r\nTo: {to}\r\nSubject: bomb\r\nMessage-ID: {}\r\n\
         Content-Type: multipart/mixed; boundary=b0\r\n\r\n",
        mid("bomb")
    );
    for i in 0..8 {
        s.push_str(&format!(
            "--b{i}\r\nContent-Type: multipart/mixed; boundary=b{}\r\n\r\n",
            i + 1
        ));
    }
    s.push_str("Content-Type: text/plain\r\n\r\nx\r\n");
    for i in (0..9).rev() {
        s.push_str(&format!("--b{i}--\r\n"));
    }
    s.into_bytes()
}

fn attachment_message(message_id: &str, to: &str, filename: &str) -> Vec<u8> {
    format!(
        "From: a@probe.test\r\nTo: {to}\r\nSubject: att\r\nMessage-ID: {message_id}\r\n\
         Content-Type: multipart/mixed; boundary=bnd\r\n\r\n\
         --bnd\r\nContent-Type: text/plain\r\n\r\nhello\r\n\
         --bnd\r\nContent-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=\"{filename}\"\r\n\r\nXYZ\r\n\
         --bnd--\r\n"
    )
    .into_bytes()
}
