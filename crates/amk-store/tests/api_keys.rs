//! `api_keys`: the NULL-vs-`{}` permissions round trip, minting/authentication timing-shape edge
//! cases, the mutual-exclusion `CHECK`, the unique `prefix` index, cross-scope isolation, and the
//! FK behaviour on a pod that still owns keys — the dispatch contract's assigned edge cases.

mod support;

use amk_store::api_keys::{self, AuthenticatedKey, KeyScope, NewApiKey};
use amk_store::inboxes::{self, NewInbox};
use amk_store::{pods, StoreError};
use amk_types::api_key::{ApiKeyPermissions, KeyGrants};
use amk_types::ids::{ApiKeyId, InboxId, OrganizationId};
use sqlx::Row;
use uuid::Uuid;

fn org_key(organization_id: &OrganizationId, name: &str) -> NewApiKey {
    NewApiKey {
        organization_id: organization_id.clone(),
        pod_id: None,
        inbox_id: None,
        name: name.into(),
        permissions: None,
    }
}

// ---- permissions NULL vs '{}' -----------------------------------------------------------------

#[tokio::test]
async fn permissions_null_and_empty_round_trip_to_opposite_grants() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let omitted =
        api_keys::create(&pool, NewApiKey { permissions: None, ..org_key(&org, "omitted") })
            .await
            .unwrap();
    let empty = api_keys::create(
        &pool,
        NewApiKey { permissions: Some(ApiKeyPermissions::default()), ..org_key(&org, "empty") },
    )
    .await
    .unwrap();

    // The create response itself already carries the distinction...
    assert_eq!(omitted.permissions, None);
    assert_eq!(empty.permissions, Some(ApiKeyPermissions::default()));

    // ...and it survives a full round trip back out of storage through `get`.
    let fetched_omitted = api_keys::get(&pool, &org, &KeyScope::Organization, &omitted.api_key_id)
        .await
        .unwrap()
        .expect("just created");
    let fetched_empty = api_keys::get(&pool, &org, &KeyScope::Organization, &empty.api_key_id)
        .await
        .unwrap()
        .expect("just created");
    assert_eq!(fetched_omitted.permissions, None, "an absent object must round-trip as absent");
    assert_eq!(
        fetched_empty.permissions,
        Some(ApiKeyPermissions::default()),
        "a present-but-empty object must round-trip as present, not collapse to absent"
    );

    // And the two verdicts KeyGrants derives from them are opposites, for every one of the 36
    // flags, not merely unequal.
    let omitted_grants = KeyGrants::from_wire(fetched_omitted.permissions);
    let empty_grants = KeyGrants::from_wire(fetched_empty.permissions);
    for name in amk_types::api_key::WIRE_NAMES {
        assert!(omitted_grants.allows(name), "NULL permissions must grant {name}");
        assert!(!empty_grants.allows(name), "'{{}}' permissions must deny {name}");
    }
}

// ---- minting -------------------------------------------------------------------------------

#[tokio::test]
async fn the_minted_secret_is_not_recoverable_from_the_stored_row() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    let created = api_keys::create(&pool, org_key(&org, "probe"))
        .await
        .unwrap();
    assert!(created.api_key.starts_with("am_us_"));
    assert_eq!(created.api_key.len(), "am_us_".len() + 32);
    // The secret is a true extension of the prefix (the prefix is a real leading segment)...
    assert!(created.api_key.starts_with(&created.prefix));
    // ...but the prefix is strictly shorter: it does not itself disclose the whole secret.
    assert!(created.prefix.len() < created.api_key.len());

    // The row in the database — queried directly, bypassing every repository function's own
    // shape — must contain the hash and the prefix, and nothing that lets the plaintext secret be
    // recovered from either.
    let row = sqlx::query("SELECT hash, prefix FROM api_keys WHERE api_key_id = $1")
        .bind(uuid::Uuid::parse_str(created.api_key_id.as_str()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    let hash: String = row.try_get("hash").unwrap();
    let prefix: String = row.try_get("prefix").unwrap();
    assert_eq!(prefix, created.prefix);
    assert!(
        !hash.contains(&created.api_key),
        "the hash column must never contain the secret"
    );
    // Nothing beyond the disclosed prefix itself is recoverable: the nine characters that follow
    // the prefix (which the caller never sees stored anywhere) must not appear verbatim in the
    // hash's own encoded form (a PHC string is ASCII, so a literal substring match is meaningful).
    let undisclosed = &created.api_key[created.prefix.len()..created.prefix.len() + 9];
    assert!(
        !hash.contains(undisclosed),
        "no undisclosed portion of the secret leaks into the hash"
    );
}

#[tokio::test]
async fn two_mints_never_share_a_prefix_the_unique_index_fires() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;

    // Two real, independently minted rows practically never collide — the unique index is proven
    // here directly instead, by forcing a literal collision under it with a raw insert, so the
    // assertion does not depend on astronomically unlikely chance.
    let first = api_keys::create(&pool, org_key(&org, "first"))
        .await
        .unwrap();

    let forced = sqlx::query(
        "INSERT INTO api_keys (api_key_id, organization_id, name, prefix, hash) \
         VALUES ($1, $2, 'forced-collision', $3, 'irrelevant-hash')",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(org.as_str())
    .bind(&first.prefix)
    .execute(&pool)
    .await;

    let err = forced.expect_err("a duplicate prefix must be rejected by the unique index");
    let sqlx::Error::Database(db_err) = err else {
        panic!("expected a database error, got {err:?}");
    };
    assert!(db_err.is_unique_violation(), "must fail as a unique violation specifically");
    assert_eq!(db_err.constraint(), Some("api_keys_prefix_idx"));
}

// ---- authenticate ----------------------------------------------------------------------------

#[tokio::test]
async fn authenticate_with_the_right_secret_resolves_the_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "auth-ok"))
        .await
        .unwrap();

    let resolved = api_keys::authenticate(&pool, &created.api_key)
        .await
        .unwrap()
        .expect("the freshly minted secret must authenticate");
    assert_eq!(resolved.api_key_id, created.api_key_id);
    assert_eq!(resolved.organization_id, org);
    assert_eq!(resolved.pod_id, None);
    assert_eq!(resolved.inbox_id, None);
}

