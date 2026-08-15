//! Organizations / pods / inboxes: create/get/list/delete, idempotent `client_id` replay, and the
//! two edge cases the dispatch names explicitly:
//! * two simultaneous creates of the same inbox username → exactly one wins, via real database
//!   concurrency, not a check-then-insert race;
//! * a case-variant `inbox_id` resolves to one row, and two case-variant usernames collide.

mod support;

use amk_store::inboxes::{self, NewInbox};
use amk_store::pods::{self, NewPod};
use amk_store::{organizations, StoreError};
use amk_types::ids::{InboxId, PodId};

#[tokio::test]
async fn organization_create_get_list_delete_round_trips() {
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
    assert!(organizations::list(&pool)
        .await
        .unwrap()
        .iter()
        .any(|o| o.organization_id == org));

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

    let all = pods::list(&pool, &org).await.unwrap();
    assert_eq!(all.len(), 1, "exactly one row must exist for the replayed client_id");
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

    let all = inboxes::list(&pool, &org, None).await.unwrap();
    assert_eq!(
        all.iter()
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
    let rows = inboxes::list(&pool, &org, None).await.unwrap();
    let matching = rows.iter().filter(|i| i.inbox_id == normalized).count();
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
        let found = inboxes::get(&pool, &org, &InboxId::new(variant.clone()))
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
    assert!(!inboxes::delete(&pool, &org_b, &inbox_a).await.unwrap());
    assert!(
        inboxes::get(&pool, &org_a, &inbox_a)
            .await
            .unwrap()
            .is_some(),
        "must survive"
    );

    assert!(inboxes::delete(&pool, &org_a, &inbox_a).await.unwrap());
    assert!(inboxes::get(&pool, &org_a, &inbox_a)
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

    let leaked = inboxes::get(&pool, &org_b, &inbox_a).await.unwrap();
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

    let list_a = inboxes::list(&pool, &org_a, None).await.unwrap();
    let ids: Vec<_> = list_a.into_iter().map(|i| i.inbox_id).collect();
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
        inboxes::delete(&pool, &org, &mixed).await.unwrap(),
        "delete must normalize a mixed-case inbox_id parameter to match the stored lowercase row"
    );
    assert!(inboxes::get(&pool, &org, &inbox).await.unwrap().is_none());
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

    let result = inboxes::get(&pool, &org, &hostile).await;
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

    let result = inboxes::delete(&pool, &org, &hostile).await;
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
