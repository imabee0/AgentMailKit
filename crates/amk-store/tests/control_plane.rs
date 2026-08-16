//! Organizations / pods / inboxes: create/get/list/delete, idempotent `client_id` replay, and the
//! two edge cases the dispatch names explicitly:
//! * two simultaneous creates of the same inbox username → exactly one wins, via real database
//!   concurrency, not a check-then-insert race;
//! * a case-variant `inbox_id` resolves to one row, and two case-variant usernames collide.

mod support;

use amk_store::api_keys::{self, NewApiKey};
use amk_store::inboxes::{self, ListInboxesQuery, NewInbox};
use amk_store::pods::{self, ListPodsQuery, NewPod};
use amk_store::{organizations, InboxCursor, PageTokenError, PodCursor, SortDirection, StoreError};
use amk_types::ids::{InboxId, OrganizationId, PodId};
use amk_types::inbox::{Metadata, MetadataUpdate, MetadataValue, UpdateInboxRequest};
use sqlx::PgPool;
use std::collections::BTreeSet;
use uuid::Uuid;

fn no_cursor_asc(limit: u64) -> ListPodsQuery {
    ListPodsQuery { limit, direction: SortDirection::Ascending, cursor: None }
}

fn no_cursor_asc_inboxes(limit: u64) -> ListInboxesQuery {
    ListInboxesQuery { limit, direction: SortDirection::Ascending, cursor: None }
}

/// Build a [`Metadata`] map from literal pairs — used only by `inboxes::update`'s tests below.
fn md(pairs: &[(&str, MetadataValue)]) -> Metadata {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// Build a `MetadataUpdate::Merge` map from literal pairs, `None` meaning "delete this key".
fn merge(pairs: &[(&str, Option<MetadataValue>)]) -> MetadataUpdate {
    MetadataUpdate::Merge(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

// `organizations::list` is deleted (dispatch contract decision 5): it took no credential and
// returned every organization in the deployment, with no wire route behind it —
// `GET /v0/organizations` returns *the* organization for the authenticated key and calls
// `organizations::get`, never this. This test's only use of `list` was a redundant re-check of
// exactly what `get` already establishes below, so it is rewritten against `get`, not replaced
// with an equivalent.
#[tokio::test]
async fn organization_create_get_delete_round_trips() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let org = support::seed_org(&pool).await;
    let fetched = organizations::get(&pool, &org)
        .await
        .unwrap()
        .expect("just created");
    assert_eq!(fetched.organization_id, org);
    assert_eq!(fetched.inbox_count, 0, "no inboxes yet");

    assert!(organizations::delete(&pool, &org).await.unwrap());
    assert!(organizations::get(&pool, &org).await.unwrap().is_none());
    // Deleting an already-absent organization is a no-op, not an error.
    assert!(!organizations::delete(&pool, &org).await.unwrap());
}

#[tokio::test]
async fn pod_client_id_replay_returns_the_original_row_not_a_duplicate() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let org = support::seed_org(&pool).await;
    let client_id = format!("replay-{}", support::unique_suffix());
    let new = || NewPod {
        organization_id: org.clone(),
        pod_id: PodId::new_random(),
        client_id: Some(client_id.clone()),
        name: "replayed-pod".into(),
    };

    let first = pods::create(&pool, new()).await.unwrap();
    let second = pods::create(&pool, new()).await.unwrap();
    assert_eq!(
        first.pod_id, second.pod_id,
        "replay must return the original pod, not a new one"
    );

    let all = pods::list(&pool, &org, no_cursor_asc(10)).await.unwrap();
    assert_eq!(all.items.len(), 1, "exactly one row must exist for the replayed client_id");
}

/// The `client_id` replay `SELECT` in `pods::create`'s fallback path is itself organization-scoped
/// (`WHERE organization_id = $1 AND client_id = $2`), but nothing asserted that before: two
/// different organizations replaying the *same* `client_id` string must get two independent pods,
/// and each organization's own replay must return its own pod, never the other tenant's.
#[tokio::test]
async fn pod_client_id_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let org_a = support::seed_org(&pool).await;
    let org_b = support::seed_org(&pool).await;
    let client_id = format!("shared-{}", support::unique_suffix());

    let pod_a = pods::create(
        &pool,
        NewPod {
            organization_id: org_a.clone(),
            pod_id: PodId::new_random(),
            client_id: Some(client_id.clone()),
            name: "org-a-pod".into(),
        },
    )
    .await
    .unwrap();
    let pod_b = pods::create(
        &pool,
        NewPod {
            organization_id: org_b.clone(),
            pod_id: PodId::new_random(),
            client_id: Some(client_id.clone()),
            name: "org-b-pod".into(),
        },
    )
    .await
    .unwrap();
    assert_ne!(
        pod_a.pod_id, pod_b.pod_id,
        "the same client_id string in two different organizations must create two independent \
         pods, not collide"
    );

    // Replay under EACH org must return that org's own pod. Checking only one direction is not
    // reliable proof: the fallback `SELECT` carries no `ORDER BY`, so if its organization_id pin
    // were dropped, which of the two colliding rows an unordered scan returns first is a
    // physical-layout accident — a single-direction check could pass by that accident alone. With
    // both directions checked, the pin-dropped scan can favour at most ONE fixed row for both
    // calls, so at most one of the two checks below could coincidentally pass; the other is
    // guaranteed to observe the wrong tenant's row.
    let replay_a = pods::create(
        &pool,
        NewPod {
            organization_id: org_a.clone(),
            pod_id: PodId::new_random(),
            client_id: Some(client_id.clone()),
            name: "org-a-pod-replay".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        replay_a.pod_id, pod_a.pod_id,
        "a replay under org_a must hand back org_a's own pod, not org_b's"
    );

    let replay_b = pods::create(
        &pool,
        NewPod {
            organization_id: org_b.clone(),
            pod_id: PodId::new_random(),
            client_id: Some(client_id.clone()),
            name: "org-b-pod-replay".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        replay_b.pod_id, pod_b.pod_id,
        "a replay under org_b must hand back org_b's own pod, not org_a's"
    );
}

/// Isolates `pods::get`'s organization pin: `pod_id` is a globally unique UUID (naming it directly
/// carries no secrecy), so this is a real cross-tenant read if the pin is ever dropped.
#[tokio::test]
async fn pod_get_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org_a, pod_a, _) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;

    let leaked = pods::get(&pool, &org_b, pod_a).await.unwrap();
    assert!(leaked.is_none(), "org_b must not read org_a's pod by naming its id directly");
    let _ = org_a; // kept for readability of the seeded triple
}

