//! Pagination: the envelope shape, the default direction (fixture 22), and the token failure
//! modes the dispatch contract's edge-case list names — tampered, truncated, invalid base64, a
//! foreign scope, and a deleted resource (which must NOT be an error — keyset pagination resumes
//! past a deleted row by design).

mod support;

use base64::{engine::general_purpose::STANDARD, Engine as _};

#[tokio::test]
async fn last_page_omits_the_token_and_a_non_last_page_carries_one() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    for name in ["p1", "p2"] {
        support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": name})).await;
    }

    let first_page = support::get(&router, "/v0/pods?limit=1", Some(&key)).await;
    assert_eq!(first_page.status, 200);
    let v = first_page.json.unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["limit"], 1, "a supplied limit is echoed");
    let token = v["next_page_token"]
        .as_str()
        .expect("more than one pod remains")
        .to_owned();

    let second_page =
        support::get(&router, &format!("/v0/pods?limit=1&page_token={token}"), Some(&key)).await;
    let v2 = second_page.json.unwrap();
    assert_eq!(v2["count"], 1);
    assert!(
        v2.get("next_page_token").is_none(),
        "the last page must OMIT next_page_token entirely: {v2}"
    );
    assert_ne!(v["pods"][0]["pod_id"], v2["pods"][0]["pod_id"], "must actually have advanced");
}

#[tokio::test]
async fn an_omitted_limit_is_not_echoed_and_defaults_to_descending_order() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "older"})).await;
    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "newer"})).await;

    let resp = support::get(&router, "/v0/pods", Some(&key)).await;
    let v = resp.json.unwrap();
    assert!(v.get("limit").is_none(), "an internal default limit must never be echoed");
    let pods = v["pods"].as_array().unwrap();
    assert_eq!(pods[0]["name"], "newer", "fixture 22: newest first by default");
    assert_eq!(pods[1]["name"], "older");
}

#[tokio::test]
async fn ascending_true_reverses_the_default_order() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "older"})).await;
    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "newer"})).await;

    let resp = support::get(&router, "/v0/pods?ascending=true", Some(&key)).await;
    let pods = resp.json.unwrap()["pods"].as_array().unwrap().clone();
    assert_eq!(pods[0]["name"], "older");
    assert_eq!(pods[1]["name"], "newer");
}

#[tokio::test]
async fn a_limit_above_the_maximum_is_clamped_not_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);
    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "p"})).await;

    let resp = support::get(&router, "/v0/pods?limit=999999999", Some(&key)).await;
    assert_eq!(resp.status, 200, "clamped, never a validation_error: {}", resp.body);
    assert_eq!(
        resp.json.unwrap()["limit"],
        100,
        "the echo is the APPLIED value — echoing the caller's raw 999999999 beside 1 item would \
         be self-contradictory"
    );
}

// ---- malformed / hostile page tokens -----------------------------------------------------------

fn assert_invalid_page_token(resp: &support::TestResponse) {
    assert_eq!(resp.status, 400, "body: {}", resp.body);
    assert_eq!(resp.code(), Some("validation_error"), "body: {}", resp.body);
}

#[tokio::test]
async fn an_invalid_base64_page_token_is_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?page_token=!!!not-base64!!!", Some(&key)).await;
    assert_invalid_page_token(&resp);
}

#[tokio::test]
async fn a_truncated_page_token_is_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "a"})).await;
    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "b"})).await;
    let first = support::get(&router, "/v0/pods?limit=1", Some(&key)).await;
    let token = first.json.unwrap()["next_page_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let truncated = &token[..token.len() - 6];

    let resp =
        support::get(&router, &format!("/v0/pods?limit=1&page_token={truncated}"), Some(&key))
            .await;
    assert_invalid_page_token(&resp);
}

#[tokio::test]
async fn a_tampered_but_structurally_valid_page_token_is_rejected_or_silently_wrong_never_a_500() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // A well-formed base64(JSON) object missing the required fields entirely.
    let bogus = STANDARD.encode(r#"{"not":"a real cursor"}"#);
    let resp = support::get(&router, &format!("/v0/pods?page_token={bogus}"), Some(&key)).await;
    assert_invalid_page_token(&resp);
}

#[tokio::test]
async fn a_page_token_from_a_different_scope_is_rejected() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod_a = support::seed_pod(&pool, &org).await;
    let pod_b = support::seed_pod(&pool, &org).await;
    let inbox_a1 = support::seed_inbox(&pool, &org, pod_a, "a1").await;
    let inbox_a2 = support::seed_inbox(&pool, &org, pod_a, "a2").await;
    let org_key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // Mint a token by paging pod A's inboxes...
    let first =
        support::get(&router, &format!("/v0/pods/{pod_a}/inboxes?limit=1"), Some(&org_key)).await;
    let v = first.json.unwrap();
    let inbox_names = [inbox_a1.as_str(), inbox_a2.as_str()];
    assert!(inbox_names.contains(&v["inboxes"][0]["inbox_id"].as_str().unwrap()));
    let token = v["next_page_token"]
        .as_str()
        .expect("2 inboxes in pod A")
        .to_owned();

    // ...then replay it against pod B, which pins a different scope in the cursor check.
    let replayed = support::get(
        &router,
        &format!("/v0/pods/{pod_b}/inboxes?limit=1&page_token={token}"),
        Some(&org_key),
    )
    .await;
    assert_invalid_page_token(&replayed);
}

#[tokio::test]
async fn a_page_token_whose_row_was_deleted_still_resumes_it_is_not_an_error() {
    // Keyset pagination's whole point: the comparison never needs the referenced row to still
    // exist. This is NOT a rejection case, unlike every other test in this file — it is the
    // control that proves the previous ones are about genuine tampering, not database drift.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let created_a =
        support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "a"})).await;
    support::post(&router, "/v0/pods", Some(&key), serde_json::json!({"name": "b"})).await;
    let pod_a_id = created_a.json.unwrap()["pod_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first = support::get(&router, "/v0/pods?limit=1&ascending=true", Some(&key)).await;
    let first_json = first.json.unwrap();
    let token = first_json["next_page_token"].as_str().unwrap().to_owned();
    assert_eq!(first_json["pods"][0]["pod_id"], pod_a_id);

    // Delete the cursor row itself, then resume from the token minted while it still existed.
    support::delete(&router, &format!("/v0/pods/{pod_a_id}"), Some(&key)).await;

    let resumed = support::get(
        &router,
        &format!("/v0/pods?limit=1&ascending=true&page_token={token}"),
        Some(&key),
    )
    .await;
    assert_eq!(
        resumed.status, 200,
        "a deleted cursor row must not turn into an error: {}",
        resumed.body
    );
    assert_eq!(resumed.json.unwrap()["pods"][0]["name"], "b");
}
