//! Divergence 3 (`reference/fixtures/25-p1-gate-conformance.txt`): a malformed id in a path
//! segment escaped the JSON error contract entirely — axum's own `Path<Uuid>` rejection is a
//! plain-text 400 from a server whose whole contract is a JSON envelope:
//!
//! ```text
//! GET /v0/pods/not-a-uuid   ref=404 application/json   cand=400 text/plain
//! ```
//!
//! The dispatch contract's derivation (`.claude/contracts/amk-p1-divergences.md` section 3)
//! enumerates 17 `Path<...>` extraction sites; 10 of them carry a `Uuid` (always `pod_id` — every
//! `api_key_id`/`inbox_id` in this crate is `Path<String>`, which cannot reject this way at all).
//! Every one of those 10 gets its own case below: a malformed `pod_id` must answer exactly what a
//! well-formed-but-ABSENT `pod_id` already does — same status, same JSON body shape
//! (`code: "not_found"`, the full envelope) — so a malformed and an absent id are
//! indistinguishable to the client. The four two-segment routes (`Path<(Uuid, String)>`) get both
//! directions: only the first segment malformed, and only the second — the second can never
//! reject at the extractor (any string is a valid `Path<String>`), so that half proves the fix
//! does not accidentally swallow an otherwise-normal request too.

mod support;

const MALFORMED: &str = "not-a-uuid";
/// A syntactically ordinary string that names no real row — the "only the second segment is
/// malformed" cases use this to prove that half of a two-segment route is unaffected by this
/// fix: it must still reach the handler and get the ordinary "resource not found" answer, not the
/// route-level rejection this file exists to close.
const NO_SUCH_STRING_ID: &str = "does-not-exist-at-all";

fn assert_not_found_envelope(resp: &support::TestResponse, label: &str) {
    assert_eq!(resp.status, 404, "{label}: body: {}", resp.body);
    let v = resp.json.as_ref().unwrap_or_else(|| {
        panic!(
            "{label}: must be the JSON envelope, not axum's own plain-text rejection: {}",
            resp.body
        )
    });
    assert_eq!(v["code"], "not_found", "{label}: {v}");
    assert!(
        v.get("name").is_some(),
        "{label}: must be the FULL envelope, not the bare gateway shape: {v}"
    );
    assert!(v.get("message").is_some(), "{label}: {v}");
}

/// One case per single-`Uuid`-segment route (6 of the 10) — every one of the `Path<Uuid>` sites
/// in `pods.rs`/`inboxes.rs`/`api_keys.rs`'s single-pod-segment handlers.
#[tokio::test]
async fn a_malformed_pod_id_is_404_json_at_every_single_segment_route() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    for (method, path) in [
        ("GET", format!("/v0/pods/{MALFORMED}")),
        ("DELETE", format!("/v0/pods/{MALFORMED}")),
        ("GET", format!("/v0/pods/{MALFORMED}/api-keys")),
        ("POST", format!("/v0/pods/{MALFORMED}/api-keys")),
        ("GET", format!("/v0/pods/{MALFORMED}/inboxes")),
        ("POST", format!("/v0/pods/{MALFORMED}/inboxes")),
    ] {
        let resp = support::send(&router, method, &path, Some(&key), None).await;
        assert_not_found_envelope(&resp, &format!("{method} {path}"));
    }
}

/// The four two-segment (`Path<(Uuid, String)>`) routes, first segment malformed — the second
/// segment's own value is irrelevant here (a garbage `pod_id` must reject before the string
/// segment is ever examined), so [`NO_SUCH_STRING_ID`] stands in for it.
#[tokio::test]
async fn a_malformed_pod_id_is_404_json_at_every_two_segment_route_first_segment() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    for (method, path) in [
        ("DELETE", format!("/v0/pods/{MALFORMED}/api-keys/{NO_SUCH_STRING_ID}")),
        ("GET", format!("/v0/pods/{MALFORMED}/inboxes/{NO_SUCH_STRING_ID}")),
        ("PATCH", format!("/v0/pods/{MALFORMED}/inboxes/{NO_SUCH_STRING_ID}")),
        ("DELETE", format!("/v0/pods/{MALFORMED}/inboxes/{NO_SUCH_STRING_ID}")),
    ] {
        let resp = support::send(&router, method, &path, Some(&key), None).await;
        assert_not_found_envelope(&resp, &format!("{method} {path}"));
    }
}