/// Isolates `pods::delete`'s organization pin — a destructive cross-tenant write if dropped.
#[tokio::test]
async fn pod_delete_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let org_a = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org_a).await;
    let org_b = support::seed_org(&pool).await;

    assert!(
        !pods::delete(&pool, &org_b, pod_a).await.unwrap(),
        "org_b must not delete org_a's pod by naming its id directly"
    );
    assert!(
        pods::get(&pool, &org_a, pod_a).await.unwrap().is_some(),
        "the pod must survive the cross-org delete attempt"
    );

    assert!(pods::delete(&pool, &org_a, pod_a).await.unwrap());
    assert!(pods::get(&pool, &org_a, pod_a).await.unwrap().is_none());
}

/// A pod id that was never created at all — distinct from `pod_delete_is_scoped_to_its_organization`
/// above, which exercises a *real* pod under the wrong organization. Both are `Ok(false)`, but for
/// different reasons, and neither must ever surface as `PodNotEmpty`: that variant means "a real
/// row still references this pod", which cannot be true of a pod that never existed.
#[tokio::test]
async fn pod_delete_on_an_absent_pod_is_ok_false_never_pod_not_empty() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let never_created = PodId::new_random();

    let result = pods::delete(&pool, &org, never_created).await;
    assert!(
        matches!(result, Ok(false)),
        "an absent pod must be Ok(false), never PodNotEmpty or a database error: {result:?}"
    );
}

/// Fixture 22's own scenario, the primary case decision 2 exists for: a pod that still owns an
/// inbox refuses to delete. Asserted on the rows, not only the error — fixture 22's refusal is
/// *total*, and a partial delete that then errors would pass an error-only assertion.
#[tokio::test]
async fn pod_delete_on_a_pod_owning_an_inbox_is_rejected_and_both_rows_survive() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let result = pods::delete(&pool, &org, pod).await;
    assert!(
        matches!(result, Err(StoreError::PodNotEmpty)),
        "a pod owning an inbox must be refused: {result:?}"
    );
    assert!(
        pods::get(&pool, &org, pod).await.unwrap().is_some(),
        "the pod must survive the rejected delete"
    );
    assert!(
        inboxes::get(&pool, &org, None, &inbox)
            .await
            .unwrap()
            .is_some(),
        "the inbox must survive the rejected delete too — the refusal is total"
    );
}

#[tokio::test]
async fn inbox_client_id_replay_returns_the_original_row_not_a_duplicate() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let client_id = format!("replay-{}", support::unique_suffix());
    let username = format!("replay-inbox-{}@example.test", support::unique_suffix());
    let new = || NewInbox {
        inbox_id: InboxId::new(username.clone()),
        organization_id: org.clone(),
        pod_id: pod,
        client_id: Some(client_id.clone()),
        display_name: None,
        metadata: None,
    };

    let first = inboxes::create(&pool, new()).await.unwrap();
    let second = inboxes::create(&pool, new()).await.unwrap();
    assert_eq!(first.inbox_id, second.inbox_id);

    let all = inboxes::list(&pool, &org, None, no_cursor_asc_inboxes(10))
        .await
        .unwrap();
    assert_eq!(
        all.items
            .iter()
            .filter(|i| i.client_id.as_deref() == Some(client_id.as_str()))
            .count(),
        1
    );
}

/// Inbox-side sibling of [`pod_client_id_is_scoped_to_its_organization`]: `inboxes::create`'s
/// replay `SELECT` is organization-scoped too, but usernames must still be globally unique
/// (fixture 18 / `inbox_id_is_globally_unique_across_organizations`), so this test uses two
/// distinct usernames under the same shared `client_id` string to isolate the replay path itself
/// from the separate inbox_id uniqueness constraint.
#[tokio::test]
async fn inbox_client_id_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org_a, pod_a, _) = support::seed_org_pod_inbox(&pool).await;
    let (org_b, pod_b, _) = support::seed_org_pod_inbox(&pool).await;
    let client_id = format!("shared-{}", support::unique_suffix());
    let suffix = support::unique_suffix();

    let inbox_a = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(format!("client-a-{suffix}@example.test")),
            organization_id: org_a.clone(),
            pod_id: pod_a,
            client_id: Some(client_id.clone()),
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap();
    let inbox_b = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(format!("client-b-{suffix}@example.test")),
            organization_id: org_b.clone(),
            pod_id: pod_b,
            client_id: Some(client_id.clone()),
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap();
    assert_ne!(inbox_a.inbox_id, inbox_b.inbox_id);

    // Replay under EACH org (same client_id, a THIRD distinct username per org so the attempted
    // insert conflicts only on client_id, never on inbox_id) must hand back that org's own row.
    // Checking only one direction is not reliable proof: the fallback `SELECT` carries no `ORDER
    // BY`, so if its organization_id pin were dropped, which of the two colliding rows an
    // unordered scan happens to return first is a physical-layout accident — a single-direction
    // check can pass by that accident alone. With both directions checked, the (unmutated-query)
    // unordered scan can favour at most ONE fixed row for both calls, so at most one of the two
    // checks below could coincidentally pass if the pin were dropped; the other is guaranteed to
    // observe the wrong tenant's row.
    let replay_a = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(format!("client-a-replay-{suffix}@example.test")),
            organization_id: org_a.clone(),
            pod_id: pod_a,
            client_id: Some(client_id.clone()),
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        replay_a.inbox_id, inbox_a.inbox_id,
        "a replay under org_a must hand back org_a's own inbox, not org_b's"
    );

    let replay_b = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(format!("client-b-replay-{suffix}@example.test")),
            organization_id: org_b.clone(),
            pod_id: pod_b,
            client_id: Some(client_id.clone()),
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        replay_b.inbox_id, inbox_b.inbox_id,
        "a replay under org_b must hand back org_b's own inbox, not org_a's"
    );
}