/// `AUTHENTICATE_SQL`'s `WHERE prefix = $1` is this dispatch's entire reason for existing — the
/// O(1) lookup path — but every other `authenticate` test above creates exactly one key, so any
/// query that returns *a* row (an `ORDER BY ... LIMIT 1` with no `WHERE` at all, say) passes them
/// by accident. Three keys, across the three different scopes, each asserted to resolve to
/// *itself specifically* — not merely `Some(_)` — so a mutation that drops the `WHERE` clause
/// cannot survive on luck of insertion order: presenting A's secret must resolve A even though B
/// and C both exist, and likewise for B and C.
#[tokio::test]
async fn authenticate_resolves_the_specific_key_presented_not_merely_any_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let key_a = api_keys::create(&pool, org_key(&org, "selectivity-a"))
        .await
        .unwrap();
    let key_b = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod),
            inbox_id: None,
            name: "selectivity-b".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    let key_c = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox.clone()),
            name: "selectivity-c".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let resolved_a = api_keys::authenticate(&pool, &key_a.api_key)
        .await
        .unwrap()
        .expect("key A's own secret must authenticate");
    assert_eq!(resolved_a.api_key_id, key_a.api_key_id, "A must resolve to A, not B or C");

    let resolved_b = api_keys::authenticate(&pool, &key_b.api_key)
        .await
        .unwrap()
        .expect("key B's own secret must authenticate");
    assert_eq!(resolved_b.api_key_id, key_b.api_key_id, "B must resolve to B, not A or C");

    let resolved_c = api_keys::authenticate(&pool, &key_c.api_key)
        .await
        .unwrap()
        .expect("key C's own secret must authenticate");
    assert_eq!(resolved_c.api_key_id, key_c.api_key_id, "C must resolve to C, not A or B");
}

#[tokio::test]
async fn authenticate_rejects_every_kind_of_miss() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "auth-miss"))
        .await
        .unwrap();

    // A known prefix with the wrong secret.
    let wrong_secret = format!("{}wrong-secret-suffix-000000", created.prefix);
    assert!(api_keys::authenticate(&pool, &wrong_secret)
        .await
        .unwrap()
        .is_none());

    // An unknown prefix entirely (well-formed shape, no row behind it).
    assert!(api_keys::authenticate(&pool, "am_us_00000000000000000000000000000000")
        .await
        .unwrap()
        .is_none());

    // No prefix separator at all.
    assert!(api_keys::authenticate(&pool, "not-a-key-at-all")
        .await
        .unwrap()
        .is_none());

    // Empty string.
    assert!(api_keys::authenticate(&pool, "").await.unwrap().is_none());

    // A value whose prefix matches a real row but which is far longer than any key ever minted —
    // must still fail the secret comparison, not panic and not succeed on a truncated match.
    let too_long = format!("{}{}", created.api_key, "-and-then-some-trailing-garbage");
    assert!(api_keys::authenticate(&pool, &too_long)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn authenticate_never_writes_used_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "no-write"))
        .await
        .unwrap();

    api_keys::authenticate(&pool, &created.api_key)
        .await
        .unwrap();
    let after = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.used_at, None, "authenticate must not touch used_at");

    assert!(api_keys::touch_used_at(&pool, &created.api_key_id)
        .await
        .unwrap());
    let touched = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert!(touched.used_at.is_some(), "touch_used_at is the only call that sets it");
}

/// `touch_used_at`'s `WHERE` clause names one row; nothing above proved it touches only that row.
#[tokio::test]
async fn touch_used_at_updates_only_the_named_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key_a = api_keys::create(&pool, org_key(&org, "touch-a"))
        .await
        .unwrap();
    let key_b = api_keys::create(&pool, org_key(&org, "touch-b"))
        .await
        .unwrap();

    assert!(api_keys::touch_used_at(&pool, &key_a.api_key_id)
        .await
        .unwrap());

    let touched_a = api_keys::get(&pool, &org, &KeyScope::Organization, &key_a.api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert!(touched_a.used_at.is_some(), "the named key must be touched");

    let untouched_b = api_keys::get(&pool, &org, &KeyScope::Organization, &key_b.api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched_b.used_at, None, "a sibling key must not be touched");
}

// ---- inbox-scoped key + case folding (fixture 18) -----------------------------------------

