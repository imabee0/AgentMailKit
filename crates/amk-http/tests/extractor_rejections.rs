//! Every axum extractor rejection this crate can produce answers the ordinary `validation_error`
//! envelope — 400, `application/json`, one `errors[]` entry — never axum's own `text/plain` body
//! and never a status the error catalog does not contain (415/422/413).
//!
//! `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` is the one source of truth for
//! the target shape; every assertion below traces to a specific line of it. Every test asserts
//! **status, content-type, and the parsed envelope** — a test that checks only `code()` is what
//! let the defects this dispatch fixes ship in the first place.
//!
//! # Known, out-of-scope divergences from the fixture's exact wording
//!
//! Two sub-cases the fixture observed on the reference cannot be reproduced by this dispatch
//! without editing `crate::pagination`, which is outside this dispatch's writable paths (and
//! "improving the query-parameter parsing while in there" is explicitly prohibited). Both are
//! documented in `crate::body`'s own doc and pinned here as the ACTUAL, correctly-scoped
//! behaviour rather than asserted against the fixture's exact wording:
//! - `?limit=-1` and `?limit=` are classified `invalid_type`/`NaN` here, not the reference's
//!   `too_small` — `ListQuery::limit: Option<u64>` cannot represent a negative number at all, so
//!   `-1` fails identically to `abc` at the Rust type level. `?limit=0` does not fail AT ALL: it
//!   is a valid `u64` and `crate::pagination::resolve` already treats it as a legitimate empty-page
//!   request, unchanged by this dispatch.
//! - `?limit=101` returns 200 (correct — no cap is enforced) but echoes `limit:100`, not `101`:
//!   `crate::pagination::MAX_LIMIT` clamps before echoing, which is existing, out-of-scope
//!   behaviour this dispatch does not touch.

mod support;

use amk_http::config::DEFAULT_MAX_BODY_BYTES;
use serde_json::json;

/// Asserts the one envelope shape every malformed-request case in this file must produce:
/// 400, `application/json`, `code: "validation_error"`. Callers go on to assert the specific
/// `errors[0]` shape themselves.
fn assert_validation_envelope(resp: &support::TestResponse, label: &str) {
    assert_eq!(resp.status, 400, "{label}: body={}", resp.body);
    assert_eq!(
        resp.content_type.as_deref(),
        Some("application/json"),
        "{label}: must be application/json, never axum's own text/plain: content_type={:?} body={}",
        resp.content_type,
        resp.body
    );
    assert_eq!(resp.code(), Some("validation_error"), "{label}: body={}", resp.body);
    assert!(
        resp.first_error().is_some(),
        "{label}: validation_error must carry a non-empty errors[]: body={}",
        resp.body
    );
}

// ---- 1. body is not JSON at all --------------------------------------------------------------

#[tokio::test]
async fn a_body_that_is_not_json_at_all_is_400_invalid_format_json_string_empty_path() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    for (label, raw) in [
        ("literal text", &b"not json"[..]),
        ("a NUL byte", &b"\x00"[..]),
    ] {
        let resp = support::send_raw(
            &router,
            "POST",
            "/v0/pods",
            Some(&key),
            Some("application/json"),
            raw,
        )
        .await;
        assert_validation_envelope(&resp, label);
        let issue = resp.first_error().unwrap();
        assert_eq!(issue["code"], "invalid_format", "{label}: {issue}");
        assert_eq!(issue["format"], "json_string", "{label}: {issue}");
        assert_eq!(
            issue["path"],
            json!([]),
            "{label}: a syntax failure has an empty path: {issue}"
        );
        assert_eq!(issue["message"], "Invalid JSON string", "{label}: {issue}");
    }

    // `[INFERRED]` (`crate::body::JsonBody`'s own doc): an empty body sent under an EXPLICIT
    // `Content-Type: application/json` is treated as a genuine syntax failure — the client
    // asserted JSON and then sent none — unlike a missing/mismatched content-type, which
    // synthesizes `{}` (cases 4 and 5 below). This is the one sub-case of this test no fixture
    // line probes directly; it reconciles fixture 27 §2's "no body, no Content-Type at all -> 200"
    // with this exact assigned edge case.
    let resp =
        support::send_raw(&router, "POST", "/v0/pods", Some(&key), Some("application/json"), b"")
            .await;
    assert_validation_envelope(&resp, "empty string with the JSON content-type");
    let issue = resp.first_error().unwrap();
    assert_eq!(issue["code"], "invalid_format");
    assert_eq!(issue["format"], "json_string");
    assert_eq!(issue["path"], json!([]));
}

