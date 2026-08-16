//! Divergence 2 (`reference/fixtures/25-p1-gate-conformance.txt`): the error envelope omitted
//! `fix`. `ErrorEnvelope` always carried the field; nothing gave every code a value. This file
//! pins the PRESENT half across three call sites that reach `fix` through different paths — a
//! route that never matched (`lib.rs`'s explicit override), a resource lookup that goes through
//! `amk_core::scope::ScopeDenial` (already sets its own `fix` upstream — this proves the central
//! backfill does not clobber it), and a permission denial that goes through neither (this crate's
//! own central backfill is the only thing that could have set it) — so no single call site could
//! make this pass by accident.
//!
//! The ABSENT half — a 401/403 gateway body must never carry `fix` — is already pinned by
//! `tests/auth.rs`'s full-body equality assertions (`assert_eq!(v, json!({"message": …}))`, which
//! by construction rejects any body carrying an extra key); not duplicated here.

mod support;

#[tokio::test]
async fn fix_is_present_on_the_not_found_envelope_for_an_unmatched_route() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/this-route-does-not-exist", None).await;
    assert_eq!(resp.status, 404);
    let v = resp.json.expect("must be the JSON envelope");
    let fix = v.get("fix").and_then(|f| f.as_str());
    assert!(
        fix.is_some_and(|s| !s.is_empty()),
        "fix must be present and non-empty on an app-layer error: {v}"
    );
}

#[tokio::test]
async fn fix_is_present_on_a_not_found_resource_lookup() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // A well-formed but absent pod id: reaches the handler, then `ScopeFilter::not_found`, which
    // constructs its envelope via `amk_core::scope::ScopeDenial::into_envelope` — a DIFFERENT
    // construction path from `AppError::new`, already setting its own `fix` (`amk_core::scope`'s
    // own `MASK_FIX`) before this crate's central backfill ever runs. Checking presence alone
    // would pass even if the backfill unconditionally overwrote every `fix` — the substring below
    // is `MASK_FIX`'s own text, not `fix_for(NotFound)`'s generic default here in this crate, so
    // this only passes if the resource-aware value survived untouched.
    let resp =
        support::get(&router, "/v0/pods/00000000-0000-4000-8000-000000000000", Some(&key)).await;
    assert_eq!(resp.status, 404);
    let v = resp.json.expect("must be the JSON envelope");
    let fix = v.get("fix").and_then(|f| f.as_str());
    assert!(fix.is_some_and(|s| !s.is_empty()), "fix must be present: {v}");
    assert!(
        fix.is_some_and(|s| s.contains("Visibility depends on the credential's scope")),
        "fix must be ScopeDenial's own resource-aware text, not the generic default \
         this crate's central backfill would supply if it clobbered an already-set value: {v}"
    );
}

#[tokio::test]
async fn fix_is_present_on_a_missing_permission_denial() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    // Present-but-empty permissions grants nothing (the NULL-vs-`{}` distinction) — this key can
    // authenticate but `pod_read` denies it, `amk_core::permissions::Denial::MissingPermission`,
    // which reaches `AppError` through neither `AppError::new` nor `ScopeDenial`: the ONLY thing
    // that can have set `fix` on this response is this crate's own central backfill.
    let key = support::mint_key(
        &pool,
        &org,
        None,
        None,
        Some(amk_types::api_key::ApiKeyPermissions::default()),
    )
    .await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods", Some(&key)).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.code(), Some("missing_permission"));
    let v = resp.json.expect("must be the JSON envelope");
    let fix = v.get("fix").and_then(|f| f.as_str());
    assert!(fix.is_some_and(|s| !s.is_empty()), "fix must be present: {v}");
}