#[tokio::test]
async fn an_inbox_scoped_key_created_with_mixed_case_authenticates_and_resolves_the_folded_inbox() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, _) = support::seed_org_pod_inbox(&pool).await;
    let mixed_case = InboxId::new(format!("AMKCase-{}@Example.Test", support::unique_suffix()));
    let created_inbox = inboxes::create(
        &pool,
        NewInbox {
            inbox_id: mixed_case.clone(),
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .unwrap();
    let inbox = created_inbox.inbox_id;

    let created = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(mixed_case.clone()),
            name: "inbox-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    // Stored and returned in normalized (lowercased) form, exactly like every other inbox_id.
    assert_eq!(created.inbox_id.as_ref().unwrap(), &inbox, "must resolve to the normalized id");
    assert_ne!(created.inbox_id.as_ref().unwrap().as_str(), mixed_case.as_str());

    let resolved: AuthenticatedKey = api_keys::authenticate(&pool, &created.api_key)
        .await
        .unwrap()
        .expect("must authenticate");
    assert_eq!(resolved.inbox_id.as_ref().unwrap(), &inbox);

    // Listing/getting at the inbox mount resolves regardless of which casing is presented.
    let via_original_case =
        api_keys::get(&pool, &org, &KeyScope::Inbox(mixed_case.clone()), &created.api_key_id)
            .await
            .unwrap();
    let via_normalized =
        api_keys::get(&pool, &org, &KeyScope::Inbox(inbox.clone()), &created.api_key_id)
            .await
            .unwrap();
    assert!(
        via_original_case.is_some(),
        "the caller's own casing must still resolve the key"
    );
    assert!(via_normalized.is_some());
}

// ---- the CHECK: pod_id and inbox_id are mutually exclusive --------------------------------

#[tokio::test]
async fn a_row_naming_both_pod_id_and_inbox_id_is_rejected_by_the_database() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let attempt = sqlx::query(
        "INSERT INTO api_keys (api_key_id, organization_id, pod_id, inbox_id, name, prefix, hash) \
         VALUES ($1, $2, $3, $4, 'invalid', 'am_us_invalidxx', 'irrelevant-hash')",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(org.as_str())
    .bind(pod.0)
    .bind(inbox.normalized().as_str())
    .execute(&pool)
    .await;

    let err = attempt.expect_err("a row naming both pod_id and inbox_id must be rejected");
    let sqlx::Error::Database(db_err) = err else {
        panic!("expected a database error, got {err:?}");
    };
    assert!(db_err.is_check_violation(), "must fail the CHECK, not some other constraint");
    assert_eq!(db_err.constraint(), Some("api_keys_scope_not_both_pod_and_inbox"));
}

// ---- pod deletion with keys attached --------------------------------------------------------

#[tokio::test]
async fn deleting_a_pod_that_owns_keys_is_rejected_by_the_declared_fk_behaviour() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;

    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod),
            inbox_id: None,
            name: "pod-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let result = pods::delete(&pool, &org, pod).await;
    // The declared FK behaviour is the default (no ON DELETE clause, same as every other table in
    // this crate) — NO ACTION, so the delete itself fails outright rather than orphaning or
    // cascading through the key.
    assert!(
        matches!(result, Err(StoreError::Database(_))),
        "deleting a pod with keys attached must fail via the FK, not silently orphan them: \
         {result:?}"
    );
    assert!(
        pods::get(&pool, &org, pod).await.unwrap().is_some(),
        "the pod must survive the rejected delete"
    );
}

// ---- cross-scope listing -------------------------------------------------------------------

#[tokio::test]
async fn listing_at_one_pod_never_returns_a_sibling_pods_keys() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod_a),
            inbox_id: None,
            name: "pod-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod_b),
            inbox_id: None,
            name: "pod-b-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let listed_a = api_keys::list(&pool, &org, &KeyScope::Pod(pod_a))
        .await
        .unwrap();
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].api_key_id, key_a.api_key_id);

    let listed_b = api_keys::list(&pool, &org, &KeyScope::Pod(pod_b))
        .await
        .unwrap();
    assert_eq!(listed_b.len(), 1);
    assert!(
        listed_b.iter().all(|k| k.api_key_id != key_a.api_key_id),
        "pod_b's listing must not contain pod_a's key"
    );
}

/// `list`'s inbox pin, the sibling of [`listing_at_one_pod_never_returns_a_sibling_pods_keys`] for
/// the inbox mount — `LIST_SQL`'s `inbox_id = $3` fragment has no other test exercising it.
#[tokio::test]
async fn listing_at_one_inbox_never_returns_a_sibling_inboxs_keys() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "sibling").await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_b.clone()),
            name: "inbox-b-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let listed_a = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_a))
        .await
        .unwrap();
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].api_key_id, key_a.api_key_id);

    let listed_b = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_b))
        .await
        .unwrap();
    assert_eq!(listed_b.len(), 1);
    assert!(
        listed_b.iter().all(|k| k.api_key_id != key_a.api_key_id),
        "inbox_b's listing must not contain inbox_a's key"
    );
}

/// `list`'s pod pin isolates a sibling pod; `get`/`delete` share the same `WHERE` fragment, but
/// nothing above exercised it through them directly — a mutation of the `pod_id = $3` predicate
/// in `GET_SQL`/`DELETE_SQL` specifically (as opposed to `LIST_SQL`) would otherwise survive.
#[tokio::test]
async fn get_and_delete_also_pin_the_pod_scope_not_only_list() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod_a),
            inbox_id: None,
            name: "pod-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    // A sibling pod must not be able to read or delete it...
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Pod(pod_b), &key_a.api_key_id)
            .await
            .unwrap()
            .is_none(),
        "pod_b must not read pod_a's key by naming its id directly"
    );
    assert!(
        !api_keys::delete(&pool, &org, &KeyScope::Pod(pod_b), &key_a.api_key_id)
            .await
            .unwrap(),
        "pod_b must not delete pod_a's key by naming its id directly"
    );
    // ...while the owning pod can do both.
    assert!(api_keys::get(&pool, &org, &KeyScope::Pod(pod_a), &key_a.api_key_id)
        .await
        .unwrap()
        .is_some());
    assert!(api_keys::delete(&pool, &org, &KeyScope::Pod(pod_a), &key_a.api_key_id)
        .await
        .unwrap());
}

