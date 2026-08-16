//! The auth-layer asymmetry (`reference/fixtures/05-error-catalog.http`): a **missing** header is
//! bare 401; a **present-but-unusable** header (malformed, or a well-formed-but-unknown `am_`
//! key) is bare 403. Both bodies must be `{"message":…}` and nothing else — a test that only
//! checked the status code would pass even if the envelope leaked back in.

mod support;

#[tokio::test]
async fn missing_authorization_header_is_bare_401_unauthorized() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/auth/me", None).await;
    assert_eq!(resp.status, 401);
    let v = resp.json.expect("body must be JSON");
    assert_eq!(v, serde_json::json!({"message": "Unauthorized"}));
    // The single most load-bearing assertion in this file: a `code` or `name` field appearing
    // here is the bug the auth/app asymmetry exists to prevent.
    assert!(v.get("code").is_none(), "bare body must never carry code: {v}");
    assert!(v.get("name").is_none(), "bare body must never carry name: {v}");
    assert!(v.get("fix").is_none());
    assert!(v.get("docs").is_none());
}

#[tokio::test]
async fn a_well_formed_but_unknown_key_is_bare_403_not_an_envelope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    // Well-formed shape (am_us_ + hex), never minted — fixture 05's own probe value.
    let resp =
        support::get(&router, "/v0/auth/me", Some("am_us_00000000000000000000000000000000")).await;
    assert_eq!(resp.status, 403);
    let v = resp.json.expect("body must be JSON");
    assert_eq!(v, serde_json::json!({"message": "Forbidden"}));
    assert!(v.get("code").is_none(), "bare body must never carry code: {v}");
    assert!(v.get("name").is_none(), "bare body must never carry name: {v}");
}

#[tokio::test]
async fn a_malformed_bearer_token_is_bare_403() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/auth/me", Some("not-a-real-key")).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.json.unwrap(), serde_json::json!({"message": "Forbidden"}));
}

#[tokio::test]
async fn a_non_bearer_scheme_is_bare_403_not_401() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/v0/auth/me")
        .header("authorization", "Basic dXNlcjpwYXNz")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(
        response.status(),
        403,
        "a present, merely-wrong-scheme header is not a MISSING one"
    );
}

#[tokio::test]
async fn a_valid_secret_presented_without_the_bearer_prefix_is_bare_403() {
    // A genuine, minted key's secret sent as the WHOLE header value (no "Bearer " prefix) must
    // still be rejected — this pins that the scheme strip is a real `strip_prefix`, not an
    // `unwrap_or(value)` that would fall through to treating the raw header as the presented
    // secret. Sending it as `Authorization: Basic <secret>` would coincidentally still 403
    // (the string "Basic <secret>" doesn't match any key either), so the probe must send the raw
    // secret with NO scheme token at all to actually distinguish the two implementations.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // Sanity: the key works with the correct scheme.
    let sane = support::get(&router, "/v0/auth/me", Some(&key)).await;
    assert_eq!(sane.status, 200, "the key itself must be valid: {}", sane.body);

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/v0/auth/me")
        .header("authorization", key.as_str())
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(
        response.status(),
        403,
        "a valid secret with no 'Bearer ' scheme prefix must still be rejected"
    );
}

#[tokio::test]
async fn an_empty_bearer_token_is_bare_403() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);
    let resp = support::get(&router, "/v0/auth/me", Some("")).await;
    assert_eq!(resp.status, 403);
}

#[tokio::test]
async fn a_valid_key_reaches_the_handler_and_gets_the_full_envelope_shape_on_error() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // auth/me succeeds for a bare org-scoped key.
    let resp = support::get(&router, "/v0/auth/me", Some(&key)).await;
    assert_eq!(resp.status, 200);
    let v = resp.json.unwrap();
    assert_eq!(v["organization_id"], org.as_str());
    assert_eq!(v["scope_type"], "organization");
    assert_eq!(v["scope_id"], org.as_str());
    assert!(v.get("pod_id").is_none(), "org-scoped identity omits pod_id");
    assert!(v.get("inbox_id").is_none(), "org-scoped identity omits inbox_id");

    // An application-layer failure with the SAME (valid) credential gets the full envelope, not
    // the bare shape — proving the asymmetry is keyed on which layer rejected, not on status code.
    let missing =
        support::get(&router, "/v0/pods/00000000-0000-0000-0000-000000000000", Some(&key)).await;
    assert_eq!(missing.status, 404);
    let v = missing.json.unwrap();
    assert_eq!(v["code"], "not_found");
    assert!(v.get("name").is_some(), "app-layer failures DO carry name");
}