/// The concurrency edge case: two real, simultaneous `create()` calls for the same normalized
/// username. Exactly one must win; the loser must get [`StoreError::InboxAlreadyExists`], not a
/// generic database error and not a silent duplicate — proving the collision is resolved by the
/// database's own unique index, not by a check-then-insert in this crate.
#[tokio::test]
async fn two_simultaneous_creates_of_the_same_username_collide_exactly_once() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("race-{}@example.test", support::unique_suffix());

    let new = |case: &str| NewInbox {
        inbox_id: InboxId::new(case.to_owned()),
        organization_id: org.clone(),
        pod_id: pod,
        client_id: None,
        display_name: None,
        metadata: None,
    };

    // Two different casings of the SAME address: fixture 18 says both normalize to one inbox, so
    // this is simultaneously the concurrency test and the case-fold-then-collide test.
    let upper = username.to_uppercase();
    let (r1, r2) =
        tokio::join!(inboxes::create(&pool, new(&username)), inboxes::create(&pool, new(&upper)),);

    let results = [r1, r2];
    let wins = results.iter().filter(|r| r.is_ok()).count();
    let collisions = results
        .iter()
        .filter(|r| matches!(r, Err(StoreError::InboxAlreadyExists)))
        .count();
    assert_eq!(wins, 1, "exactly one create must win: {}", debug(&results));
    assert_eq!(collisions, 1, "the other must get the collision error, not a generic DB error");

    let normalized = InboxId::new(username.to_lowercase());
    let rows = inboxes::list(&pool, &org, None, no_cursor_asc_inboxes(10))
        .await
        .unwrap();
    let matching = rows
        .items
        .iter()
        .filter(|i| i.inbox_id == normalized)
        .count();
    assert_eq!(matching, 1, "the database must hold exactly one row for the normalized id");
}

fn debug(results: &[Result<amk_types::Inbox, StoreError>; 2]) -> String {
    format!(
        "[{}, {}]",
        results[0].as_ref().map(|_| "Ok").unwrap_or("Err"),
        results[1].as_ref().map(|_| "Ok").unwrap_or("Err"),
    )
}

#[tokio::test]
async fn case_variant_inbox_id_resolves_to_one_row() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let mixed = format!("MixedCase-{}@Example.Test", support::unique_suffix());
    let created = inboxes::create(
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
    .unwrap();
    assert_eq!(
        created.inbox_id,
        InboxId::new(mixed.to_lowercase()),
        "stored form is lowercased"
    );

    for variant in [mixed.clone(), mixed.to_uppercase(), mixed.to_lowercase()] {
        let found = inboxes::get(&pool, &org, None, &InboxId::new(variant.clone()))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{variant} must resolve to the same inbox"));
        assert_eq!(found.inbox_id, created.inbox_id);
    }
}

#[tokio::test]
async fn inbox_delete_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org_a, pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;

    // org_b must not be able to delete org_a's inbox by naming it.
    assert!(!inboxes::delete(&pool, &org_b, None, &inbox_a)
        .await
        .unwrap());
    assert!(
        inboxes::get(&pool, &org_a, None, &inbox_a)
            .await
            .unwrap()
            .is_some(),
        "must survive"
    );

    assert!(inboxes::delete(&pool, &org_a, None, &inbox_a)
        .await
        .unwrap());
    assert!(inboxes::get(&pool, &org_a, None, &inbox_a)
        .await
        .unwrap()
        .is_none());
    let _ = pod_a; // kept for readability of the seeded triple
}

/// Isolates `inboxes::get`'s organization pin: `inbox_id` **is** the public email address, so
/// naming it directly carries no secrecy at all.
#[tokio::test]
async fn inbox_get_is_scoped_to_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org_a, _pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let org_b = support::seed_org(&pool).await;

    let leaked = inboxes::get(&pool, &org_b, None, &inbox_a).await.unwrap();
    assert!(
        leaked.is_none(),
        "org_b must not read org_a's inbox by naming its address directly"
    );
    let _ = org_a; // kept for readability of the seeded triple
}

/// `inboxes::list` must return the *exact* set for its organization, not merely include it:
/// `.any(...)` stays true even if the list also contains every other tenant's addresses.
#[tokio::test]
async fn inbox_list_returns_the_exact_set_for_its_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org_a, _pod_a, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let (_org_b, _pod_b, _inbox_b) = support::seed_org_pod_inbox(&pool).await;

    let list_a = inboxes::list(&pool, &org_a, None, no_cursor_asc_inboxes(10))
        .await
        .unwrap();
    let ids: Vec<_> = list_a.items.into_iter().map(|i| i.inbox_id).collect();
    assert_eq!(
        ids,
        vec![inbox_a],
        "org_a's inbox list must be exactly its own inboxes, not merely include one"
    );
}

/// `inboxes::delete`'s `inbox_id` parameter must be folded to its normalized form before
/// comparison, exactly like `create` and `get` — every other test reaches `delete` through an
/// already-normalized id (`seed_org_pod_inbox`'s return value), so this bypasses that and passes a
/// raw mixed-case id directly.
#[tokio::test]
async fn inbox_delete_normalizes_a_mixed_case_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };

    let (org, _pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let mixed = InboxId::new(inbox.as_str().to_uppercase());

    assert!(
        inboxes::delete(&pool, &org, None, &mixed).await.unwrap(),
        "delete must normalize a mixed-case inbox_id parameter to match the stored lowercase row"
    );
    assert!(inboxes::get(&pool, &org, None, &inbox)
        .await
        .unwrap()
        .is_none());
}