/// Sibling of the case-folding test, but for a *different* inbox rather than a differently-cased
/// rendering of the *same* one: `get`'s `inbox_id = $4` predicate must reject a sibling inbox in
/// the same pod, not merely accept the right one.
#[tokio::test]
async fn get_pins_the_named_inbox_not_just_the_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "sibling").await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    assert!(
        api_keys::get(&pool, &org, &KeyScope::Inbox(inbox_b), &key_a.api_key_id)
            .await
            .unwrap()
            .is_none(),
        "a sibling inbox in the same pod must not read this key by naming its id directly"
    );
    assert!(api_keys::get(&pool, &org, &KeyScope::Inbox(inbox_a), &key_a.api_key_id)
        .await
        .unwrap()
        .is_some());
}

/// The inbox-mount sibling of [`get_and_delete_also_pin_the_pod_scope_not_only_list`]:
/// `KeyScope::Inbox` had appeared with `get` and `list`, never with `delete`, so a mutation of
/// `DELETE_SQL`'s `inbox_id = $4` predicate specifically had nothing exercising it.
#[tokio::test]
async fn delete_pins_the_named_inbox_not_just_the_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "sibling").await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    let key_b = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_b.clone()),
            name: "inbox-b-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    // Inbox B's scope must not be able to delete inbox A's key, and vice versa.
    assert!(
        !api_keys::delete(&pool, &org, &KeyScope::Inbox(inbox_b.clone()), &key_a.api_key_id)
            .await
            .unwrap(),
        "inbox_b must not delete inbox_a's key by naming its id directly"
    );
    assert!(
        !api_keys::delete(&pool, &org, &KeyScope::Inbox(inbox_a.clone()), &key_b.api_key_id)
            .await
            .unwrap(),
        "inbox_a must not delete inbox_b's key by naming its id directly"
    );
    // Both keys must have survived the cross-scope attempts...
    assert!(api_keys::get(&pool, &org, &KeyScope::Inbox(inbox_a.clone()), &key_a.api_key_id)
        .await
        .unwrap()
        .is_some());
    assert!(api_keys::get(&pool, &org, &KeyScope::Inbox(inbox_b.clone()), &key_b.api_key_id)
        .await
        .unwrap()
        .is_some());
    // ...while each inbox can delete its own.
    assert!(api_keys::delete(&pool, &org, &KeyScope::Inbox(inbox_a), &key_a.api_key_id)
        .await
        .unwrap());
    assert!(api_keys::delete(&pool, &org, &KeyScope::Inbox(inbox_b), &key_b.api_key_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn listing_at_org_scope_never_leaks_a_sibling_organizations_keys() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org_a = support::seed_org(&pool).await;
    let org_b = support::seed_org(&pool).await;

    let key_a = api_keys::create(&pool, org_key(&org_a, "org-a-key"))
        .await
        .unwrap();
    api_keys::create(&pool, org_key(&org_b, "org-b-key"))
        .await
        .unwrap();

    let listed_a = api_keys::list(&pool, &org_a, &KeyScope::Organization)
        .await
        .unwrap();
    assert!(listed_a.iter().any(|k| k.api_key_id == key_a.api_key_id));
    assert_eq!(listed_a.len(), 1, "org_a's listing must contain only its own key");

    // Directly reading org_a's key by id, but scoped under org_b, must miss.
    let leaked = api_keys::get(&pool, &org_b, &KeyScope::Organization, &key_a.api_key_id)
        .await
        .unwrap();
    assert!(leaked.is_none(), "org_b must not read org_a's key by naming its id directly");

    // And a delete attempt under the wrong org must not affect the row.
    assert!(!api_keys::delete(&pool, &org_b, &KeyScope::Organization, &key_a.api_key_id)
        .await
        .unwrap());
    assert!(api_keys::get(&pool, &org_a, &KeyScope::Organization, &key_a.api_key_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn org_mount_listing_returns_every_scope_level_within_the_organization() {
    // The org mount applies no pod/inbox filter, so it must return keys at every scope level —
    // organization-, pod- and inbox-scoped alike — as long as they belong to this organization.
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;

    let org_scoped = api_keys::create(&pool, org_key(&org, "org-scoped"))
        .await
        .unwrap();
    let pod_scoped = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: Some(pod),
            inbox_id: None,
            name: "pod-scoped".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    let inbox_scoped = api_keys::create(
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

    let listed = api_keys::list(&pool, &org, &KeyScope::Organization)
        .await
        .unwrap();
    let ids: Vec<ApiKeyId> = listed.into_iter().map(|k| k.api_key_id).collect();
    assert!(ids.contains(&org_scoped.api_key_id));
    assert!(ids.contains(&pod_scoped.api_key_id));
    assert!(ids.contains(&inbox_scoped.api_key_id));
}

// ---- get()/delete()/touch_used_at() with a syntactically invalid id -----------------------

#[tokio::test]
async fn a_non_uuid_api_key_id_is_treated_as_not_found_not_an_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // A real row exists in the same organization throughout, so a bogus id has something to
    // (wrongly) resolve to if the guard against it is broken.
    let real = api_keys::create(&pool, org_key(&org, "real-key-in-the-same-org"))
        .await
        .unwrap();
    let bogus = ApiKeyId::new("not-a-uuid-at-all");

    assert!(api_keys::get(&pool, &org, &KeyScope::Organization, &bogus)
        .await
        .unwrap()
        .is_none());
    assert!(!api_keys::delete(&pool, &org, &KeyScope::Organization, &bogus)
        .await
        .unwrap());
    assert!(!api_keys::touch_used_at(&pool, &bogus).await.unwrap());

    // And the real key is untouched by any of the three attempts above.
    let still_there = api_keys::get(&pool, &org, &KeyScope::Organization, &real.api_key_id)
        .await
        .unwrap();
    assert!(still_there.is_some(), "the real key must survive every bogus-id attempt");
    assert_eq!(still_there.unwrap().used_at, None, "touch_used_at must not have reached it");
}

/// A hostile id (an embedded NUL byte) reproduces a defect a review lens found live against this
/// crate's *previous* fix: comparing the presented id as `text` in SQL (`api_key_id::text =
/// lower($n)`) closed the string-equality gap but not the encoding one — Postgres `text` cannot
/// carry a NUL byte, so binding one as a parameter fails with `22021 invalid byte sequence`
/// *before* any comparison runs, surfacing as `StoreError::Database` (a 500-class error) rather
/// than the uniform "not found" every other malformed id gets — a caller-observable
/// distinguisher, which is a denial-masking defect. `ApiKeyId::from_path_segment`
/// (`crates/amk-types/src/ids.rs`) only rejects invalid UTF-8, and `%00` percent-decodes to
/// perfectly valid UTF-8, so this is reachable from the wire, not merely a theoretical string.
///
/// The fix parses in Rust (`Uuid::parse_str(..).ok()`) so a NUL byte never reaches a query
/// parameter as text at all, and binds the *value* — `None` becomes SQL `NULL`, which matches
/// zero rows and never errors. Asserting `Ok(..)` rather than merely `is_none()`/`!...` is the
/// whole point of this test: an `Err` here would be exactly the regression this closes, even if
/// its payload happened to look like "not found" some other way.
#[tokio::test]
async fn an_api_key_id_with_an_embedded_nul_byte_returns_ok_not_a_database_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let hostile = ApiKeyId::new("abc\0def");

    let got = api_keys::get(&pool, &org, &KeyScope::Organization, &hostile).await;
    assert!(got.is_ok(), "must be Ok(None), not a database error: {got:?}");
    assert_eq!(got.unwrap(), None);

    let deleted = api_keys::delete(&pool, &org, &KeyScope::Organization, &hostile).await;
    assert!(deleted.is_ok(), "must be Ok(false), not a database error: {deleted:?}");
    assert!(!deleted.unwrap());

    let touched = api_keys::touch_used_at(&pool, &hostile).await;
    assert!(touched.is_ok(), "must be Ok(false), not a database error: {touched:?}");
    assert!(!touched.unwrap());
}

/// The regression that proved the *first* fix (a Rust-side `let Some(id) = parse(..) else {
/// return Ok(None) }`) was not an equivalent mutant: rewriting that early return into
/// `.unwrap_or_else(Uuid::nil)` made a non-UUID id silently resolve any row seeded at the nil
/// UUID sentinel. This seeds exactly that row directly via SQL (bypassing `create`, which always
/// mints a real v4 UUID and could never produce it), then asserts a non-UUID id still does not
/// resolve to it — the case a mutant reintroducing that fallback would fail.
#[tokio::test]
async fn a_non_uuid_id_never_resolves_the_seeded_nil_uuid_sentinel() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // The nil UUID is a fixed sentinel by design (that is the whole point of the test), so —
    // unlike every other seed helper in this suite, which mints a fresh random id specifically to
    // avoid colliding with another run — this row collides with itself across repeated runs
    // against the shared dev database (`tests/support/mod.rs`'s own documented lack of per-test
    // schema isolation). `ON CONFLICT` re-points the existing row at *this* run's fresh org
    // (and clears any scope an earlier run's row happened to carry) instead of erroring.
    sqlx::query(
        "INSERT INTO api_keys (api_key_id, organization_id, name, prefix, hash) \
         VALUES ($1, $2, 'nil-sentinel', 'am_us_nilsentnl', 'irrelevant-hash') \
         ON CONFLICT (api_key_id) DO UPDATE SET \
            organization_id = EXCLUDED.organization_id, pod_id = NULL, inbox_id = NULL",
    )
    .bind(Uuid::nil())
    .bind(org.as_str())
    .execute(&pool)
    .await
    .unwrap();

    let bogus = ApiKeyId::new("not-a-uuid-at-all");
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Organization, &bogus)
            .await
            .unwrap()
            .is_none(),
        "a non-UUID id must not resolve the seeded nil-UUID row"
    );
    assert!(
        !api_keys::delete(&pool, &org, &KeyScope::Organization, &bogus)
            .await
            .unwrap(),
        "a non-UUID id must not delete the seeded nil-UUID row"
    );

    // The nil-UUID row itself remains reachable by its own (real, if unusual) id — proving the
    // miss above is about the bogus id, not about the nil row being unreachable altogether.
    let nil_id = ApiKeyId::new(Uuid::nil().to_string());
    assert!(api_keys::get(&pool, &org, &KeyScope::Organization, &nil_id)
        .await
        .unwrap()
        .is_some());
}

