//! `[SPEC:fixture 05-error-catalog.http]`/contract: unknown path or wrong method both answer the
//! full envelope, `code: "not_found"`, HTTP 404. There is no 405 anywhere in this crate.

mod support;

#[tokio::test]
async fn an_unknown_path_is_404_with_the_full_envelope() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    // No credential at all — the fallback must not require auth to say a route doesn't exist.
    let resp = support::get(&router, "/v0/this-route-does-not-exist", None).await;
    assert_eq!(resp.status, 404, "body: {}", resp.body);
    let v = resp
        .json
        .expect("must be the JSON envelope, not axum's default body");
    assert_eq!(v["code"], "not_found");
    assert!(v.get("name").is_some(), "the FULL envelope, not the bare auth-layer shape");
}

#[tokio::test]
async fn a_matched_path_with_the_wrong_method_is_404_never_405() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // /v0/auth/me exists only as GET; PUT is never registered on any route in this dispatch.
    let resp = support::send(&router, "PUT", "/v0/auth/me", Some(&key), None).await;
    assert_eq!(resp.status, 404, "must never be 405: body: {}", resp.body);
    assert_eq!(resp.code(), Some("not_found"));

    // /v0/pods exists as GET+POST; DELETE at the collection level is not registered.
    let resp = support::delete(&router, "/v0/pods", Some(&key)).await;
    assert_eq!(resp.status, 404, "must never be 405: body: {}", resp.body);
    assert_eq!(resp.code(), Some("not_found"));
}

#[tokio::test]
async fn organizations_has_no_post_or_patch() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::post(&router, "/v0/organizations", Some(&key), serde_json::json!({})).await;
    assert_eq!(resp.status, 404, "GET /v0/organizations is the only organizations operation");
}
