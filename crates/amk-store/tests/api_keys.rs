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

// ---- get()/delete() with a syntactically invalid id --------------------------------------

#[tokio::test]
async fn a_non_uuid_api_key_id_is_treated_as_not_found_not_an_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // A real row exists in the same organization throughout — this is the shape a review lens
    // used to catch an earlier version of this module: `get`'s `Uuid::parse_str(..).ok()` fed a
    // `let Some(id) = .. else { return Ok(None) }`, and mutating that early return into
    // `.unwrap_or_else(Uuid::nil)` made a malformed id resolve any row seeded at the nil UUID.
    // `get`/`delete`/`touch_used_at` no longer parse the id in Rust at all — the presented string
    // is compared against the column's own canonical text directly in SQL — so there is no
    // Rust-side fallback value left for a mutation to substitute, and a garbage id cannot resolve
    // *this* row (or any row) regardless of what exists alongside it.
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

/// `get`/`delete`/`touch_used_at` compare `api_key_id::text = lower($n)` rather than a native
/// `uuid = uuid` comparison, specifically so a caller-presented id needs no Rust-side parsing —
/// but that shifts case-insensitivity onto the `lower()` call, which a native `Uuid` comparison
/// used to give for free. A differently-cased rendering of a real id must still resolve it.
#[tokio::test]
async fn get_resolves_a_differently_cased_rendering_of_a_real_api_key_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let created = api_keys::create(&pool, org_key(&org, "case-check"))
        .await
        .unwrap();
    let uppercased = ApiKeyId::new(created.api_key_id.as_str().to_uppercase());
    assert_ne!(uppercased, created.api_key_id, "sanity: the rendering actually differs");

    let resolved = api_keys::get(&pool, &org, &KeyScope::Organization, &uppercased)
        .await
        .unwrap();
    assert!(
        resolved.is_some(),
        "an uppercased rendering of a real UUID must still resolve it"
    );
    assert_eq!(resolved.unwrap().api_key_id, created.api_key_id);
}