/// `Uuid::parse_str` accepts several renderings of one value — hyphenated (any case), simple-32
/// (no hyphens), braced, and the `urn:uuid:` form — but [`ApiKeyId`] is `string_id!`
/// (`amk_types::ids`), not `uuid_id!`: opaque and byte-exact, deliberately unlike `PodId`/
/// `ThreadId`. Nothing in any fixture says AgentMail accepts an alternate rendering of an id it
/// issued, so binding the parsed value alone (accepting all five as equivalent) would be an
/// invented, wider equality rule than this crate has evidence for — the exact defect a prior
/// review round found and rejected `lower()` case-folding for. Only the id's own canonical
/// (lowercase-hyphenated) rendering — the one [`create`] actually returns — may resolve it; every
/// other rendering of the very same UUID value must resolve `None`, same as a different UUID or a
/// NUL-bearing string does.
///
/// `exact_api_key_uuid` is one function shared by `get`, `delete` and `touch_used_at`, but a
/// mutation of any *one* call site's `.filter(..)` is invisible to a test that only exercises a
/// different call site — an uppercase rendering silently deleting another key's row, or setting
/// its `used_at`, would have passed a `get`-only version of this test. Every rendering below is
/// therefore driven through all three: `get` must miss it, `delete` must not remove the row (and
/// the row must still be there afterward), and `touch_used_at` must not set `used_at` (and it
/// must still be absent afterward). Only at the very end does the canonical rendering get used to
/// actually touch and then delete the row, proving the two negatives above were not simply "this
/// function always returns false/None regardless of input".
#[tokio::test]
async fn only_the_canonical_rendering_of_an_api_key_id_resolves_it_everywhere() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "rendering-check"))
        .await
        .unwrap();
    let real = Uuid::parse_str(created.api_key_id.as_str()).expect("create always mints a UUID");

    // The canonical rendering — the id as create() actually returned it — resolves via get().
    let canonical = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
        .await
        .unwrap();
    assert_eq!(canonical.map(|k| k.api_key_id), Some(created.api_key_id.clone()));

    // Every other rendering of the SAME UUID value, plus a genuinely different UUID, must be a
    // no-op through all three functions.
    let different = Uuid::new_v4().to_string();
    let non_resolving_renderings = [
        real.hyphenated().to_string().to_uppercase(),
        real.simple().to_string(),
        format!("{{{real}}}"),
        format!("urn:uuid:{real}"),
        different.clone(),
    ];
    for rendering in non_resolving_renderings {
        let id = ApiKeyId::new(rendering.clone());

        let resolved = api_keys::get(&pool, &org, &KeyScope::Organization, &id)
            .await
            .unwrap();
        assert!(resolved.is_none(), "get: rendering {rendering:?} must not resolve the key");

        assert!(
            !api_keys::delete(&pool, &org, &KeyScope::Organization, &id)
                .await
                .unwrap(),
            "delete: rendering {rendering:?} must not report success"
        );
        let survived = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
            .await
            .unwrap();
        assert!(
            survived.is_some(),
            "delete: rendering {rendering:?} must not actually remove the row"
        );

        assert!(
            !api_keys::touch_used_at(&pool, &id).await.unwrap(),
            "touch_used_at: rendering {rendering:?} must not report success"
        );
        let untouched = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            untouched.used_at, None,
            "touch_used_at: rendering {rendering:?} must not actually set used_at"
        );
    }

    // The canonical rendering, and only the canonical rendering, actually reaches the row: touch
    // it, observe the effect, then delete it, observe that too. This is what proves the negatives
    // above were real refusals and not a function that never does anything.
    assert!(api_keys::touch_used_at(&pool, &created.api_key_id)
        .await
        .unwrap());
    let touched = api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        touched.used_at.is_some(),
        "the canonical rendering must be able to touch the row"
    );

    assert!(api_keys::delete(&pool, &org, &KeyScope::Organization, &created.api_key_id)
        .await
        .unwrap());
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
            .await
            .unwrap()
            .is_none(),
        "the canonical rendering must be able to delete the row"
    );
}

