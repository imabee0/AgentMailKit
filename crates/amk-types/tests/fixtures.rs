//! Golden-fixture regression tests.
//!
//! The `reference/fixtures/` captures are the contract. Until now they were cited in doc comments
//! and their values hand-transcribed into unit tests — which means a mistranscription, or a
//! fixture later corrected against the live API, would never be noticed. These tests read the
//! fixture files themselves, so the evidence and the code cannot drift apart silently.
//!
//! The captures are working notes: some JSON is verbatim on one line, some was hand-wrapped for
//! reading and is therefore not parseable. We assert against the verbatim lines and, for the
//! wrapped ones, against the facts stated alongside them (status codes, error codes). Pretending
//! to parse prose would produce a test that passes for the wrong reason.

use std::{fs, path::PathBuf};

use amk_types::{
    error::GatewayError, message::SendMessageResponse, page::Cursor, pod::Identity, ErrorCode,
};
use serde_json::Value;

fn fixture(name: &str) -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "reference",
        "fixtures",
        name,
    ]
    .iter()
    .collect();
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {name} unreadable at {}: {e}", path.display()))
}

/// Every complete JSON object that appears on a single line, wherever it starts. Captures are
/// often annotated (`-> 403  {"message":"Forbidden"}`), so anchoring at the start of the line
/// would silently skip the bodies we most want to assert on. Hand-wrapped JSON is excluded by
/// construction: a wrapped object does not parse.
fn verbatim_json_lines(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        for (i, _) in line.match_indices('{') {
            // A stream deserializer stops at the end of the first complete value, so trailing
            // prose after the object does not defeat the parse.
            let mut stream = serde_json::Deserializer::from_str(&line[i..]).into_iter::<Value>();
            if let Some(Ok(v)) = stream.next() {
                if v.is_object() {
                    out.push(v);
                }
                break;
            }
        }
    }
    out
}

/// Round-tripping through our type must reproduce the captured JSON **exactly** — no field
/// dropped, none invented. Comparing `Value`s rather than strings ignores key order, which the
/// wire does not guarantee, while still catching every structural difference.
fn assert_round_trips<T>(captured: &Value, what: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let typed: T = serde_json::from_value(captured.clone()).unwrap_or_else(|e| {
        panic!("{what}: our type cannot parse the live capture: {e}\n{captured:#}")
    });
    let ours = serde_json::to_value(&typed).expect("serialize");
    assert_eq!(
        &ours, captured,
        "{what}: our serialization diverges from the live capture.\n  ours: {ours:#}\n  live: {captured:#}"
    );
}

#[test]
fn identity_round_trips_fixture_01() {
    let text = fixture("01-auth-me.http");
    let bodies = verbatim_json_lines(&text);
    let captured = bodies
        .iter()
        .find(|v| v.get("scope_type").is_some())
        .expect("01-auth-me.http no longer contains a verbatim Identity body");

    assert_round_trips::<Identity>(captured, "auth/me Identity");

    // The scope facts the fixture establishes: an org-scoped key reports scope_id == organization_id.
    let id: Identity = serde_json::from_value(captured.clone()).unwrap();
    assert_eq!(
        id.scope_id,
        id.organization_id.as_str(),
        "org-scoped key must report scope_id == organization_id"
    );
    assert!(
        id.pod_id.is_none() && id.inbox_id.is_none(),
        "org scope carries no pod/inbox id"
    );
}

#[test]
fn send_response_round_trips_fixture_03() {
    let text = fixture("03-id-formats.http");
    let captured = verbatim_json_lines(&text)
        .into_iter()
        .find(|v| v.get("message_id").is_some() && v.get("thread_id").is_some())
        .expect("03-id-formats.http no longer contains a verbatim SendMessageResponse");

    assert_round_trips::<SendMessageResponse>(&captured, "SendMessageResponse");

    // The finding this fixture exists for: message_id is the SES angle-bracket RFC 5322 value,
    // header-derived rather than minted by AgentMail.
    let resp: SendMessageResponse = serde_json::from_value(captured).unwrap();
    let raw = resp.message_id.as_str();
    assert!(
        raw.starts_with('<') && raw.ends_with('>'),
        "message_id must keep its angle brackets: {raw}"
    );
    assert!(raw.contains('@'), "message_id must be an addr-spec: {raw}");
    assert!(resp.message_id.is_bracketed(), "is_bracketed must agree with the capture");

    // It travels in a path segment, so the characters that would break routing must survive a
    // round trip through our encoding.
    let encoded = resp.message_id.to_path_segment();
    for c in ['<', '>', '@'] {
        assert!(!encoded.contains(c), "{c} must be percent-encoded in a path segment: {encoded}");
    }
    assert_eq!(
        amk_types::MessageId::from_path_segment(&encoded).expect("decodes"),
        resp.message_id,
        "path-segment encoding must round-trip"
    );
}

