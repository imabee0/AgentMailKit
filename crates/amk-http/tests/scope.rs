//! The highest-value tests in this dispatch: a scope or cross-org denial masks as `not_found`
//! (404), never `forbidden` (403), at all three mounts. A test that only checks `!= 200` would
//! pass even if the status flipped to 403 — every assertion here pins the exact code.

mod support;

use amk_types::api_key::ApiKeyPermissions;

fn assert_masked_not_found(resp: &support::TestResponse) {
    assert_eq!(resp.status, 404, "a scope/cross-org denial must be 404, body: {}", resp.body);
    assert_eq!(resp.code(), Some("not_found"), "must be not_found, body: {}", resp.body);
    assert!(resp.json.as_ref().unwrap().get("suggestions").is_none(), "a mask must not hint");
}

// ---- pod-scoped key, cross-pod -----------------------------------------------------------------

#[tokio::test]
async fn pod_scoped_key_reaching_a_sibling_pods_inbox_is_not_found_never_forbidden() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let mine = support::seed_pod(&pool, &org).await;
    let theirs = support::seed_pod(&pool, &org).await;
    let their_inbox = support::seed_inbox(&pool, &org, theirs, "foreign").await;
    let key = support::pod_key(&pool, &org, mine).await;
    let router = support::test_router(pool);

    let resp = support::get(
        &router,
        &format!("/v0/inboxes/{}", their_inbox.to_path_segment()),
        Some(&key),
    )
    .await;
    assert_masked_not_found(&resp);
}

#[tokio::test]
async fn pod_scoped_key_reaching_a_sibling_pod_directly_is_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let mine = support::seed_pod(&pool, &org).await;
    let theirs = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, mine).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, &format!("/v0/pods/{theirs}"), Some(&key)).await;
    assert_masked_not_found(&resp);
    // And its own pod is visible, the boundary on the other side.
    let mine_resp = support::get(&router, &format!("/v0/pods/{mine}"), Some(&key)).await;
    assert_eq!(mine_resp.status, 200);
}

#[tokio::test]
async fn pod_scoped_key_probing_a_foreign_inbox_mount_is_not_found_not_an_empty_list() {
    // The regression amk_core::scope's own doc warns about: a Mount::Inbox probe that is
    // accepted without proof lets a nonexistent-to-this-credential inbox answer 200 {"count":0}
    // instead of 404. This is the reachable HTTP path for it: a pod-scoped key's api-keys list
    // under a sibling pod's inbox.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let mine = support::seed_pod(&pool, &org).await;
    let theirs = support::seed_pod(&pool, &org).await;
    let their_inbox = support::seed_inbox(&pool, &org, theirs, "foreign").await;
    let key = support::pod_key(&pool, &org, mine).await;
    let router = support::test_router(pool);

    let resp = support::get(
        &router,
        &format!("/v0/inboxes/{}/api-keys", their_inbox.to_path_segment()),
        Some(&key),
    )
    .await;
    assert_masked_not_found(&resp);
}

#[tokio::test]
async fn org_scoped_key_probing_a_nonexistent_pod_mount_is_not_found_not_an_empty_list() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let nonexistent = uuid::Uuid::new_v4();
    let resp = support::get(&router, &format!("/v0/pods/{nonexistent}/inboxes"), Some(&key)).await;
    assert_masked_not_found(&resp);
}

// ---- inbox-scoped key, cross-inbox --------------------------------------------------------------

#[tokio::test]
async fn inbox_scoped_key_cannot_see_a_sibling_inbox_in_the_same_pod() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mine = support::seed_inbox(&pool, &org, pod, "mine").await;
    let sibling = support::seed_inbox(&pool, &org, pod, "sibling").await;
    let key = support::inbox_key(&pool, &org, &mine).await;
    let router = support::test_router(pool);

    let resp =
        support::get(&router, &format!("/v0/inboxes/{}", sibling.to_path_segment()), Some(&key))
            .await;
    assert_masked_not_found(&resp);

    // Its own inbox is visible.
    let own =
        support::get(&router, &format!("/v0/inboxes/{}", mine.to_path_segment()), Some(&key)).await;
    assert_eq!(own.status, 200);

    // And an inbox-scoped key cannot read its own parent pod.
    let pod_resp = support::get(&router, &format!("/v0/pods/{pod}"), Some(&key)).await;
    assert_masked_not_found(&pod_resp);
}