/// Table-driven regression for the "total, never `Err`, never panics" property `get`/`delete`/
/// `touch_used_at` are supposed to have for *any* caller-supplied `api_key_id`, not merely the two
/// specific cases (`an_api_key_id_with_an_embedded_nul_byte_returns_ok_not_a_database_error`,
/// `a_non_uuid_id_never_resolves_the_seeded_nil_uuid_sentinel`) checked in separately. A review
/// lens ran this exact battery by hand — empty string, bare NUL, embedded NUL, a ~1 MB string,
/// CR/LF, raw control bytes, a whitespace-padded rendering of a real canonical UUID, emoji, and a
/// SQL-fragment string — and every one returned `Ok`; until this test existed, that evidence lived
/// only in a review transcript, not in the tree. The specific future regression this guards
/// against: a length-bounded or charset fast-path added ahead of `Uuid::parse_str` (for
/// performance, say) that panics or errors on one of these instead of falling through to `None`.
#[tokio::test]
async fn hostile_api_key_id_inputs_are_total_across_get_delete_and_touch_used_at() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let real = api_keys::create(&pool, org_key(&org, "hostile-battery"))
        .await
        .unwrap();

    let control_bytes: String = (0u8..32).map(char::from).collect();
    let hostile_inputs: Vec<String> = vec![
        String::new(),                               // empty string
        "\0".to_owned(),                             // bare NUL
        "abc\0def".to_owned(),                       // embedded NUL
        "a".repeat(1024 * 1024),                     // ~1 MB string
        "abc\r\ndef".to_owned(),                     // CR/LF
        control_bytes,                               // raw control bytes (incl. NUL)
        format!("  {}  ", real.api_key_id.as_str()), // whitespace-padded canonical UUID
        "🙂🙂🙂".to_owned(),                         // multi-byte / emoji
        "'; DROP TABLE api_keys; --".to_owned(),     // SQL-fragment string
    ];

    for input in hostile_inputs {
        let label = if input.len() > 64 {
            format!("<{} bytes>", input.len())
        } else {
            format!("{input:?}")
        };
        let id = ApiKeyId::new(input);

        let got = api_keys::get(&pool, &org, &KeyScope::Organization, &id).await;
        assert!(got.is_ok(), "get must return Ok for hostile input {label}: {got:?}");

        let deleted = api_keys::delete(&pool, &org, &KeyScope::Organization, &id).await;
        assert!(deleted.is_ok(), "delete must return Ok for hostile input {label}: {deleted:?}");

        let touched = api_keys::touch_used_at(&pool, &id).await;
        assert!(
            touched.is_ok(),
            "touch_used_at must return Ok for hostile input {label}: {touched:?}"
        );
    }

    // The real row must have survived the entire battery, untouched.
    let survivor = api_keys::get(&pool, &org, &KeyScope::Organization, &real.api_key_id)
        .await
        .unwrap()
        .expect("the real key must survive the entire hostile battery");
    assert_eq!(survivor.used_at, None, "no hostile input should have touched the real key");
}

