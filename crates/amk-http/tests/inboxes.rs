//! `/v0/inboxes` and `/v0/pods/{pod_id}/inboxes`: creation defaults (fixture 23), the collision
//! shape (fixture 05), case folding (fixture 18), the `PATCH` validation rules this crate owns,
//! and the 202 delete status (fixture 22).

mod support;

#[tokio::test]
async fn create_with_explicit_fields_round_trips() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let username = format!("probe-{}", support::unique_suffix());
    let resp = support::post(
        &router,
        "/v0/inboxes",
        Some(&key),
        serde_json::json!({"username": username, "display_name": "Probe"}),
    )
    .await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    let v = resp.json.unwrap();
    assert_eq!(v["inbox_id"], format!("{username}@example.test"));
    assert_eq!(v["email"], v["inbox_id"]);
    assert_eq!(v["display_name"], "Probe");
    assert_eq!(v["pod_id"], pod.to_string());
}

#[tokio::test]
async fn create_with_an_empty_body_generates_username_domain_and_display_name() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let resp = support::post(&router, "/v0/inboxes", Some(&key), serde_json::json!({})).await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    let v = resp.json.unwrap();
    let inbox_id = v["inbox_id"].as_str().unwrap();
    assert!(inbox_id.ends_with("@example.test"), "{inbox_id}");
    let local = inbox_id.strip_suffix("@example.test").unwrap();
    assert!(
        local
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "generated shape must be lowercase alnum, no separator: {local}"
    );
    assert_eq!(v["display_name"], "AmkTest", "the configured product_name, not AgentMail's own");
}

#[tokio::test]
async fn create_with_no_configured_domain_fails_closed_not_agentmail_to() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::unconfigured_router(pool);

    let resp = support::post(&router, "/v0/inboxes", Some(&key), serde_json::json!({})).await;
    assert_eq!(resp.status, 500, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("internal_error"));
    // No inbox object is returned — the `docs` field's fixed host
    // (`docs.agentmail.to/errors#...`, the one legitimate `agentmail.to` substring every envelope
    // carries) is not evidence a guessed domain leaked; the absence of an `inbox_id`/`email` is.
    let v = resp.json.unwrap();
    assert!(v.get("inbox_id").is_none() && v.get("email").is_none(), "{v}");
}

#[tokio::test]
async fn collision_is_already_exists_403_with_three_suggestions_none_colliding() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let username = format!("dup-{}", support::unique_suffix());
    let first = support::post(
        &router,
        "/v0/inboxes",
        Some(&key),
        serde_json::json!({"username": username}),
    )
    .await;
    assert_eq!(first.status, 200);

    let collision = support::post(
        &router,
        "/v0/inboxes",
        Some(&key),
        serde_json::json!({"username": username}),
    )
    .await;
    assert_eq!(collision.status, 403, "body: {}", collision.body);
    assert_eq!(collision.code(), Some("already_exists"), "body: {}", collision.body);
    let v = collision.json.unwrap();
    let suggestions = v["suggestions"].as_array().expect("suggestions[] present");
    assert_eq!(suggestions.len(), 3, "{suggestions:?}");
    for s in suggestions {
        let s = s.as_str().unwrap();
        assert!(s.starts_with(&username), "{s}");
        assert_eq!(s.len(), username.len() + 4, "{s}");
        assert!(s[username.len()..].chars().all(|c| c.is_ascii_digit()), "{s}");
    }
    let unique: std::collections::HashSet<_> = suggestions.iter().collect();
    assert_eq!(unique.len(), suggestions.len(), "suggestions must not repeat: {suggestions:?}");
}

#[tokio::test]
async fn inbox_lookup_folds_case_per_fixture_18() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let username = format!("MixedCase{}", support::unique_suffix());
    let created = support::post(
        &router,
        "/v0/inboxes",
        Some(&key),
        serde_json::json!({"username": username}),
    )
    .await;
    let stored_id = created.json.unwrap()["inbox_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(stored_id, stored_id.to_lowercase(), "creation must lowercase the username");

    for variant in [stored_id.to_uppercase(), swapcase(&stored_id)] {
        let resp =
            support::get(&router, &format!("/v0/inboxes/{}", percent_encode(&variant)), Some(&key))
                .await;
        assert_eq!(resp.status, 200, "{variant} must resolve, body: {}", resp.body);
        assert_eq!(resp.json.unwrap()["inbox_id"], stored_id);
    }
}

fn swapcase(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

fn percent_encode(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

#[tokio::test]
async fn plus_addressing_round_trips_through_the_path() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let username = format!("plus{}+tag", support::unique_suffix());
    let created = support::post(
        &router,
        "/v0/inboxes",
        Some(&key),
        serde_json::json!({"username": username}),
    )
    .await;
    assert_eq!(created.status, 200, "body: {}", created.body);
    let inbox_id = created.json.unwrap()["inbox_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(inbox_id.contains('+'), "{inbox_id}");

    let resp =
        support::get(&router, &format!("/v0/inboxes/{}", percent_encode(&inbox_id)), Some(&key))
            .await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    assert_eq!(resp.json.unwrap()["inbox_id"], inbox_id);
}

// ---- update validation (this crate's own rules) -----------------------------------------------

#[tokio::test]
async fn update_with_neither_field_is_a_validation_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "up").await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let resp = support::patch(
        &router,
        &format!("/v0/inboxes/{}", inbox.to_path_segment()),
        Some(&key),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status, 400, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("validation_error"));
}