#[tokio::test]
async fn inbox_scoped_key_lists_at_most_its_own_inbox_from_the_org_mount() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mine = support::seed_inbox(&pool, &org, pod, "mine").await;
    let _sibling = support::seed_inbox(&pool, &org, pod, "sibling").await;
    let key = support::inbox_key(&pool, &org, &mine).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/inboxes", Some(&key)).await;
    assert_eq!(resp.status, 200);
    let v = resp.json.unwrap();
    assert_eq!(v["count"], 1, "must see only its own inbox, never the sibling: {v}");
    assert_eq!(v["inboxes"][0]["inbox_id"], mine.as_str());
}

// ---- delete-specific: masking must hold on the mutating path too, not only GET -----------------

#[tokio::test]
async fn pod_scoped_key_deleting_a_sibling_pod_is_masked_before_reaching_the_store() {
    // `pods::delete`'s own pre-check (the extra ownership pin `handlers::pods::delete` applies
    // before calling the store) is exercised only by GET elsewhere in this file — DELETE has an
    // independent guard and needs its own pin.
    //
    // The clean-path pin can't be "the pod-scoped key deletes its own pod and gets 204": the key
    // authenticating the request is ITSELF a live `api_keys.pod_id` foreign-key reference into
    // that pod (migration 0007), so that pod is never actually empty while being used to make the
    // request — deleting it always 409s at the store, guard or no guard. The precise, guard-only
    // signal is the status CODE the two cases produce: a sibling pod is masked as 404 not_found
    // before the store is ever called; its own (non-empty-by-construction) pod reaches the store
    // and gets 409 cannot_delete — proving the guard let the legitimate target through rather than
    // rejecting it outright.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let mine = support::seed_pod(&pool, &org).await;
    let theirs = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, mine).await;
    let router = support::test_router(pool);

    let sibling_resp = support::delete(&router, &format!("/v0/pods/{theirs}"), Some(&key)).await;
    assert_masked_not_found(&sibling_resp);

    let own_resp = support::delete(&router, &format!("/v0/pods/{mine}"), Some(&key)).await;
    assert_eq!(
        own_resp.status, 409,
        "the guard must let its OWN pod reach the store, not mask it too: {}",
        own_resp.body
    );
    assert_eq!(own_resp.code(), Some("cannot_delete"));
}

#[tokio::test]
async fn inbox_scoped_key_cannot_patch_or_delete_a_sibling_inbox_but_can_its_own() {
    // `bound_inbox_matches` gates `get_inbox`, `update_inbox` and `delete_inbox` identically, but
    // only the GET path was pinned by a test elsewhere in this file — PATCH/DELETE need their own
    // clean-path and hostile-path pins, since they are independent call sites of the same guard.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let mine = support::seed_inbox(&pool, &org, pod, "mine").await;
    let sibling = support::seed_inbox(&pool, &org, pod, "sibling").await;
    let key = support::inbox_key(&pool, &org, &mine).await;
    let router = support::test_router(pool);

    // Hostile: PATCH/DELETE on the sibling are both masked as not_found.
    let patch_sibling = support::patch(
        &router,
        &format!("/v0/inboxes/{}", sibling.to_path_segment()),
        Some(&key),
        serde_json::json!({"display_name": "hijacked"}),
    )
    .await;
    assert_masked_not_found(&patch_sibling);

    let delete_sibling =
        support::delete(&router, &format!("/v0/inboxes/{}", sibling.to_path_segment()), Some(&key))
            .await;
    assert_masked_not_found(&delete_sibling);

    // Clean: the same operations on its own inbox succeed.
    let patch_own = support::patch(
        &router,
        &format!("/v0/inboxes/{}", mine.to_path_segment()),
        Some(&key),
        serde_json::json!({"display_name": "renamed"}),
    )
    .await;
    assert_eq!(patch_own.status, 200, "body: {}", patch_own.body);

    let delete_own =
        support::delete(&router, &format!("/v0/inboxes/{}", mine.to_path_segment()), Some(&key))
            .await;
    assert_eq!(delete_own.status, 202, "body: {}", delete_own.body);
}

// ---- cross-organization, at all three mounts -----------------------------------------------