#[test]
fn auth_layer_errors_are_bare_gateway_bodies_fixture_05() {
    let text = fixture("05-error-catalog.http");

    // The asymmetry this fixture proved: auth-layer failures return a bare {"message": ...} with
    // no name/code/fix/docs — even for a WELL-FORMED but invalid am_ key.
    let bare: Vec<Value> = verbatim_json_lines(&text)
        .into_iter()
        .filter(|v| v.get("message").is_some() && v.get("code").is_none())
        .collect();
    assert!(!bare.is_empty(), "05 no longer contains a verbatim bare gateway body");

    for captured in &bare {
        assert_round_trips::<GatewayError>(captured, "bare gateway error");
        let obj = captured.as_object().unwrap();
        assert_eq!(obj.len(), 1, "the gateway body carries ONLY `message`: {captured}");
        for forbidden in ["name", "code", "fix", "docs", "errors", "suggestions"] {
            assert!(!obj.contains_key(forbidden), "auth-layer body must not carry `{forbidden}`");
        }
    }

    let messages: Vec<&str> = bare.iter().filter_map(|v| v["message"].as_str()).collect();
    assert!(messages.contains(&"Forbidden"), "expected the 403 body");
    assert!(messages.contains(&"Unauthorized"), "expected the 401 body");
}

#[test]
fn error_code_statuses_match_fixture_05() {
    let text = fixture("05-error-catalog.http");

    // Pair each `-> NNN` with the FIRST error code stated at or after it, then stop until the next
    // status marker. Only the first is the envelope's own `code`; later ones belong to nested
    // `errors[]` entries, whose `code` is a different vocabulary entirely (zod-style `custom`,
    // `invalid_type`) and deliberately typed as a plain String rather than an ErrorCode.
    let mut pairs: Vec<(String, u16)> = Vec::new();
    let mut pending: Option<u16> = None;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("-> ") {
            if let Ok(status) = rest.split_whitespace().next().unwrap_or("").parse::<u16>() {
                pending = Some(status);
            }
        }
        if let (Some(status), Some(idx)) = (pending, line.find("\"code\":\"")) {
            let tail = &line[idx + 8..];
            if let Some(end) = tail.find('"') {
                pairs.push((tail[..end].to_string(), status));
                pending = None;
            }
        }
    }

    assert!(
        pairs.len() >= 3,
        "expected at least already_exists/not_found/validation_error in 05; found {pairs:?}"
    );

    for (code_str, status) in &pairs {
        let code: ErrorCode = serde_json::from_value(Value::String(code_str.clone()))
            .unwrap_or_else(|e| {
                panic!("fixture names code `{code_str}` which ErrorCode lacks: {e}")
            });
        assert_eq!(
            code.status(),
            *status,
            "`{code_str}`: live capture says HTTP {status}, our table says {}",
            code.status()
        );
    }

    // The specific correction this fixture forced — both prior guesses (docs 409, SDK 422) were wrong.
    assert!(
        pairs
            .iter()
            .any(|(c, s)| c == "already_exists" && *s == 403),
        "05 must still establish already_exists at 403; found {pairs:?}"
    );
}

/// `errors[].code` is NOT an `ErrorCode`. The live capture carries `"code":"custom"`, a zod-style
/// issue kind from the validation layer, which shares a field name with the envelope's code but
/// nothing else. Typing it as `ErrorCode` would reject valid responses; this pins it open.
#[test]
fn validation_issue_code_is_a_separate_vocabulary_fixture_05() {
    let text = fixture("05-error-catalog.http");
    assert!(
        text.contains("\"code\":\"custom\""),
        "05 must still show a nested errors[] code outside the ErrorCode catalog"
    );

    let issue: amk_types::ValidationIssue = serde_json::from_str(
        r#"{"code":"custom","path":[],"message":"to, cc, or bcc must be specified"}"#,
    )
    .expect("ValidationIssue must accept a code that is not in the ErrorCode catalog");
    assert_eq!(issue.code, "custom");
    assert!(issue.path.is_empty(), "whole-body rules carry an empty path array");

    // The same string must NOT parse as an envelope code — that is the distinction being pinned.
    assert!(
        serde_json::from_value::<ErrorCode>(Value::String("custom".into())).is_err(),
        "`custom` must not be an ErrorCode"
    );
}