// ---- 2. body is valid JSON of the wrong type ---------------------------------------------------

#[tokio::test]
async fn a_wrong_typed_field_is_400_invalid_type_naming_the_field() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::post(&router, "/v0/pods", Some(&key), json!({"name": 123})).await;
    assert_validation_envelope(&resp, "name: 123");
    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §2, asserted as the WHOLE
    // object, not merely the code: `{"expected":"string","code":"invalid_type","path":["name"],
    // "message":"Invalid input: expected string, received number"}`.
    assert_eq!(
        resp.first_error().unwrap(),
        &json!({
            "code": "invalid_type",
            "path": ["name"],
            "expected": "string",
            "message": "Invalid input: expected string, received number",
        }),
        "body: {}",
        resp.body
    );
}

// ---- 3. body is valid JSON naming a field the target type does not have -----------------------

#[tokio::test]
async fn an_unknown_field_is_silently_ignored_not_rejected() {
    // `[INFERRED]`: no capture covers this case. `CreatePodRequest` (like every one of this
    // dispatch's 8 body types) derives `serde::Deserialize` WITHOUT `#[serde(deny_unknown_fields)]`
    // — the crate-wide, pre-existing default — so an unrecognized key is silently dropped rather
    // than rejected. This dispatch does not add `deny_unknown_fields` anywhere: doing so would be
    // "improving" behaviour no fixture asked for, and would risk rejecting a future SDK version
    // that adds a field this server does not know about yet.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::post(
        &router,
        "/v0/pods",
        Some(&key),
        json!({"name": "kept", "nosuchfield": "dropped"}),
    )
    .await;
    assert_eq!(resp.status, 200, "an unknown field must not reject the request: {}", resp.body);
    assert_eq!(resp.json.unwrap()["name"], "kept");
}

// ---- 4 & 5. content-type is not enforced; an absent body is `{}` ------------------------------

#[tokio::test]
async fn content_type_text_plain_with_a_json_body_still_creates_the_resource() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §2, verbatim probe.
    let resp =
        support::send_raw(&router, "POST", "/v0/pods", Some(&key), Some("text/plain"), b"{}").await;
    assert_eq!(resp.status, 200, "not 400, not 415: body={}", resp.body);
    assert!(resp.json.unwrap().get("pod_id").is_some(), "the pod must actually be created");
}

#[tokio::test]
async fn no_content_type_header_at_all_with_no_body_still_creates_the_resource() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §2, verbatim probe.
    let resp = support::send(&router, "POST", "/v0/pods", Some(&key), None).await;
    assert_eq!(resp.status, 200, "not 400, not 415: body={}", resp.body);
    assert!(resp.json.unwrap().get("pod_id").is_some(), "the pod must actually be created");
}

#[tokio::test]
async fn no_body_at_all_on_a_post_is_treated_as_an_empty_object() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::send(&router, "POST", "/v0/pods", Some(&key), None).await;
    assert_eq!(resp.status, 200, "body={}", resp.body);
    let v = resp.json.unwrap();
    // `CreatePodRequest.name` is `Option<String>`; an absent body means an absent `name`, so the
    // handler's own `[ASSUMED]` default fires — proof the body was genuinely treated as `{}`
    // rather than merely not rejected.
    assert_eq!(v["name"], "New Pod");
}

// ---- 6. query: limit and ascending -------------------------------------------------------------

#[tokio::test]
async fn limit_abc_is_invalid_type_received_nan() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?limit=abc", Some(&key)).await;
    assert_validation_envelope(&resp, "?limit=abc");
    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1, whole object.
    assert_eq!(
        resp.first_error().unwrap(),
        &json!({
            "code": "invalid_type",
            "path": ["limit"],
            "expected": "number",
            "received": "NaN",
            "message": "Invalid input: expected number, received NaN",
        }),
        "body: {}",
        resp.body
    );
}

#[tokio::test]
async fn limit_negative_one_and_limit_empty_are_rejected_as_invalid_type_not_too_small() {
    // Documented divergence — see this file's own module doc and `crate::body`'s doc on
    // `query_deserialize_error_to_issue`: the reference classifies these as `too_small`
    // (`ListQuery::limit` would need to become a signed/coercing type to reproduce that, which is
    // `crate::pagination`, out of this dispatch's scope). What IS pinned here is what this crate
    // actually does today: still 400, still `validation_error`, still names `limit`, never a
    // silent acceptance of a negative page size.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    for query in ["?limit=-1", "?limit="] {
        let resp = support::get(&router, &format!("/v0/pods{query}"), Some(&key)).await;
        assert_validation_envelope(&resp, query);
        let issue = resp.first_error().unwrap();
        assert_eq!(issue["code"], "invalid_type", "{query}: {issue}");
        assert_eq!(issue["path"], json!(["limit"]), "{query}: {issue}");
    }
}