#[tokio::test]
async fn cross_organization_is_masked_at_all_three_mounts() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org_a = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org_a).await;
    let inbox_a = support::seed_inbox(&pool, &org_a, pod_a, "a").await;

    let org_b = support::seed_org(&pool).await;
    let pod_b = support::seed_pod(&pool, &org_b).await;
    let inbox_b = support::seed_inbox(&pool, &org_b, pod_b, "b").await;

    let key_a = support::org_key(&pool, &org_a).await;
    let router = support::test_router(pool);

    // Organization mount: org A's key can never see org B's inbox by id.
    let resp =
        support::get(&router, &format!("/v0/inboxes/{}", inbox_b.to_path_segment()), Some(&key_a))
            .await;
    assert_masked_not_found(&resp);

    // Pod mount: org A's key naming org B's pod id.
    let resp = support::get(&router, &format!("/v0/pods/{pod_b}"), Some(&key_a)).await;
    assert_masked_not_found(&resp);
    let resp = support::get(&router, &format!("/v0/pods/{pod_b}/inboxes"), Some(&key_a)).await;
    assert_masked_not_found(&resp);

    // Inbox mount: org A's key naming org B's inbox in a sub-collection.
    let resp = support::get(
        &router,
        &format!("/v0/inboxes/{}/api-keys", inbox_b.to_path_segment()),
        Some(&key_a),
    )
    .await;
    assert_masked_not_found(&resp);

    // The sanity check on the other side: org A's own resources are all visible to it.
    let resp =
        support::get(&router, &format!("/v0/inboxes/{}", inbox_a.to_path_segment()), Some(&key_a))
            .await;
    assert_eq!(resp.status, 200);
}

// ---- permission escalation: child cannot exceed parent, at every level ------------------------

#[tokio::test]
async fn a_child_key_requesting_a_permission_the_parent_lacks_is_permission_escalation() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // Parent holds only api_key_create + inbox_read; nothing else.
    let parent_perms = ApiKeyPermissions {
        api_key_create: Some(true),
        inbox_read: Some(true),
        ..Default::default()
    };
    let parent = support::mint_key(&pool, &org, None, None, Some(parent_perms)).await;
    let router = support::test_router(pool);

    // Requesting a permission (message_send) the parent does not hold.
    let resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&parent),
        serde_json::json!({"name": "escalated", "permissions": {"message_send": true}}),
    )
    .await;
    assert_eq!(resp.status, 403, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("permission_escalation"), "body: {}", resp.body);
}

#[tokio::test]
async fn a_restricted_parent_cannot_mint_an_unrestricted_child() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let parent_perms = ApiKeyPermissions { api_key_create: Some(true), ..Default::default() };
    let parent = support::mint_key(&pool, &org, None, None, Some(parent_perms)).await;
    let router = support::test_router(pool);

    // No `permissions` field at all -> an unrestricted child request.
    let resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&parent),
        serde_json::json!({"name": "unbounded"}),
    )
    .await;
    assert_eq!(resp.status, 403, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("permission_escalation"), "body: {}", resp.body);
}

#[tokio::test]
async fn a_child_key_equal_to_its_parent_is_allowed() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let parent_perms = ApiKeyPermissions {
        api_key_create: Some(true),
        inbox_read: Some(true),
        ..Default::default()
    };
    let parent = support::mint_key(&pool, &org, None, None, Some(parent_perms)).await;
    let router = support::test_router(pool);

    let resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&parent),
        serde_json::json!({"name": "same", "permissions": {"inbox_read": true}}),
    )
    .await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
}

#[tokio::test]
async fn escalation_is_enforced_at_every_level_of_the_chain() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let root = support::org_key(&pool, &org).await; // unrestricted
    let router = support::test_router(pool);

    // root -> parent, restricted to api_key_create + message_read + inbox_read.
    let parent_resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&root),
        serde_json::json!({
            "name": "parent",
            "permissions": {"api_key_create": true, "message_read": true, "inbox_read": true}
        }),
    )
    .await;
    assert_eq!(parent_resp.status, 200, "body: {}", parent_resp.body);
    let parent_key = parent_resp.json.unwrap()["api_key"]
        .as_str()
        .unwrap()
        .to_owned();

    // parent -> child, dropping inbox_read.
    let child_resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&parent_key),
        serde_json::json!({
            "name": "child",
            "permissions": {"api_key_create": true, "message_read": true}
        }),
    )
    .await;
    assert_eq!(child_resp.status, 200, "body: {}", child_resp.body);
    let child_key = child_resp.json.unwrap()["api_key"]
        .as_str()
        .unwrap()
        .to_owned();

    // child -> grandchild, trying to reclaim inbox_read: must fail, even though the ROOT (and
    // the grandparent) both held it.
    let grandchild_resp = support::post(
        &router,
        "/v0/api-keys",
        Some(&child_key),
        serde_json::json!({"name": "grandchild", "permissions": {"inbox_read": true}}),
    )
    .await;
    assert_eq!(grandchild_resp.status, 403, "body: {}", grandchild_resp.body);
    assert_eq!(grandchild_resp.code(), Some("permission_escalation"));
}