// ---- hostile bytes reaching SQL (`.claude/contracts/amk-store-id-safety.md`) --------------------
//
// `InboxId::new` is infallible, so a NUL-bearing id can reach these functions regardless of
// caller discipline. Unguarded, it would fail at Postgres parameter encoding (SQLSTATE 22021) —
// a `StoreError::Database`, not the uniform not-found every other unresolvable id produces. Each
// of the five call paths the dispatch contract names gets its own direct test: the previous
// dispatch's fifth review round found a regression test that guarded `get` while `delete`'s call
// site was unprotected, and a mutant left the suite green while an uppercase id really deleted
// the row. Testing through a shared helper would reproduce exactly that gap.

/// `inboxes::get`, one of the five named call paths: a NUL-bearing `inbox_id` must return
/// `Ok(None)`, never `Err(StoreError::Database(_))`.
#[tokio::test]
async fn inbox_get_with_a_nul_byte_in_inbox_id_is_not_found_not_a_database_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let hostile = InboxId::new("abc\0def@example.test");

    let result = inboxes::get(&pool, &org, None, &hostile).await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing inbox_id must mask as not-found, not error: {result:?}"
    );
}

/// `inboxes::delete`, the sibling of [`inbox_get_with_a_nul_byte_in_inbox_id_is_not_found_not_a_database_error`]
/// — tested independently rather than assumed to inherit `get`'s guard.
#[tokio::test]
async fn inbox_delete_with_a_nul_byte_in_inbox_id_is_a_no_op_not_a_database_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let hostile = InboxId::new("abc\0def@example.test");

    let result = inboxes::delete(&pool, &org, None, &hostile).await;
    assert!(
        matches!(result, Ok(false)),
        "a NUL-bearing inbox_id must delete nothing, not error: {result:?}"
    );
}

/// `inboxes::create` with a NUL-bearing `client_id`: unlike the lookups above, this fails at the
/// `INSERT` bind rather than a masked lookup — an ungraceful `StoreError::Database`, not a
/// denial-distinguishing side channel, since create has no not-found outcome to hide behind. The
/// dispatch contract still asks for a decision: guarded the same way, returning
/// [`StoreError::InvalidValue`], because the check is one already-public predicate call and the
/// alternative is a caller-controlled 500 that a wire-body `client_id` can trivially trigger.
#[tokio::test]
async fn inboxes_create_rejects_a_nul_byte_in_client_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("client-id-nul-{}@example.test", support::unique_suffix());

    let result = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: Some("abc\0def".to_owned()),
            display_name: None,
            metadata: None,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("client_id"))),
        "a NUL-bearing client_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `pods::create` sibling of [`inboxes_create_rejects_a_nul_byte_in_client_id`] — same reasoning,
/// tested independently at its own call site.
#[tokio::test]
async fn pods_create_rejects_a_nul_byte_in_client_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let result = pods::create(
        &pool,
        NewPod {
            organization_id: org,
            pod_id: PodId::new_random(),
            client_id: Some("abc\0def".to_owned()),
            name: "hostile-client-id".into(),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("client_id"))),
        "a NUL-bearing client_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

/// `inboxes::create`'s *own* `inbox_id` (the username): caller-supplied in the request body,
/// exactly like `client_id` above, and bound into the same `INSERT` — tested independently at its
/// own call site rather than assumed to be covered by the `client_id` guard sitting next to it.
#[tokio::test]
async fn inboxes_create_rejects_a_nul_byte_in_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;

    let result = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new("abc\0def@example.test"),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("inbox_id"))),
        "a NUL-bearing inbox_id must be a typed InvalidValue, not a raw database error: {result:?}"
    );
}

// ---- pod pin (`.claude/contracts/amk-store-inbox-update.md`) -----------------------------------
//
// `get` and `delete` used to pin `organization_id` only, unlike `list` — a pod-scoped credential
// could resolve, and delete, a sibling pod's inbox in the same organization. Each call path gets
// its own direct test, exactly like the id-safety guards above: testing through a shared helper
// would reproduce the same gap that let `get` be fixed while `delete` stayed open.

/// `inboxes::get` must not resolve an inbox in a sibling pod, even though both pods share the same
/// organization. `inbox_id` is the public email address, so naming it directly carries no secrecy.
#[tokio::test]
async fn inbox_get_is_scoped_to_its_pod_not_just_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    let inbox_a = support::seed_inbox(&pool, &org, pod_a, "pod-a-inbox").await;

    let leaked = inboxes::get(&pool, &org, Some(pod_b), &inbox_a)
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "a pod_b-scoped credential must not read pod_a's inbox in the same organization"
    );

    // Sanity: the same lookup still resolves at pod_a's own scope, and unscoped (organization
    // mount) — this is a pin, not a break.
    assert!(inboxes::get(&pool, &org, Some(pod_a), &inbox_a)
        .await
        .unwrap()
        .is_some());
    assert!(inboxes::get(&pool, &org, None, &inbox_a)
        .await
        .unwrap()
        .is_some());
}

/// `inboxes::delete`, tested independently rather than assumed to inherit `get`'s pin. A denial
/// that still writes is the defect, so the row is asserted unmodified afterwards, not just the
/// return value.
#[tokio::test]
async fn inbox_delete_is_scoped_to_its_pod_not_just_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    let inbox_a = support::seed_inbox(&pool, &org, pod_a, "pod-a-inbox").await;

    assert!(
        !inboxes::delete(&pool, &org, Some(pod_b), &inbox_a)
            .await
            .unwrap(),
        "a pod_b-scoped credential must not delete pod_a's inbox in the same organization"
    );
    assert!(
        inboxes::get(&pool, &org, None, &inbox_a)
            .await
            .unwrap()
            .is_some(),
        "the row must be unmodified after the cross-pod delete attempt"
    );

    assert!(inboxes::delete(&pool, &org, Some(pod_a), &inbox_a)
        .await
        .unwrap());
    assert!(inboxes::get(&pool, &org, None, &inbox_a)
        .await
        .unwrap()
        .is_none());
}

// ---- inboxes::update (`.claude/contracts/amk-store-inbox-update.md`) ---------------------------

