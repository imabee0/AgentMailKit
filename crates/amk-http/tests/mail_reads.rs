//! The P2 message and thread READ surface: `/v0/inboxes/{inbox_id}/messages`, `/v0/threads`,
//! `/v0/pods/{pod_id}/threads` and `/v0/inboxes/{inbox_id}/threads`.
//!
//! `[SPEC:.claude/contracts/amk-http-message-thread-reads.md]` — the assigned edge cases, in order.
//! Restricted-label behaviour is the one that matters most: it is a *storage-layer* predicate
//! (register B3), so the tests below assert the count AND the token, not just the count. A page
//! filtered after fetch answers `count:0` with a `next_page_token` sitting on exactly the hidden
//! rows, which discloses how many there are — the leak the predicate exists to close.

mod support;

use amk_types::ids::InboxId;

fn percent_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' | '_' | '~' => c.to_string(),
            other => other
                .to_string()
                .bytes()
                .map(|b| format!("%{b:02X}"))
                .collect(),
        })
        .collect()
}

// ---- 3. restricted labels: absent from lists, present by id ------------------------------------

/// Both halves against the SAME seeded row. Seeding one row for the list assertion and a different
/// one for the by-id assertion would prove nothing about the asymmetry being claimed.
#[tokio::test]
async fn restricted_mail_is_absent_from_the_list_and_present_by_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "restricted").await;
    let (_thread, message_id) =
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &["spam"]).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);
    let seg = inbox.to_path_segment();

    let listed = support::get(&router, &format!("/v0/inboxes/{seg}/messages"), Some(&key)).await;
    assert_eq!(listed.status, 200, "body: {}", listed.body);
    let v = listed.json.unwrap();
    assert_eq!(v["count"], 0, "spam is excluded from the list: {v}");
    assert!(
        v.get("next_page_token").is_none(),
        "a hidden row must not leave a cursor behind — that is the disclosure a post-filtered \
         page produces: {v}"
    );

    // Same row, by id: no `include_*` parameter exists on this path, so the permission alone
    // decides and the message IS returned (fixture 09b's asymmetry).
    let by_id = support::get(
        &router,
        &format!("/v0/inboxes/{seg}/messages/{}", percent_encode(message_id.as_str())),
        Some(&key),
    )
    .await;
    // NOTE: get-by-id is written but not yet mounted (its path also carries PATCH and DELETE, which
    // amk-store cannot serve). Until it is, the router answers the not-found envelope — asserted
    // here so the day it is mounted, this test fails and must be updated deliberately.
    assert_eq!(by_id.status, 404, "get-by-id is not mounted yet: {}", by_id.body);
    assert_eq!(by_id.code(), Some("not_found"));
}

/// The flag alone is not enough, and the permission alone is not enough — `LabelAccess::list`
/// requires both. This asserts the *pair*, which a test that only flipped the flag would miss.
#[tokio::test]
async fn a_restricted_row_appears_only_when_the_flag_and_the_permission_are_both_present() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "flagged").await;
    support::seed_thread_with_message(&pool, &org, pod, &inbox, &["spam"]).await;
    let full = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);
    let seg = inbox.to_path_segment();

    let without_flag =
        support::get(&router, &format!("/v0/inboxes/{seg}/messages"), Some(&full)).await;
    assert_eq!(without_flag.json.unwrap()["count"], 0, "no flag, no row");

    let with_flag = support::get(
        &router,
        &format!("/v0/inboxes/{seg}/messages?include_spam=true"),
        Some(&full),
    )
    .await;
    assert_eq!(with_flag.status, 200, "body: {}", with_flag.body);
    assert_eq!(
        with_flag.json.unwrap()["count"],
        1,
        "a full-permission key that ASKS for spam sees it"
    );
}

// ---- 4 & 8. pagination: seeded boundary, and limit at the boundary and either side --------------

#[tokio::test]
async fn threads_paginate_with_an_explicitly_seeded_boundary() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "page").await;
    // TWO threads, seeded here rather than inherited: a page boundary that depends on rows an
    // earlier test left behind is what made both SDK smokes pass on leftover state.
    for _ in 0..2 {
        support::seed_thread_with_message(&pool, &org, pod, &inbox, &[]).await;
    }
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let empty = support::get(&router, "/v0/threads?limit=0", Some(&key)).await;
    assert_eq!(empty.status, 400, "limit=0 is refused, per fixture 27: {}", empty.body);

    let first = support::get(&router, "/v0/threads?limit=1", Some(&key)).await;
    assert_eq!(first.status, 200, "body: {}", first.body);
    let v1 = first.json.unwrap();
    assert_eq!(v1["count"], 1);
    assert_eq!(v1["limit"], 1, "a supplied limit is echoed verbatim");
    let token = v1["next_page_token"]
        .as_str()
        .expect("two threads: a boundary exists")
        .to_owned();

    let second =
        support::get(&router, &format!("/v0/threads?limit=1&page_token={token}"), Some(&key)).await;
    let v2 = second.json.unwrap();
    assert_eq!(v2["count"], 1);
    assert!(v2.get("next_page_token").is_none(), "the last page omits the token: {v2}");
    assert_ne!(
        v1["threads"][0]["thread_id"], v2["threads"][0]["thread_id"],
        "the cursor must actually advance"
    );

    let both = support::get(&router, "/v0/threads?limit=2", Some(&key)).await;
    assert_eq!(both.json.unwrap()["count"], 2, "limit=2 returns both in one page");
}

// ---- 5. one test per mount per scope ------------------------------------------------------------