#[test]
fn cursor_matches_fixture_04_keyset() {
    let text = fixture("04-pagination.http");

    // The decoded cursor is pretty-printed across lines in the capture; take the block after the
    // line announcing it.
    let start = text
        .find("decodes to:")
        .expect("04 no longer shows the decoded cursor");
    let block = &text[start..];
    let open = block.find('{').expect("no cursor object");
    let close = block.find('}').expect("unterminated cursor object");
    let captured: Value =
        serde_json::from_str(&block[open..=close]).expect("decoded cursor block is not valid JSON");

    let keys: Vec<&str> = captured
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["inbox_id", "message_id", "timestamp"],
        "the keyset is (message_id, inbox_id, timestamp) — a changed key set changes pagination semantics"
    );

    // Our Cursor must carry that keyset through an encode/decode round trip unchanged: the scheme
    // is base64(JSON keyset), so a token we mint is readable as the same object.
    let mut cursor = Cursor::new();
    for (k, v) in captured.as_object().unwrap() {
        cursor = cursor.with(k, v.clone());
    }
    let decoded = Cursor::decode(&cursor.encode()).expect("our own token must decode");
    assert_eq!(
        serde_json::to_value(&decoded.0).unwrap(),
        captured,
        "encode/decode must preserve the keyset exactly"
    );
    assert_eq!(decoded.get_str("inbox_id"), Some("amk-probe@agentmail.to"));

    // A token is opaque to clients, but a tampered one must not silently become an empty cursor.
    assert!(Cursor::decode("not-base64!!").is_err(), "malformed token must be rejected");
    assert!(Cursor::decode("bnVsbA==").is_err(), "base64 of `null` is not an object cursor");
}

/// The fixture set is the regression suite; a capture nothing asserts against is a gap. This test
/// fails when a fixture is added without being wired in, so the gap is visible rather than assumed.
#[test]
fn every_fixture_is_either_asserted_or_explicitly_deferred() {
    // Asserted by the tests above.
    const ASSERTED: &[&str] = &[
        "01-auth-me.http",
        "03-id-formats.http",
        "04-pagination.http",
        "05-error-catalog.http",
    ];
    // Deferred WITH a reason and the phase that closes it. Not a parking lot: each entry names the
    // crate that will assert it, and this list may only shrink.
    const DEFERRED: &[(&str, &str)] = &[
        ("00-probe-teardown.txt", "operational ledger, not a wire shape"),
        ("06-download-url-expiry.txt", "amk-store signed downloads, P2"),
        ("07-webhook-retry-curve.txt", "amk-events retry engine, P4"),
        ("09-event-payloads.txt", "amk-events payload shapes, P4"),
        ("09b-unauthenticated-variant.txt", "amk-ingest labelling + list exclusion, P2"),
        ("10-dkim-keys.txt", "migration evidence, P6"),
        ("10b-dkim-extraction.txt", "migration evidence, P6"),
        ("11-cp-smtp-relay.txt", "migration evidence, P6"),
        ("12-stalwart-dependents.txt", "migration evidence, P6"),
        ("13-source-ip-echo.txt", "deployment evidence, P6"),
        ("14-imap-crate-survey.txt", "survey, no wire shape"),
        ("15-compile-spike.txt", "dependency pins, asserted by the build itself"),
        ("16-threading-matrix", "amk-core threading rules, P2"),
        ("17-message-complained.txt", "amk-events complaint payload, P4"),
        ("C1-domain-shape.txt", "amk-types domain shapes, P5"),
    ];

    let dir: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "reference",
        "fixtures",
    ]
    .iter()
    .collect();
    let mut unaccounted = Vec::new();
    for entry in fs::read_dir(&dir).expect("reference/fixtures must exist") {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let known = ASSERTED.contains(&name.as_str()) || DEFERRED.iter().any(|(n, _)| *n == name);
        if !known {
            unaccounted.push(name);
        }
    }
    unaccounted.sort();
    assert!(
        unaccounted.is_empty(),
        "fixtures with no assertion and no recorded deferral: {unaccounted:?}\n\
         Wire it into a test, or add it to DEFERRED with the crate and phase that will."
    );
}