async fn seed_inbox_with(
    pool: &sqlx::PgPool,
    org: &amk_types::ids::OrganizationId,
    pod: PodId,
    display_name: Option<&str>,
    metadata: Option<Metadata>,
) -> amk_types::Inbox {
    let inbox_id = InboxId::new(format!("update-{}@example.test", support::unique_suffix()));
    inboxes::create(
        pool,
        NewInbox {
            inbox_id,
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: display_name.map(str::to_owned),
            metadata,
        },
    )
    .await
    .expect("seed inbox")
}

fn no_change() -> UpdateInboxRequest {
    UpdateInboxRequest { display_name: None, metadata: MetadataUpdate::Unchanged }
}

/// A fully-empty request (`display_name` absent, `metadata` absent) is a no-op: no error, the
/// current row is returned unchanged, and `updated_at` does not bump — "sending an empty object
/// is rejected" and "at least one field required" are `amk-http`'s wire-validation rules, not
/// this crate's.
#[tokio::test]
async fn inbox_update_with_a_fully_empty_request_is_a_no_op_and_does_not_bump_updated_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(
        &pool,
        &org,
        pod,
        Some("Original"),
        Some(md(&[("a", MetadataValue::Number(1.0))])),
    )
    .await;

    let updated = inboxes::update(&pool, &org, None, &created.inbox_id, no_change())
        .await
        .unwrap()
        .expect("the inbox still resolves");

    assert_eq!(updated, created, "a fully-empty request must not change anything");
}

/// `Merge` with an empty map is a no-op on metadata specifically — checked independently of
/// [`inbox_update_with_a_fully_empty_request_is_a_no_op_and_does_not_bump_updated_at`] because
/// `display_name` is also present here, so `updated_at` DOES bump: the merge's own no-op-ness
/// must not suppress the bump `display_name`'s presence causes.
#[tokio::test]
async fn inbox_update_merge_empty_is_a_no_op_on_metadata_even_when_display_name_changes() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(
        &pool,
        &org,
        pod,
        Some("Original"),
        Some(md(&[("a", MetadataValue::Number(1.0))])),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest { display_name: Some("New Name".into()), metadata: merge(&[]) },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert_eq!(updated.metadata, created.metadata, "Merge(empty) must not change metadata");
    assert_eq!(updated.display_name.as_deref(), Some("New Name"));
    assert!(
        updated.updated_at.0 > created.updated_at.0,
        "display_name's presence must still bump updated_at"
    );
}

/// `Merge(empty)` combined with an absent `display_name` — the literal "nets to nothing" case:
/// no error, no metadata change, and `updated_at` unchanged, exactly like the fully-empty request.
#[tokio::test]
async fn inbox_update_merge_empty_and_absent_display_name_does_not_bump_updated_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created =
        seed_inbox_with(&pool, &org, pod, None, Some(md(&[("a", MetadataValue::Number(1.0))])))
            .await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest { display_name: None, metadata: merge(&[]) },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert_eq!(updated, created, "Merge(empty) with no other field must be a total no-op");
}

/// `Clear` sets the column to SQL `NULL`, never `{}` — the trap the contract measured against the
/// dev database (`||` and the naive guarded form both fail this).
#[tokio::test]
async fn inbox_update_clear_sets_metadata_to_sql_null_not_empty_object() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created =
        seed_inbox_with(&pool, &org, pod, None, Some(md(&[("a", MetadataValue::Number(1.0))])))
            .await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest { display_name: None, metadata: MetadataUpdate::Clear },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert!(
        updated.metadata.is_none(),
        "Clear must leave the column SQL NULL — Inbox.metadata is None only for a NULL column, \
         never for a stored {{}}: {:?}",
        updated.metadata
    );
    assert!(
        updated.updated_at.0 > created.updated_at.0,
        "an explicit Clear must bump updated_at"
    );
}

/// `Clear` starting from an already-NULL column: still a legitimate explicit action (present on
/// the wire), so it bumps `updated_at` even though the column's value does not change — the same
/// "presence, not value-equality" rule `display_name` gets.
#[tokio::test]
async fn inbox_update_clear_on_already_null_metadata_still_bumps_updated_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;
    assert!(created.metadata.is_none(), "seed must start with no metadata");

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest { display_name: None, metadata: MetadataUpdate::Clear },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert!(updated.metadata.is_none());
    assert!(
        updated.updated_at.0 > created.updated_at.0,
        "Clear is an explicit action and must bump updated_at even from NULL"
    );
}

/// `Merge` adds a new key, overwrites an existing one, and deletes a key mapped to `null` — all
/// three in one call, matching the contract's own verified case:
/// `{"a":1,"b":2} + {"c":3} - {a} => {"b":2,"c":3}`, plus an overwrite of `b`.
#[tokio::test]
async fn inbox_update_merge_adds_overwrites_and_deletes_on_null_value() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(
        &pool,
        &org,
        pod,
        None,
        Some(md(&[
            ("a", MetadataValue::Number(1.0)),
            ("b", MetadataValue::Number(2.0)),
        ])),
    )
    .await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: None,
            metadata: merge(&[
                ("a", None),                                      // delete
                ("b", Some(MetadataValue::Number(99.0))),         // overwrite
                ("c", Some(MetadataValue::String("new".into()))), // add
            ]),
        },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert_eq!(
        updated.metadata,
        Some(md(&[
            ("b", MetadataValue::Number(99.0)),
            ("c", MetadataValue::String("new".into()))
        ])),
        "a must be deleted, b overwritten, c added"
    );
}

/// A `Merge` that deletes a key which was never present is a no-op, not an error — the "netting
/// to nothing" behaviour has to hold even when the merge map itself is non-empty.
#[tokio::test]
async fn inbox_update_merge_with_null_for_a_key_that_does_not_exist_is_a_no_op_not_an_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest { display_name: None, metadata: merge(&[("missing", None)]) },
    )
    .await
    .unwrap()
    .expect("must not error");

    assert!(
        updated.metadata.is_none(),
        "deleting a nonexistent key from NULL metadata stays NULL"
    );
}