// ---- a NUL byte in `inbox_id` itself (not `api_key_id`) ------------------------------------
//
// Everything above drives a hostile *`api_key_id`* through `exact_api_key_uuid`'s existing
// `Option<Uuid>` guard. `inbox_id` is a different, ordinary `InboxId` bound as `text` into
// `INSERT_SQL`/`GET_SQL`/`LIST_SQL`/`DELETE_SQL`, and until this dispatch's guard, nothing in this
// module checked it: a review panel's independent enumeration found `api_keys.rs` had zero calls
// to `has_forbidden_byte` at all.

/// `create`'s own `NewApiKey.inbox_id` reaches `INSERT_SQL` as `text` — a NUL fails the bind, not
/// the query, so it must be rejected in Rust before the mint even happens, exactly like
/// `inboxes::create`'s own `inbox_id`.
#[tokio::test]
async fn create_rejects_a_nul_byte_in_inbox_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let hostile = InboxId::new("abc\0def@example.test");

    let result = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org,
            pod_id: None,
            inbox_id: Some(hostile),
            name: "hostile-inbox".into(),
            permissions: None,
        },
    )
    .await;
    assert!(
        matches!(result, Err(StoreError::InvalidValue("inbox_id"))),
        "expected InvalidValue(\"inbox_id\"), got {result:?}"
    );
}

/// `get`'s `KeyScope::Inbox` reaches `scope_params` and then `GET_SQL`'s `inbox_id = $4` as
/// `text`. Asserting `Ok(None)` (not merely `!= Err`) is the point: the pre-fix behaviour was
/// `Err(StoreError::Database(_))` from SQLSTATE `22021`, and a mutant that reintroduced that would
/// still technically "return a Result".
#[tokio::test]
async fn get_rejects_a_nul_byte_in_the_inbox_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let created = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox),
            name: "real-inbox-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    let _ = pod;

    let hostile_scope = KeyScope::Inbox(InboxId::new("abc\0def@example.test"));
    let got = api_keys::get(&pool, &org, &hostile_scope, &created.api_key_id).await;
    assert!(got.is_ok(), "must be Ok(None), not a database error: {got:?}");
    assert_eq!(got.unwrap(), None);
}

/// `delete`'s sibling of the `get` test above — same `inbox_id = $4` fragment in `DELETE_SQL`, a
/// separate call site the dispatch contract requires its own test for (a prior dispatch shipped a
/// regression test that covered `get` while `delete`'s call site went unmutated).
#[tokio::test]
async fn delete_rejects_a_nul_byte_in_the_inbox_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, _pod, inbox) = support::seed_org_pod_inbox(&pool).await;
    let created = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox),
            name: "real-inbox-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let hostile_scope = KeyScope::Inbox(InboxId::new("abc\0def@example.test"));
    let deleted = api_keys::delete(&pool, &org, &hostile_scope, &created.api_key_id).await;
    assert!(deleted.is_ok(), "must be Ok(false), not a database error: {deleted:?}");
    assert!(!deleted.unwrap());

    // The real key must have survived — this is not merely "delete always reports false".
    assert!(
        api_keys::get(&pool, &org, &KeyScope::Organization, &created.api_key_id)
            .await
            .unwrap()
            .is_some(),
        "a hostile scope must not have deleted the real row"
    );
}