#[tokio::test]
async fn limit_zero_is_not_a_rejection_it_is_an_empty_page() {
    // Documented divergence, same reasoning as the test above: the reference rejects `?limit=0`
    // as `too_small`; `u64` parses `"0"` successfully, and `crate::pagination::resolve` already
    // treats a supplied `limit` of `0` as "return nothing", not an error — unchanged by this
    // dispatch, pinned here so the gap is visible rather than silently assumed.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?limit=0", Some(&key)).await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
    assert_eq!(resp.json.unwrap()["count"], 0);
}

#[tokio::test]
async fn ascending_maybe_is_invalid_value_with_the_full_stringbool_list() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?ascending=maybe", Some(&key)).await;
    assert_validation_envelope(&resp, "?ascending=maybe");
    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1, whole object.
    assert_eq!(
        resp.first_error().unwrap(),
        &json!({
            "code": "invalid_value",
            "path": ["ascending"],
            "expected": "stringbool",
            "values": [
                "true", "1", "yes", "on", "y", "enabled",
                "false", "0", "no", "off", "n", "disabled",
            ],
            "message": "Invalid option: expected one of \"true\"|\"1\"|\"yes\"|\"on\"|\"y\"|\
                         \"enabled\"|\"false\"|\"0\"|\"no\"|\"off\"|\"n\"|\"disabled\"",
        }),
        "body: {}",
        resp.body
    );
}

// ---- 7. unknown params ignored; no upper cap enforced ------------------------------------------

#[tokio::test]
async fn an_unknown_query_parameter_is_ignored() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?nosuchparam=1", Some(&key)).await;
    assert_eq!(resp.status, 200, "body: {}", resp.body);
}

#[tokio::test]
async fn limit_101_is_accepted_not_rejected_though_the_echoed_value_is_clamped() {
    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1: "no upper cap is
    // enforced" — the 200 half of this is exactly what this dispatch's wrapper must preserve (an
    // over-limit `limit` is a valid `u64`, never an extractor rejection). The echoed VALUE is a
    // documented, out-of-scope divergence — see this file's own module doc.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?limit=101", Some(&key)).await;
    assert_eq!(resp.status, 200, "a limit above 100 must not be REJECTED: {}", resp.body);
    assert_eq!(
        resp.json.unwrap()["limit"],
        100,
        "crate::pagination::MAX_LIMIT clamps the echoed value — out of this dispatch's scope"
    );
}

// ---- 8. page_token's own issue kind changes ----------------------------------------------------

#[tokio::test]
async fn page_token_nul_byte_is_invalid_format_base64url_not_custom() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let resp = support::get(&router, "/v0/pods?page_token=%00", Some(&key)).await;
    assert_validation_envelope(&resp, "?page_token=%00");
    // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §3(e): this USED TO be
    // `{"code":"custom","path":["page_token"],...}` — the defect `error::with_issue` fixed.
    let issue = resp.first_error().unwrap();
    assert_eq!(issue["code"], "invalid_format", "body: {}", resp.body);
    assert_eq!(issue["format"], "base64url", "body: {}", resp.body);
    assert_eq!(issue["path"], json!(["page_token"]), "body: {}", resp.body);
    assert_ne!(issue["code"], "custom", "this is exactly the defect fixture 27 found: {issue}");
}

// ---- 9. every one of the 14 rewritten sites is reachable ---------------------------------------