/// `display_name`'s bump rule is presence, not value-equality: resending the byte-identical value
/// still bumps `updated_at`.
#[tokio::test]
async fn inbox_update_resending_the_same_display_name_still_bumps_updated_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, Some("Same Name"), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: Some("Same Name".into()),
            metadata: MetadataUpdate::Unchanged,
        },
    )
    .await
    .unwrap()
    .expect("the inbox still resolves");

    assert_eq!(updated.display_name.as_deref(), Some("Same Name"));
    assert!(
        updated.updated_at.0 > created.updated_at.0,
        "presence, not value equality, must bump updated_at"
    );
}

/// `update` on an inbox that exists, but in a *different* pod of the same organization, must
/// return `Ok(None)` — and the row must be unmodified afterwards. A scope miss that still writes
/// is the defect, so this asserts the target row, not just the return value.
#[tokio::test]
async fn inbox_update_is_scoped_to_its_pod_not_just_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    let created = seed_inbox_with(&pool, &org, pod_a, Some("Original"), None).await;

    let result = inboxes::update(
        &pool,
        &org,
        Some(pod_b),
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: Some("Hijacked".into()),
            metadata: MetadataUpdate::Clear,
        },
    )
    .await
    .unwrap();
    assert!(result.is_none(), "a pod_b-scoped credential must not update pod_a's inbox");

    let after = inboxes::get(&pool, &org, None, &created.inbox_id)
        .await
        .unwrap()
        .expect("the inbox still exists");
    assert_eq!(after, created, "the row must be unmodified after the cross-pod update attempt");
}

/// Fixture 18: `update` resolves its target exactly as `get` does, case-insensitively.
#[tokio::test]
async fn inbox_update_resolves_a_mixed_case_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;
    let mixed = InboxId::new(created.inbox_id.as_str().to_uppercase());

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &mixed,
        UpdateInboxRequest {
            display_name: Some("Cased".into()),
            metadata: MetadataUpdate::Unchanged,
        },
    )
    .await
    .unwrap()
    .expect("a mixed-case inbox_id must still resolve the same row");
    assert_eq!(updated.inbox_id, created.inbox_id);
}

/// `update`'s own `inbox_id` (the lookup) is a NUL-bearing id: masks as `Ok(None)`, exactly like
/// `get`/`delete`, never a raw database error and never distinguishable from a genuine miss.
#[tokio::test]
async fn inbox_update_with_a_nul_byte_in_inbox_id_is_not_found_not_a_database_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let hostile = InboxId::new("abc\0def@example.test");

    let result = inboxes::update(&pool, &org, None, &hostile, no_change()).await;
    assert!(
        matches!(result, Ok(None)),
        "a NUL-bearing inbox_id must mask as not-found, not error: {result:?}"
    );
}

// ---- the five-field text guard table (`.claude/contracts/amk-store-inbox-update.md`) -----------
//
// Each row gets its own hostile test (calling that function directly, never through a shared
// helper) AND a clean-path test for the same field, so a guard widened to reject legitimate input
// (e.g. `is_some()` instead of `is_some_and(has_forbidden_byte)`) fails too.

#[tokio::test]
async fn inboxes_create_rejects_a_nul_byte_in_display_name() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("dn-nul-{}@example.test", support::unique_suffix());

    let result = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: Some("abc\0def".to_owned()),
            metadata: None,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("display_name"))),
        "a NUL-bearing display_name must be a typed InvalidValue: {result:?}"
    );
}

#[tokio::test]
async fn inboxes_create_with_a_clean_display_name_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("dn-clean-{}@example.test", support::unique_suffix());

    let created = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: Some("A Perfectly Normal Name".to_owned()),
            metadata: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(created.display_name.as_deref(), Some("A Perfectly Normal Name"));
}

#[tokio::test]
async fn inboxes_create_rejects_a_nul_byte_in_a_metadata_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("md-key-nul-{}@example.test", support::unique_suffix());

    let result = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: Some(md(&[("bad\0key", MetadataValue::Bool(true))])),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("metadata"))),
        "a NUL-bearing metadata key must be a typed InvalidValue, not a raw database error \
         (a check that only inspects values would miss this): {result:?}"
    );
}

#[tokio::test]
async fn inboxes_create_rejects_a_nul_byte_in_a_metadata_value() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("md-val-nul-{}@example.test", support::unique_suffix());

    let result = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: Some(md(&[("k", MetadataValue::String("bad\0value".into()))])),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("metadata"))),
        "a NUL-bearing metadata value must be a typed InvalidValue, not a raw database error \
         (a check that only inspects keys would miss this): {result:?}"
    );
}

#[tokio::test]
async fn inboxes_create_with_clean_metadata_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let username = format!("md-clean-{}@example.test", support::unique_suffix());

    let created = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: InboxId::new(username),
            organization_id: org,
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: Some(md(&[("k", MetadataValue::String("v".into()))])),
        },
    )
    .await
    .unwrap();
    assert_eq!(created.metadata, Some(md(&[("k", MetadataValue::String("v".into()))])));
}

#[tokio::test]
async fn inboxes_update_rejects_a_nul_byte_in_display_name() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let result = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: Some("abc\0def".into()),
            metadata: MetadataUpdate::Unchanged,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("display_name"))),
        "a NUL-bearing display_name must be a typed InvalidValue: {result:?}"
    );
}

#[tokio::test]
async fn inboxes_update_with_a_clean_display_name_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: Some("Clean Name".into()),
            metadata: MetadataUpdate::Unchanged,
        },
    )
    .await
    .unwrap()
    .expect("must resolve");
    assert_eq!(updated.display_name.as_deref(), Some("Clean Name"));
}

#[tokio::test]
async fn inboxes_update_rejects_a_nul_byte_in_a_metadata_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let result = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: None,
            metadata: merge(&[("bad\0key", Some(MetadataValue::Bool(true)))]),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("metadata"))),
        "a NUL-bearing merge key must be a typed InvalidValue \
         (a check that only inspects values would miss this): {result:?}"
    );
}