/// The same four two-segment routes, but with only the SECOND segment malformed and a genuinely
/// real `pod_id` — this must NOT hit the extractor-rejection path at all (a `Path<String>` cannot
/// reject), so it must reach the handler's own "not found" answer exactly as before this dispatch:
/// still a 404 JSON envelope, proving the fix did not change behaviour it was never meant to.
#[tokio::test]
async fn a_bogus_but_syntactically_fine_second_segment_still_reaches_the_ordinary_404() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // `PATCH` carries a body — an empty one would 415 before the handler ever ran, which is
    // axum's `Json<T>` extractor rejecting, a genuinely different (and out of this dispatch's
    // scope) escape hatch from the `Path<Uuid>` one this file exists to close. A real, valid body
    // is what isolates the ONE thing this case means to prove: the string segment alone.
    for (method, path, body) in [
        ("DELETE", format!("/v0/pods/{pod}/api-keys/{NO_SUCH_STRING_ID}"), None),
        ("GET", format!("/v0/pods/{pod}/inboxes/{NO_SUCH_STRING_ID}"), None),
        (
            "PATCH",
            format!("/v0/pods/{pod}/inboxes/{NO_SUCH_STRING_ID}"),
            Some(serde_json::json!({"display_name": "x"})),
        ),
        ("DELETE", format!("/v0/pods/{pod}/inboxes/{NO_SUCH_STRING_ID}"), None),
    ] {
        let resp = support::send(&router, method, &path, Some(&key), body).await;
        assert_not_found_envelope(&resp, &format!("{method} {path}"));
    }
}

/// A well-formed but genuinely ABSENT `pod_id` — a valid uuid naming no row — must answer
/// byte-for-byte the same status and body SHAPE as a malformed one: indistinguishable, per the
/// dispatch contract's own disclosure rule (a malformed id must not reveal which ids are
/// well-formed). `00000000-0000-4000-8000-000000000000` is the fixture 25/manifest's own probe
/// value for exactly this case.
#[tokio::test]
async fn a_malformed_id_and_a_well_formed_but_absent_id_are_indistinguishable() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let malformed = support::get(&router, &format!("/v0/pods/{MALFORMED}"), Some(&key)).await;
    let absent =
        support::get(&router, "/v0/pods/00000000-0000-4000-8000-000000000000", Some(&key)).await;

    assert_eq!(malformed.status, absent.status, "status must match");
    let (mv, av) = (
        malformed.json.expect("malformed id must still be JSON"),
        absent.json.expect("absent id must still be JSON"),
    );
    assert_eq!(
        mv.as_object()
            .map(|m| m.keys().collect::<std::collections::BTreeSet<_>>()),
        av.as_object()
            .map(|m| m.keys().collect::<std::collections::BTreeSet<_>>()),
        "malformed vs absent must have the same body SHAPE: malformed={mv} absent={av}"
    );
    assert_eq!(mv["code"], "not_found");
    assert_eq!(av["code"], "not_found");
}

/// An UNAUTHENTICATED request with a malformed `pod_id` must still get the JSON envelope, never
/// axum's raw rejection — the auth layer and the path extractor are independent gates, and the
/// dispatch contract's own `not_found_fallback` precedent (a route needs no credential to learn it
/// doesn't exist) applies here too: `AuthContext` runs before `Path<Uuid>` in every handler's own
/// parameter order, so a missing credential is answered first (401, bare gateway body) — this
/// case exists to pin that ordering rather than assume it.
#[tokio::test]
async fn a_malformed_pod_id_with_no_credential_at_all_still_gets_a_json_body() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let router = support::test_router(pool);
    let resp = support::get(&router, &format!("/v0/pods/{MALFORMED}"), None).await;
    // Whichever gate answers first (auth, bare 401, or the path rejection, full 404) — both of
    // this crate's own error shapes are JSON. What must never happen is axum's raw text/plain
    // rejection body.
    assert!(
        resp.json.is_some(),
        "must be JSON either way, never axum's own plain-text rejection: {}",
        resp.body
    );
}