/// The test this dispatch exists to make impossible to fake: `scope_params` turning a hostile
/// `KeyScope::Inbox` into `(None, None)` would satisfy every test above (each asserts an *empty*
/// or *false* result, and `(None, None)` unpins the query rather than narrowing it) while actually
/// widening an inbox-scoped `list` into an organization-wide one — a cross-tenant leak dressed up
/// as a not-found. Two *different* inboxes each get a key; listing at a hostile rendering of
/// inbox A's scope must return neither key, not inbox B's (which a widened, unpinned query would
/// include). If the guard is ever moved from its per-function early return into `scope_params`
/// itself, this is the test that fails — the others would still pass.
#[tokio::test]
async fn list_at_a_hostile_inbox_scope_returns_empty_not_every_key_in_the_organization() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "sibling").await;

    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_b),
            name: "inbox-b-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    // A NUL-bearing rendering of inbox A's own id — not a different inbox, so a correct guard's
    // only possible non-widening outcome is "match nothing", never "match inbox A's key" and never
    // "match every key in the organization".
    let hostile = InboxId::new(format!("{}\0", inbox_a.as_str()));
    let listed = api_keys::list(&pool, &org, &KeyScope::Inbox(hostile))
        .await
        .unwrap();
    assert!(
        listed.is_empty(),
        "a hostile inbox scope must return no keys at all, not the whole organization's: \
         {listed:?}"
    );
}

/// Positive-path sibling of the test above, proving the guard rejects exactly the hostile byte and
/// nothing more: a *clean* inbox-scoped `list` must still return precisely that inbox's own key,
/// not its sibling's — ruling out an over-broad guard (one that rejected every inbox scope, clean
/// or not) as a way to pass the negative test.
#[tokio::test]
async fn list_at_a_clean_inbox_scope_still_returns_exactly_that_inboxs_key() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let (org, pod, inbox_a) = support::seed_org_pod_inbox(&pool).await;
    let inbox_b = support::seed_inbox(&pool, &org, pod, "sibling").await;

    let key_a = api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_a.clone()),
            name: "inbox-a-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();
    api_keys::create(
        &pool,
        NewApiKey {
            organization_id: org.clone(),
            pod_id: None,
            inbox_id: Some(inbox_b),
            name: "inbox-b-key".into(),
            permissions: None,
        },
    )
    .await
    .unwrap();

    let listed = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_a))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "a clean inbox scope must still resolve exactly one key");
    assert_eq!(listed[0].api_key_id, key_a.api_key_id);
}

// ---- a NUL byte in `authenticate`'s presented credential ------------------------------------
//
// A different door from every test above: `candidate_prefix` slices the caller's raw credential
// (not an id newtype at all) and `authenticate` binds the result as `text` into `AUTHENTICATE_SQL`.
// The module's own doc comment already requires a NUL-bearing presented value to be treated as
// "a malformed presented value" — one of the three kinds of miss `authenticate` promises to cost
// the same as every other kind.

/// `authenticate` must resolve `Ok(None)` for a NUL-bearing credential, not `Err` — asserted as
/// `Ok(None)` specifically (not `is_err()`), because the regression this guards against is a
/// database error escaping as `StoreError::Database`, not merely "some Result variant".
#[tokio::test]
async fn authenticate_with_a_nul_byte_in_the_presented_value_returns_ok_none() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let hostile = "am_us_ab\0cdefg-the-rest-of-a-realistic-length-credential-tail";

    let result = api_keys::authenticate(&pool, hostile).await;
    assert!(result.is_ok(), "must be Ok(None), not a database error: {result:?}");
    assert_eq!(result.unwrap(), None);
}

/// The obvious fix for the test above — an early `return Ok(None)` the moment the presented value
/// is found to carry a NUL — is wrong: it would skip the module's own documented "exactly one
/// `verify_secret` call, unconditional on every path" invariant, so a NUL-bearing credential would
/// resolve measurably faster than a real miss (right prefix, wrong secret) — reopening, at the
/// query-parameter auth precedence path, precisely the timing side channel the module's five prior
/// review rounds closed for every other kind of miss. Argon2id's default parameters are a
/// deliberately expensive, memory-hard hash (tens of milliseconds is the normal cost on ordinary
/// hardware), so a genuine verify and a skipped one differ by orders of magnitude — the floor below
/// is generous enough to catch a skip without being sensitive to ordinary scheduling jitter.
#[tokio::test]
async fn authenticate_with_a_nul_byte_still_pays_the_real_verify_cost() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "nul-timing-baseline"))
        .await
        .unwrap();

    // A NUL inside the same visible-prefix window a real credential's prefix occupies, derived
    // from an actually-minted prefix rather than from this module's own `[ASSUMED]` constants
    // (`PREFIX_TAG`/`VISIBLE_LEN` are private, and this test has no business hardcoding either):
    // replacing the prefix's own last character is guaranteed to land inside whatever slice
    // `candidate_prefix` re-derives, so `candidate_prefix` still returns `Some(_)` and the only
    // thing left to distinguish a real miss from a skipped one is whether `verify_secret` ran.
    let mut hostile_prefix = created.prefix.clone();
    hostile_prefix.pop();
    hostile_prefix.push('\0');
    let hostile = format!("{hostile_prefix}-plus-a-realistic-length-tail-of-more-characters");

    let start = std::time::Instant::now();
    let result = api_keys::authenticate(&pool, &hostile).await;
    let elapsed = start.elapsed();
    assert!(result.is_ok(), "must be Ok(None), not a database error: {result:?}");
    assert_eq!(result.unwrap(), None);

    assert!(
        elapsed >= std::time::Duration::from_millis(1),
        "a NUL-bearing credential resolved in {elapsed:?} — too fast to have run the argon2id \
         verify against the dummy hash, meaning it took an early-return shortcut instead of \
         paying the same cost as a real miss"
    );
}