#[tokio::test]
async fn inboxes_update_rejects_a_nul_byte_in_a_metadata_value() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let result = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: None,
            metadata: merge(&[("k", Some(MetadataValue::String("bad\0value".into())))]),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("metadata"))),
        "a NUL-bearing merge value must be a typed InvalidValue \
         (a check that only inspects keys would miss this): {result:?}"
    );
}

#[tokio::test]
async fn inboxes_update_with_clean_metadata_merge_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let created = seed_inbox_with(&pool, &org, pod, None, None).await;

    let updated = inboxes::update(
        &pool,
        &org,
        None,
        &created.inbox_id,
        UpdateInboxRequest {
            display_name: None,
            metadata: merge(&[("k", Some(MetadataValue::String("v".into())))]),
        },
    )
    .await
    .unwrap()
    .expect("must resolve");
    assert_eq!(updated.metadata, Some(md(&[("k", MetadataValue::String("v".into()))])));
}

#[tokio::test]
async fn pods_create_rejects_a_nul_byte_in_name() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let result = pods::create(
        &pool,
        NewPod {
            organization_id: org,
            pod_id: PodId::new_random(),
            client_id: None,
            name: "bad\0name".into(),
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("name"))),
        "a NUL-bearing pod name must be a typed InvalidValue: {result:?}"
    );
}

#[tokio::test]
async fn pods_create_with_a_clean_name_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let created = pods::create(
        &pool,
        NewPod {
            organization_id: org,
            pod_id: PodId::new_random(),
            client_id: None,
            name: "A Clean Pod Name".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(created.name, "A Clean Pod Name");
}

#[tokio::test]
async fn api_keys_create_rejects_a_nul_byte_in_name() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let result = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org,
            pod_id: None,
            inbox_id: None,
            name: "bad\0name".into(),
            permissions: None,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("name"))),
        "a NUL-bearing api key name must be a typed InvalidValue: {result:?}"
    );
}

#[tokio::test]
async fn api_keys_create_with_a_clean_name_succeeds() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let created = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org,
            pod_id: None,
            inbox_id: None,
            name: "A Clean Key Name".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(created.name, "A Clean Key Name");
}

// ---- pods::list / inboxes::list keyset pagination (`.claude/contracts/amk-store-http-prereqs.md`) --

async fn walk_pods(
    pool: &PgPool,
    org: &OrganizationId,
    direction: SortDirection,
    limit: u64,
) -> Vec<PodId> {
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = pods::list(pool, org, ListPodsQuery { limit, direction, cursor })
            .await
            .unwrap();
        seen.extend(page.items.into_iter().map(|p| p.pod_id));
        match page.next {
            Some(token) => cursor = Some(PodCursor::decode(&token).unwrap()),
            None => break,
        }
    }
    seen
}

#[tokio::test]
async fn pods_list_full_walk_ascending_sees_every_row_exactly_once() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let mut seeded = BTreeSet::new();
    for _ in 0..5 {
        seeded.insert(support::seed_pod(&pool, &org).await);
    }

    let seen = walk_pods(&pool, &org, SortDirection::Ascending, 2).await;
    assert_eq!(seen.len(), 5, "no duplicate and no omission: {seen:?}");
    assert_eq!(seen.into_iter().collect::<BTreeSet<_>>(), seeded);
}

#[tokio::test]
async fn pods_list_full_walk_descending_is_the_exact_reverse() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    for _ in 0..5 {
        support::seed_pod(&pool, &org).await;
    }

    let ascending = walk_pods(&pool, &org, SortDirection::Ascending, 2).await;
    let mut descending = walk_pods(&pool, &org, SortDirection::Descending, 2).await;
    descending.reverse();
    assert_eq!(ascending, descending);
}

#[tokio::test]
async fn pods_list_with_limit_zero_returns_empty_and_runs_no_query() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // A closed pool makes any real query fail: this proves the `limit == 0` early return happens
    // before `pods::list` ever reaches `sqlx::query(...).fetch_all(pool)`, not merely that the
    // eventual result happens to be empty.
    pool.close().await;

    let page = pods::list(&pool, &org, no_cursor_asc(0)).await.unwrap();
    assert_eq!(page.items, Vec::new());
    assert!(page.next.is_none());
}

#[tokio::test]
async fn pods_list_with_u64_max_limit_returns_every_row_without_panicking() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    for _ in 0..3 {
        support::seed_pod(&pool, &org).await;
    }

    let page = pods::list(
        &pool,
        &org,
        ListPodsQuery { limit: u64::MAX, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(page.next.is_none());
}

/// The test that fails if the primary-key tiebreak is dropped from `ORDER BY`: two pods sharing
/// one `created_at` millisecond, seeded via raw SQL (`pods::create`'s own `INSERT` has no way to
/// pin `created_at` — it is always `DEFAULT now()`) must both be seen exactly once across the
/// walk. `limit: 1` is deliberate, not `walk_pods`'s usual `2`: with exactly two tied rows, a
/// `limit` large enough to return both in one query never exercises a cursor comparison at all,
/// so a dropped `ORDER BY` tiebreak would go uncaught — the walk must actually cross a page
/// boundary between the two tied rows for this test to test anything.
#[tokio::test]
async fn pods_list_breaks_a_created_at_tie_by_pod_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let same_instant = chrono::DateTime::parse_from_rfc3339("2026-08-15T05:00:00.000Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut ids = Vec::new();
    for _ in 0..2 {
        let pod_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pods (pod_id, organization_id, name, created_at) VALUES ($1, $2, 'tie', $3)",
        )
        .bind(pod_id)
        .bind(org.as_str())
        .bind(same_instant)
        .execute(&pool)
        .await
        .unwrap();
        ids.push(PodId::from(pod_id));
    }

    let seen = walk_pods(&pool, &org, SortDirection::Ascending, 1).await;
    assert_eq!(seen.len(), 2, "both same-instant pods must be seen exactly once each: {seen:?}");
    let mut seen_sorted = seen;
    seen_sorted.sort();
    let mut expected_sorted = ids;
    expected_sorted.sort();
    assert_eq!(seen_sorted, expected_sorted);
}

