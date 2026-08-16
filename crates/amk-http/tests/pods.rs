//! `/v0/pods`: list, create, get, delete — including the two probe-settled facts
//! (`reference/fixtures/22-org-mount-and-delete-semantics.txt`): `DELETE` on a non-empty pod is
//! `409 cannot_delete`, and `DELETE` on an empty one is `204`.

mod support;

#[tokio::test]
async fn create_list_get_delete_round_trip() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let created =
        support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "probe-pod"}))
            .await;
    assert_eq!(created.status, 200, "body: {}", created.body);
    let body = created.json.unwrap();
    assert_eq!(body["name"], "probe-pod");
    assert_eq!(body["organization_id"], org.as_str());
    assert!(body.get("client_id").is_none(), "an absent optional must be omitted, not null");
    let pod_id = body["pod_id"].as_str().unwrap().to_owned();

    let got = support::get(&router, &format!("/v0/pods/{pod_id}"), Some(&key)).await;
    assert_eq!(got.status, 200);
    assert_eq!(got.json.unwrap()["pod_id"], pod_id);

    let listed = support::get(&router, "/v0/pods", Some(&key)).await;
    assert_eq!(listed.status, 200);
    let v = listed.json.unwrap();
    assert!(v["pods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["pod_id"] == pod_id));
    assert!(v.get("limit").is_none(), "an unsupplied limit must not be echoed");

    let deleted = support::delete(&router, &format!("/v0/pods/{pod_id}"), Some(&key)).await;
    assert_eq!(deleted.status, 204, "empty pod deletes with 204, body: {}", deleted.body);
    assert!(deleted.body.is_empty(), "204 must carry no body: {:?}", deleted.body);
}

#[tokio::test]
async fn deleting_a_pod_that_still_owns_an_inbox_is_409_cannot_delete_and_nothing_is_touched() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "occupant").await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::delete(&router, &format!("/v0/pods/{pod}"), Some(&key)).await;
    assert_eq!(resp.status, 409, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("cannot_delete"), "body: {}", resp.body);

    // The refusal is total: both survive.
    let pod_still = support::get(&router, &format!("/v0/pods/{pod}"), Some(&key)).await;
    assert_eq!(pod_still.status, 200);
    let inbox_still =
        support::get(&router, &format!("/v0/inboxes/{}", inbox.to_path_segment()), Some(&key))
            .await;
    assert_eq!(inbox_still.status, 200);
}

#[tokio::test]
async fn get_pod_requires_pod_read_even_when_the_pod_is_in_scope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::mint_key(&pool, &org, None, None, Some(Default::default())).await; // empty grants
    let router = support::test_router(pool);

    let resp = support::get(&router, &format!("/v0/pods/{pod}"), Some(&key)).await;
    assert_eq!(resp.status, 403, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("missing_permission"), "body: {}", resp.body);
}

#[tokio::test]
async fn a_nonexistent_pod_id_is_masked_the_same_whether_or_not_the_permission_is_held() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key_full = support::org_key(&pool, &org).await;
    let key_blind = support::mint_key(&pool, &org, None, None, Some(Default::default())).await;
    let router = support::test_router(pool);

    let missing_pod = uuid::Uuid::new_v4();
    let full_resp =
        support::get(&router, &format!("/v0/pods/{missing_pod}"), Some(&key_full)).await;
    let blind_resp =
        support::get(&router, &format!("/v0/pods/{missing_pod}"), Some(&key_blind)).await;
    // Scope decided BEFORE the flag: both an unrestricted key AND a permission-less one see the
    // identical 404 for a resource that genuinely does not exist — the flag never gets a chance
    // to turn "absent" into "forbidden".
    assert_eq!(full_resp.status, 404, "body: {}", full_resp.body);
    assert_eq!(blind_resp.status, 404, "body: {}", blind_resp.body);
    assert_eq!(full_resp.code(), blind_resp.code());
}

#[tokio::test]
async fn pod_create_client_id_is_idempotent() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let body = serde_json::json!({"name": "idempotent-pod", "client_id": "cid-1"});
    let first = support::post(&router, "/v0/pods", Some(&key), body.clone()).await;
    let second = support::post(&router, "/v0/pods", Some(&key), body).await;
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    assert_eq!(
        first.json.unwrap()["pod_id"],
        second.json.unwrap()["pod_id"],
        "replaying the same client_id must return the original pod, not a duplicate"
    );
}