#[tokio::test]
async fn every_body_and_query_site_answers_the_validation_envelope_not_axums_own_rejection() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let pod = support::seed_pod(&pool, &org).await;
    let inbox = support::seed_inbox(&pool, &org, pod, "extractor-site").await;
    let inbox_segment = inbox.to_path_segment();
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // The 8 `JsonBody` sites — a syntactically invalid body fails EVERY target type identically,
    // regardless of that type's own fields, so one payload proves all 8 without needing a
    // per-type-valid-but-wrong-typed payload.
    let body_sites: &[(&str, &str)] = &[
        ("POST", "/v0/api-keys"),
        ("POST", "/v0/pods/{pod}/api-keys"),
        ("POST", "/v0/inboxes/{inbox}/api-keys"),
        ("POST", "/v0/inboxes"),
        ("POST", "/v0/pods/{pod}/inboxes"),
        ("PATCH", "/v0/inboxes/{inbox}"),
        ("PATCH", "/v0/pods/{pod}/inboxes/{inbox}"),
        ("POST", "/v0/pods"),
    ];
    for (method, template) in body_sites {
        let path = template
            .replace("{pod}", &pod.to_string())
            .replace("{inbox}", &inbox_segment);
        let resp = support::send_raw(
            &router,
            method,
            &path,
            Some(&key),
            Some("application/json"),
            b"not json",
        )
        .await;
        assert_validation_envelope(&resp, &format!("{method} {path}"));
    }

    // The 6 `QueryParams` sites.
    let query_sites: &[&str] = &[
        "/v0/api-keys",
        "/v0/pods/{pod}/api-keys",
        "/v0/inboxes/{inbox}/api-keys",
        "/v0/inboxes",
        "/v0/pods/{pod}/inboxes",
        "/v0/pods",
    ];
    for template in query_sites {
        let base = template
            .replace("{pod}", &pod.to_string())
            .replace("{inbox}", &inbox_segment);
        let path = format!("{base}?limit=abc");
        let resp = support::get(&router, &path, Some(&key)).await;
        assert_validation_envelope(&resp, &format!("GET {path}"));
    }
}

// ---- 10. a field that exists vs one that does not -----------------------------------------------

#[tokio::test]
async fn a_wrong_typed_known_field_and_an_unknown_field_may_legitimately_differ() {
    // Both are recorded rather than forced equal — the reference IS a schema oracle (fixture 27
    // §3(c)), so a known, wrong-typed field and a field the type has never heard of are allowed to
    // answer differently, and this pins what each one actually does.
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    let known_wrong_type =
        support::post(&router, "/v0/pods", Some(&key), json!({"name": 123})).await;
    assert_eq!(
        known_wrong_type.status, 400,
        "a known field, wrong type: {}",
        known_wrong_type.body
    );
    assert_eq!(known_wrong_type.code(), Some("validation_error"));

    let unknown_field =
        support::post(&router, "/v0/pods", Some(&key), json!({"nosuchfield": 123})).await;
    assert_eq!(unknown_field.status, 200, "an unknown field, any type: {}", unknown_field.body);
}

// ---- 11. body size: at the limit, one under, one over -------------------------------------------

/// Deliberately INVALID JSON — fixture 27 §5's own construction — so an oversized request that
/// slips under a raised or removed limit still cannot create anything; the assertion is about
/// STATUS and SHAPE, not about a payload that would otherwise succeed.
fn invalid_json_of_len(len: usize) -> Vec<u8> {
    // A run of `x` bytes is syntactically invalid JSON at any length (not `{`, not a valid
    // literal), and is exactly `len` bytes — no wrapping punctuation to account for.
    vec![b'x'; len]
}

#[tokio::test]
async fn body_size_at_the_limit_one_under_and_one_over_is_always_400_never_413() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let org = support::seed_org(&pool).await;
    let key = support::org_key(&pool, &org).await;
    let router = support::test_router(pool);

    // `count` is the PAGE's item count (`amk_types::page::list_response!`), capped at `limit` —
    // a generous limit is required here so growth across the loop below is actually visible
    // rather than masked by a cap both counts would hit identically.
    let before = support::get(&router, "/v0/pods?limit=50", Some(&key)).await;
    let count_before = before.json.unwrap()["count"].as_u64().unwrap();

    for (label, len) in [
        ("one under the limit", DEFAULT_MAX_BODY_BYTES - 1),
        ("exactly at the limit", DEFAULT_MAX_BODY_BYTES),
        ("one over the limit", DEFAULT_MAX_BODY_BYTES + 1),
    ] {
        let resp = support::send_raw(
            &router,
            "POST",
            "/v0/pods",
            Some(&key),
            Some("application/json"),
            &invalid_json_of_len(len),
        )
        .await;
        assert_eq!(resp.status, 400, "{label} ({len} bytes): never 413: body={}", resp.body);
        assert_ne!(resp.status, 413, "{label}: the error catalog has no code for 413");
        assert_eq!(
            resp.content_type.as_deref(),
            Some("application/json"),
            "{label}: never text/plain: {:?}",
            resp.content_type
        );
        let issue = resp.first_error().unwrap();
        assert_eq!(issue["code"], "invalid_format", "{label}: {issue}");
        assert_eq!(issue["format"], "json_string", "{label}: {issue}");
    }

    let after = support::get(&router, "/v0/pods?limit=50", Some(&key)).await;
    let count_after = after.json.unwrap()["count"].as_u64().unwrap();
    assert_eq!(
        count_before, count_after,
        "an invalid-JSON body at any size must create nothing"
    );
}