async fn walk_inboxes(
    pool: &PgPool,
    org: &OrganizationId,
    pod: Option<PodId>,
    direction: SortDirection,
    limit: u64,
) -> Vec<InboxId> {
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = inboxes::list(pool, org, pod, ListInboxesQuery { limit, direction, cursor })
            .await
            .unwrap();
        seen.extend(page.items.into_iter().map(|i| i.inbox_id));
        match page.next {
            Some(token) => cursor = Some(InboxCursor::decode(&token, pod).unwrap()),
            None => break,
        }
    }
    seen
}

#[tokio::test]
async fn inboxes_list_full_walk_ascending_sees_every_row_exactly_once() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mut seeded = BTreeSet::new();
    for i in 0..5 {
        seeded.insert(support::seed_inbox(&pool, &org, pod, &format!("walk-{i}")).await);
    }

    let seen = walk_inboxes(&pool, &org, None, SortDirection::Ascending, 2).await;
    assert_eq!(seen.len(), 5, "no duplicate and no omission: {seen:?}");
    assert_eq!(seen.into_iter().collect::<BTreeSet<_>>(), seeded);
}

#[tokio::test]
async fn inboxes_list_full_walk_descending_is_the_exact_reverse() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    for i in 0..5 {
        support::seed_inbox(&pool, &org, pod, &format!("walk-{i}")).await;
    }

    let ascending = walk_inboxes(&pool, &org, None, SortDirection::Ascending, 2).await;
    let mut descending = walk_inboxes(&pool, &org, None, SortDirection::Descending, 2).await;
    descending.reverse();
    assert_eq!(ascending, descending);
}

#[tokio::test]
async fn inboxes_list_with_limit_zero_returns_empty_and_runs_no_query() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    pool.close().await;

    let page = inboxes::list(&pool, &org, None, no_cursor_asc_inboxes(0))
        .await
        .unwrap();
    assert_eq!(page.items, Vec::new());
    assert!(page.next.is_none());
}

#[tokio::test]
async fn inboxes_list_with_u64_max_limit_returns_every_row_without_panicking() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    for i in 0..3 {
        support::seed_inbox(&pool, &org, pod, &format!("max-{i}")).await;
    }

    let page = inboxes::list(
        &pool,
        &org,
        None,
        ListInboxesQuery { limit: u64::MAX, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(page.next.is_none());
}

/// Sibling of [`pods_list_breaks_a_created_at_tie_by_pod_id`]: two inboxes sharing one `created_at`
/// millisecond, seeded via raw SQL for the same reason (`inboxes::create` cannot pin `created_at`).
/// `limit: 1`, deliberately — see that test's own comment on why.
#[tokio::test]
async fn inboxes_list_breaks_a_created_at_tie_by_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let same_instant = chrono::DateTime::parse_from_rfc3339("2026-08-15T05:00:00.000Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut ids = Vec::new();
    for i in 0..2 {
        let inbox_id = format!("tie-{i}-{}@example.test", support::unique_suffix());
        sqlx::query(
            "INSERT INTO inboxes (inbox_id, organization_id, pod_id, created_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&inbox_id)
        .bind(org.as_str())
        .bind(pod.0)
        .bind(same_instant)
        .execute(&pool)
        .await
        .unwrap();
        ids.push(InboxId::new(inbox_id));
    }

    let seen = walk_inboxes(&pool, &org, None, SortDirection::Ascending, 1).await;
    assert_eq!(
        seen.len(),
        2,
        "both same-instant inboxes must be seen exactly once each: {seen:?}"
    );
    let mut seen_sorted: Vec<String> = seen.iter().map(|i| i.as_str().to_owned()).collect();
    seen_sorted.sort();
    let mut expected_sorted: Vec<String> = ids.iter().map(|i| i.as_str().to_owned()).collect();
    expected_sorted.sort();
    assert_eq!(seen_sorted, expected_sorted);
}

/// A page token minted at pod A must not resume the walk at pod B — a hand-decoded `InboxCursor`
/// is asserted on the variant, not `is_err()`, exactly as the pure unit test in `pagination.rs`
/// does; this is the same guarantee exercised through the actual `GET /v0/pods/{pod_id}/inboxes`
/// shape (a real cursor minted by `inboxes::list` itself, not a hand-built one).
#[tokio::test]
async fn inboxes_list_a_pod_a_cursor_is_rejected_at_pod_b() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    support::seed_inbox(&pool, &org, pod_a, "a1").await;
    support::seed_inbox(&pool, &org, pod_a, "a2").await;

    let page = inboxes::list(
        &pool,
        &org,
        Some(pod_a),
        ListInboxesQuery { limit: 1, direction: SortDirection::Ascending, cursor: None },
    )
    .await
    .unwrap();
    let token = page.next.expect("one row remains on the second page");

    assert_eq!(InboxCursor::decode(&token, Some(pod_b)), Err(PageTokenError::WrongScope));
}

/// `inboxes::list`'s own defense-in-depth check on `query.cursor` — distinct from
/// `InboxCursor::decode`'s identical-looking check, and tested independently of it for the same
/// reason `messages::list_rejects_a_nul_byte_in_a_hand_built_cursor` is: `InboxCursor`'s fields
/// are `pub`, so a hand-built cursor bypasses `decode` entirely and nothing at the type level
/// guarantees `list` only ever receives a decoded one.
#[tokio::test]
async fn inboxes_list_rejects_a_nul_byte_in_a_hand_built_cursor() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _inbox) = support::seed_org_pod_inbox(&pool).await;

    let hostile = InboxCursor {
        created_at: chrono::Utc::now(),
        inbox_id: InboxId::new("abc\0def@x"),
        pod_id: pod,
    };
    let result = inboxes::list(
        &pool,
        &org,
        None,
        ListInboxesQuery { limit: 10, direction: SortDirection::Ascending, cursor: Some(hostile) },
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
}