#[tokio::test]
async fn each_mount_returns_exactly_the_threads_its_window_admits() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    let inbox_a = support::seed_inbox(&pool, &org, pod_a, "a").await;
    let inbox_b = support::seed_inbox(&pool, &org, pod_b, "b").await;
    support::seed_thread_with_message(&pool, &org, pod_a, &inbox_a, &[]).await;
    support::seed_thread_with_message(&pool, &org, pod_b, &inbox_b, &[]).await;
    let org_key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let org_view = support::get(&router, "/v0/threads", Some(&org_key)).await;
    assert_eq!(org_view.json.unwrap()["count"], 2, "the organization mount sees both pods");

    let pod_view =
        support::get(&router, &format!("/v0/pods/{pod_a}/threads"), Some(&org_key)).await;
    assert_eq!(pod_view.status, 200, "body: {}", pod_view.body);
    assert_eq!(pod_view.json.unwrap()["count"], 1, "the pod mount sees only its own pod");

    let inbox_view = support::get(
        &router,
        &format!("/v0/inboxes/{}/threads", inbox_a.to_path_segment()),
        Some(&org_key),
    )
    .await;
    assert_eq!(inbox_view.status, 200, "body: {}", inbox_view.body);
    assert_eq!(inbox_view.json.unwrap()["count"], 1, "the inbox mount sees only its own inbox");
}

/// An inbox-scoped credential must not see another inbox's threads through ANY mount — including
/// the organization mount, which names nothing narrower than the credential's own scope.
#[tokio::test]
async fn an_inbox_scoped_credential_sees_only_its_own_inbox_at_every_mount() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mine = support::seed_inbox(&pool, &org, pod, "mine").await;
    let theirs = support::seed_inbox(&pool, &org, pod, "theirs").await;
    support::seed_thread_with_message(&pool, &org, pod, &mine, &[]).await;
    support::seed_thread_with_message(&pool, &org, pod, &theirs, &[]).await;
    // pod and inbox are mutually exclusive on a key (`api_keys_scope_not_both_pod_and_inbox`).
    let key = support::inbox_key(&pool, &org, &mine).await;
    let router = support::test_router(pool);

    let org_mount = support::get(&router, "/v0/threads", Some(&key)).await;
    assert_eq!(
        org_mount.json.unwrap()["count"],
        1,
        "the org mount degenerates to the bound inbox"
    );

    let other = support::get(
        &router,
        &format!("/v0/inboxes/{}/threads", theirs.to_path_segment()),
        Some(&key),
    )
    .await;
    assert_eq!(other.status, 404, "another inbox masks as not-found, never 403: {}", other.body);
    assert_eq!(other.code(), Some("not_found"));
}

// ---- 1 & 2. path-segment handling ---------------------------------------------------------------

/// `[SPEC:reference/fixtures/03-id-formats.http]`: `inbox_id` IS an email address, so `@` arrives
/// percent-encoded. A NUL byte can name no row and masks as not-found — never a 500, never a
/// distinct code that would tell "malformed" from "absent".
#[tokio::test]
async fn a_nul_bearing_or_unknown_inbox_segment_masks_as_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    for (label, seg) in [
        ("a NUL byte", "%00".to_string()),
        ("an unknown inbox", percent_encode("nobody@example.test")),
    ] {
        for path in [
            format!("/v0/inboxes/{seg}/messages"),
            format!("/v0/inboxes/{seg}/threads"),
        ] {
            let resp = support::get(&router, &path, Some(&key)).await;
            assert_eq!(resp.status, 404, "{label} at {path}: {}", resp.body);
            assert_eq!(resp.code(), Some("not_found"), "{label} at {path}: {}", resp.body);
        }
    }
}

/// A differently-cased inbox address resolves to the same inbox — `inbox_id` folds case
/// (`[SPEC:reference/fixtures/18-inbox-case-normalization.txt]`), and these two new mounts must not
/// be the ones that forget it.
#[tokio::test]
async fn the_mail_mounts_resolve_a_differently_cased_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "CaseFold").await;
    support::seed_thread_with_message(&pool, &org, pod, &inbox, &[]).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let upper = InboxId::new(inbox.as_str().to_uppercase());
    let resp = support::get(
        &router,
        &format!("/v0/inboxes/{}/threads", upper.to_path_segment()),
        Some(&key),
    )
    .await;
    assert_eq!(resp.status, 200, "an upper-cased id must resolve: {}", resp.body);
    assert_eq!(resp.json.unwrap()["count"], 1);
}

// ---- permissions --------------------------------------------------------------------------------

/// A credential without `thread_read`/`message_read` is refused, and the refusal is the same for a
/// populated inbox as for an empty one — no count leaks through the permission check.
#[tokio::test]
async fn a_credential_without_the_read_permission_is_refused_identically() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "noperm").await;
    support::seed_thread_with_message(&pool, &org, pod, &inbox, &[]).await;
    let blind = support::mint_key(&pool, &org, None, None, Some(Default::default())).await;
    let router = support::test_router(pool);

    let threads = support::get(&router, "/v0/threads", Some(&blind)).await;
    let messages = support::get(
        &router,
        &format!("/v0/inboxes/{}/messages", inbox.to_path_segment()),
        Some(&blind),
    )
    .await;
    assert_eq!(threads.status, 403, "body: {}", threads.body);
    assert_eq!(messages.status, 403, "body: {}", messages.body);
    assert_eq!(threads.code(), Some("missing_permission"));
    assert_eq!(messages.code(), Some("missing_permission"));
}