#[tokio::test]
async fn update_with_an_empty_metadata_object_is_a_validation_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "up").await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);

    let resp = support::patch(
        &router,
        &format!("/v0/inboxes/{}", inbox.to_path_segment()),
        Some(&key),
        serde_json::json!({"metadata": {}}),
    )
    .await;
    assert_eq!(resp.status, 400, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("validation_error"));
}

#[tokio::test]
async fn update_metadata_null_clears_and_display_name_alone_is_accepted() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "up").await;
    let key = support::pod_key(&pool, &org, pod).await;
    let router = support::test_router(pool);
    let path = format!("/v0/inboxes/{}", inbox.to_path_segment());

    let with_meta =
        support::patch(&router, &path, Some(&key), serde_json::json!({"metadata": {"a": "1"}}))
            .await;
    assert_eq!(with_meta.status, 200, "body: {}", with_meta.body);
    assert_eq!(with_meta.json.unwrap()["metadata"], serde_json::json!({"a": "1"}));

    let renamed =
        support::patch(&router, &path, Some(&key), serde_json::json!({"display_name": "New"}))
            .await;
    assert_eq!(renamed.status, 200, "body: {}", renamed.body);
    let v = renamed.json.unwrap();
    assert_eq!(v["display_name"], "New");
    assert_eq!(v["metadata"], serde_json::json!({"a": "1"}), "unchanged when omitted");

    let cleared =
        support::patch(&router, &path, Some(&key), serde_json::json!({"metadata": null})).await;
    assert_eq!(cleared.status, 200, "body: {}", cleared.body);
    assert!(
        cleared.json.unwrap().get("metadata").is_none(),
        "cleared metadata is omitted, not null"
    );
}

// ---- pod mount ---------------------------------------------------------------------------------

#[tokio::test]
async fn pod_mount_create_list_get_update_delete() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let username = format!("podmount-{}", support::unique_suffix());
    let created = support::post(
        &router,
        &format!("/v0/pods/{pod}/inboxes"),
        Some(&key),
        serde_json::json!({"username": username}),
    )
    .await;
    assert_eq!(created.status, 200, "body: {}", created.body);
    let inbox_id = created.json.unwrap()["inbox_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let listed = support::get(&router, &format!("/v0/pods/{pod}/inboxes"), Some(&key)).await;
    assert_eq!(listed.status, 200);
    assert!(listed.json.unwrap()["inboxes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["inbox_id"] == inbox_id));

    let path = format!("/v0/pods/{pod}/inboxes/{}", percent_encode(&inbox_id));
    let got = support::get(&router, &path, Some(&key)).await;
    assert_eq!(got.status, 200);

    let updated =
        support::patch(&router, &path, Some(&key), serde_json::json!({"display_name": "X"})).await;
    assert_eq!(updated.status, 200, "body: {}", updated.body);

    let deleted = support::delete(&router, &path, Some(&key)).await;
    assert_eq!(deleted.status, 202, "fixture 22: inbox delete is accepted-then-processed");
}

#[tokio::test]
async fn pod_mount_inbox_operations_on_a_nonexistent_pod_are_masked_not_found() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);
    let nonexistent = uuid::Uuid::new_v4();

    let resp = support::get(&router, &format!("/v0/pods/{nonexistent}/inboxes"), Some(&key)).await;
    assert_eq!(resp.status, 404);
    assert_eq!(resp.code(), Some("not_found"));

    let resp = support::post(
        &router,
        &format!("/v0/pods/{nonexistent}/inboxes"),
        Some(&key),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status, 404, "creating under a nonexistent pod must not silently succeed");
}

// ---- org mount default-pod resolution (fixture 22, Q1) -----------------------------------------

#[tokio::test]
async fn org_mount_create_resolves_the_pod_whose_id_equals_the_organization_id() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org_with_default_pod(&pool).await;
    // A second, non-default pod, so the resolution is provably not "the only pod" or "the newest".
    let _other_pod = support::seed_pod(&pool, &org).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::post(&router, "/v0/inboxes", Some(&key), serde_json::json!({})).await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    let v = resp.json.unwrap();
    assert_eq!(
        v["pod_id"],
        org.as_str(),
        "must resolve the pod whose pod_id == organization_id"
    );
}

#[tokio::test]
async fn org_mount_create_is_an_internal_error_when_no_default_pod_exists() {
    let Some(pool) = support::pool().await else {
        return;
    };
    // seed_org WITHOUT the matching default pod.
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::post(&router, "/v0/inboxes", Some(&key), serde_json::json!({})).await;
    assert_eq!(resp.status, 500, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("internal_error"));
}
