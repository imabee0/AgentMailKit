//! `/v0/api-keys` at all three mounts: creation scope inheritance, listing per `KeyScope`, and the
//! secret shape (fixture 23: 204 delete, no GET-by-id).
//!
//! **Not tested here, because it is structural rather than behavioural:** a list/get response
//! never carries the plaintext secret. `handlers::api_keys::list_keys` and `delete_key` both
//! serialize `amk_types::api_key::ApiKey`, which has no `api_key` field at all — only
//! `CreateApiKeyResponse` (the one-time create response) does. There is no redaction step to test
//! because there is nothing to redact: the type itself cannot carry the secret on those paths.

mod support;

#[tokio::test]
async fn create_at_org_mount_is_organization_scoped() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp =
        support::post(&router, "/v0/api-keys", Some(&key), serde_json::json!({"name": "org-key"}))
            .await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    let v = resp.json.unwrap();
    assert!(v.get("pod_id").is_none());
    assert!(v.get("inbox_id").is_none());
    assert!(v["api_key"].as_str().unwrap().starts_with("am_us_"));
    assert_eq!(v["organization_id"], org.as_str());
}

#[tokio::test]
async fn create_at_pod_mount_is_pod_scoped_and_at_inbox_mount_is_inbox_scoped() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "scoped").await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let pod_resp = support::post(
        &router,
        &format!("/v0/pods/{pod}/api-keys"),
        Some(&key),
        serde_json::json!({"name": "pod-key"}),
    )
    .await;
    assert_eq!(pod_resp.status, 200, "body: {}", pod_resp.body);
    let v = pod_resp.json.unwrap();
    assert_eq!(v["pod_id"], pod.to_string());
    assert!(v.get("inbox_id").is_none());

    let inbox_resp = support::post(
        &router,
        &format!("/v0/inboxes/{}/api-keys", inbox.to_path_segment()),
        Some(&key),
        serde_json::json!({"name": "inbox-key"}),
    )
    .await;
    assert_eq!(inbox_resp.status, 200, "body: {}", inbox_resp.body);
    let v = inbox_resp.json.unwrap();
    assert_eq!(v["inbox_id"], inbox.as_str());
    // Divergence 4 (fixture 25): the STORED row's own `pod_id` column is still NULL for an
    // inbox-scoped key — `inbox_id` alone is the scope, the CHECK is untouched — but the RESPONSE
    // now also carries `pod_id`, the containing pod, as denormalised provenance. `pod` here is
    // that same containing pod (the inbox was created in it above), not merely `is_some()`.
    assert_eq!(
        v["pod_id"],
        pod.to_string(),
        "an inbox-scoped key's response must carry the pod that actually contains its inbox"
    );
}

#[tokio::test]
async fn a_pod_scoped_credential_creating_at_the_org_mount_is_narrowed_to_its_own_pod() {
    // POST /v0/api-keys through a pod-scoped credential: the created key must not become
    // organization-scoped (which would be an escalation route around the mount system entirely).
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let resp =
        support::post(&router, "/v0/api-keys", Some(&key), serde_json::json!({"name": "narrowed"}))
            .await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    let v = resp.json.unwrap();
    assert_eq!(v["pod_id"], pod.to_string(), "must inherit the creating credential's own pod");
}

#[tokio::test]
async fn list_scoping_matches_the_mount_a_pod_key_does_not_see_org_wide_keys() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let org_key = support::org_key(&pool, &org).await;
    let pod_key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    // Seed one more org-scoped key and one pod-scoped key.
    support::post(
        &router,
        "/v0/api-keys",
        Some(&org_key),
        serde_json::json!({"name": "another-org-key"}),
    )
    .await;
    let pod_created = support::post(
        &router,
        &format!("/v0/pods/{pod}/api-keys"),
        Some(&org_key),
        serde_json::json!({"name": "a-pod-key"}),
    )
    .await;
    let pod_key_id = pod_created.json.unwrap()["api_key_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The pod-scoped credential listing at the pod mount sees only pod-scoped keys, not the
    // org-scoped ones (KeyScope::Pod pins the row's own pod_id column).
    let listed = support::get(&router, &format!("/v0/pods/{pod}/api-keys"), Some(&pod_key)).await;
    assert_eq!(listed.status, 200, "body: {}", listed.body);
    let items = listed.json.unwrap()["api_keys"].as_array().unwrap().clone();
    assert!(items.iter().any(|k| k["api_key_id"] == pod_key_id));
    assert!(
        items.iter().all(|k| k["pod_id"] == pod.to_string()),
        "every listed key must be scoped to this pod: {items:?}"
    );
}

#[tokio::test]
async fn delete_is_204_and_there_is_no_get_by_id_route() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let created =
        support::post(&router, "/v0/api-keys", Some(&key), serde_json::json!({"name": "temp"}))
            .await;
    let api_key_id = created.json.unwrap()["api_key_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // No GET /v0/api-keys/{id} route exists at all — the fallback answers, not a resource miss.
    let get_resp = support::get(&router, &format!("/v0/api-keys/{api_key_id}"), Some(&key)).await;
    assert_eq!(get_resp.status, 404);
    assert_eq!(get_resp.code(), Some("not_found"));

    let deleted = support::delete(&router, &format!("/v0/api-keys/{api_key_id}"), Some(&key)).await;
    assert_eq!(deleted.status, 204, "body: {}", deleted.body);
    assert!(deleted.body.is_empty());

    // A second delete of the same id is now a genuine not_found.
    let again = support::delete(&router, &format!("/v0/api-keys/{api_key_id}"), Some(&key)).await;
    assert_eq!(again.status, 404);
}

#[tokio::test]
async fn a_pod_scoped_credential_cannot_delete_an_org_scoped_key_via_the_pod_mount() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let org_key = support::org_key(&pool, &org).await;
    let pod_key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let org_scoped_created = support::post(
        &router,
        "/v0/api-keys",
        Some(&org_key),
        serde_json::json!({"name": "org-scoped"}),
    )
    .await;
    let org_scoped_id = org_scoped_created.json.unwrap()["api_key_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = support::delete(
        &router,
        &format!("/v0/pods/{pod}/api-keys/{org_scoped_id}"),
        Some(&pod_key),
    )
    .await;
    assert_eq!(resp.status, 404, "an org-scoped key is not IN the pod's own KeyScope");

    // It survives — the org-scoped credential itself can still see it.
    let listed = support::get(&router, "/v0/api-keys", Some(&org_key)).await;
    assert!(listed.json.unwrap()["api_keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["api_key_id"] == org_scoped_id));
}
