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

/// Fixture 18 settled a question the plan had left open ("is `Foo@x.com` the same inbox as
/// `foo@x.com`? decide, then test") — and the answer contradicted what the first implementation
/// assumed. This reads the capture so the decision stays tied to the observation.
#[test]
fn inbox_case_normalization_matches_fixture_18() {
    let text = fixture("18-inbox-case-normalization.txt");

    // The create response is verbatim: a mixed-case username came back lowercased.
    let created = verbatim_json_lines(&text)
        .into_iter()
        .find(|v| v.get("inbox_id").is_some() && v.get("email").is_some())
        .expect("18 no longer contains the verbatim create response");
    let stored = created["inbox_id"].as_str().unwrap();
    assert_eq!(stored, stored.to_ascii_lowercase(), "the stored id must be lowercase");
    assert_eq!(created["email"].as_str().unwrap(), stored, "email and inbox_id agree");
    assert!(
        text.contains("\"username\":\"AmkCase\""),
        "18 must still show that the REQUESTED username was mixed-case"
    );

    // Our type must resolve every case variant the fixture records as 200 to that stored id.
    let stored_id = amk_types::InboxId::new(stored);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("GET /v0/inboxes/") {
            if !rest.contains("-> 200") {
                continue;
            }
            let seg = rest.split_whitespace().next().unwrap();
            let decoded = amk_types::InboxId::from_path_segment(seg).expect("decodes");
            assert!(
                stored_id.eq_normalized(&decoded),
                "fixture records {seg} returning 200, so it must resolve to {stored}"
            );
        }
    }
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

/// Fixture 21 closed Register C3 by **reversing** the behaviour we had shipped: a bare,
/// unbracketed `In-Reply-To` does join the referenced message's thread. The decisive evidence is
/// not the shared `thread_id` — that could be explained by any number of rules — but the field
/// asymmetry inside a single captured message: BARE's raw `headers.In-Reply-To` comes back
/// unbracketed exactly as sent, while its parsed, API-level `in_reply_to` comes back bracketed.
/// Upstream normalises the parsed value before matching.
///
/// That normalisation is a `MessageId` fact, which is why it is pinned here rather than left to
/// the crate that consumes it: `amk_core::threading` matches on `MessageId::bracketed`, so if this
/// assertion and the live capture ever disagree, threading is wrong at its input.
#[test]
fn bare_in_reply_to_is_rebracketed_fixture_21() {
    let text = fixture("21-unbracketed-in-reply-to.txt");
    let msgs: Vec<Value> = verbatim_json_lines(&text)
        .into_iter()
        .filter(|v| v.get("message_id").is_some() && v.get("inbox_id").is_some())
        .collect();
    assert_eq!(
        msgs.len(),
        3,
        "21 must still carry the three verbatim messages (ROOT, BARE, CONTROL)"
    );

    let header =
        |m: &Value| -> Option<String> { m["headers"]["In-Reply-To"].as_str().map(str::to_owned) };
    let root = msgs
        .iter()
        .find(|m| header(m).is_none())
        .expect("ROOT sends no In-Reply-To");
    let bare = msgs
        .iter()
        .find(|m| header(m).is_some_and(|h| !h.starts_with('<')))
        .expect("BARE sends an unbracketed In-Reply-To");
    let ctrl = msgs
        .iter()
        .find(|m| header(m).is_some_and(|h| h.starts_with('<')))
        .expect("CONTROL sends a bracketed In-Reply-To");

    // CONTROL joining is what makes the probe self-validating: had it not joined, BARE's result
    // would mean nothing. Assert the validity condition, not just the finding.
    assert_eq!(
        ctrl["thread_id"], root["thread_id"],
        "the probe is only valid if CONTROL joined ROOT's thread"
    );
    assert_eq!(
        bare["thread_id"], root["thread_id"],
        "C3: a bare In-Reply-To must join the referenced thread"
    );

    // The mechanism, in one message: raw header bare, parsed field bracketed.
    let raw = header(bare).unwrap();
    let parsed = bare["in_reply_to"].as_str().expect("BARE has in_reply_to");
    assert!(!raw.contains('<'), "BARE's raw header must stay as sent: {raw}");
    assert_eq!(
        parsed,
        root["message_id"].as_str().unwrap(),
        "the parsed value must resolve to ROOT's stored Message-ID"
    );
    assert_eq!(
        amk_types::MessageId::bracketed(&raw).as_str(),
        parsed,
        "MessageId::bracketed must reproduce upstream's normalisation of a bare addr-spec"
    );

    // ...and it must be idempotent, since CONTROL's already-bracketed value goes through the same
    // path. Double-bracketing would match nothing.
    let ctrl_raw = header(ctrl).unwrap();
    assert_eq!(
        amk_types::MessageId::bracketed(&ctrl_raw).as_str(),
        ctrl["in_reply_to"].as_str().unwrap(),
        "normalising an already-bracketed value must be a no-op"
    );
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
        "18-inbox-case-normalization.txt",
        "19-message-label-patch-gate.txt",
        "21-unbracketed-in-reply-to.txt",
        "22-org-mount-and-delete-semantics.txt",
        "23-inbox-defaults-and-key-shape.txt",
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
        ("20-search-and-label-precedence.txt", "amk-core label access modes, P1"),
        // Pre-authorised: the amk-cli dispatch produces this one, and this tripwire fires the
        // moment it lands while `amk-types` is frozen — an implementer that cannot pass the check
        // and cannot fix the cause. Not a wire shape: it is the P0 gate's live transcript, and its
        // assertion is plan-ledger's `p0-gate-sdk-authme`, which reads the file.
        ("24-p0-gate-sdk-authme.txt", "P0 gate transcript, asserted by plan-ledger"),
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

/// Fixture 19 probed each candidate label against a live message PATCH. It overturned a reading
/// two reviewers and the orchestrator had all agreed on from the OpenAPI descriptions, so the
/// constants are pinned to the observation rather than to the prose.
#[test]
fn system_and_restricted_labels_match_fixture_19() {
    use amk_types::message::labels;
    let text = fixture("19-message-label-patch-gate.txt");

    // Parse the observed table: "  <label>  -> 400 ..." rejected, "-> 200" accepted.
    let (mut rejected, mut accepted) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let t = line.trim();
        let Some((name, rest)) = t.split_once("->") else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.contains(' ') || name.contains('/') {
            continue;
        }
        if rest.trim_start().starts_with("400") {
            rejected.push(name.to_string());
        } else if rest.trim_start().starts_with("200") {
            accepted.push(name.to_string());
        }
    }
    assert!(
        rejected.len() >= 4,
        "19 must still record the rejected labels; got {rejected:?}"
    );
    assert!(
        accepted.len() >= 5,
        "19 must still record the accepted labels; got {accepted:?}"
    );

    // Every label the live API rejected is system; every one it accepted is not.
    for name in &rejected {
        assert!(labels::is_system(name), "{name} was rejected live, so it must be system");
    }
    for name in &accepted {
        assert!(!labels::is_system(name), "{name} was accepted live, so it must NOT be system");
    }
    assert_eq!(rejected.len(), labels::SYSTEM.len(), "SYSTEM must be exactly the observed set");

    // The trap this fixture exists to document: the two axes are independent.
    assert!(labels::is_restricted(labels::SPAM) && !labels::is_system(labels::SPAM));
    assert!(labels::is_system(labels::SCHEDULED) && !labels::is_restricted(labels::SCHEDULED));
    assert!(!labels::is_system(labels::UNREAD) && !labels::is_restricted(labels::UNREAD));
}

/// Fixture 22 measured two things `openapi.json` does not contain and one it contains wrongly.
/// This asserts the one that is a wire shape this crate owns: the status of `cannot_delete`.
///
/// It reads the capture rather than quoting it. The mapping was **403** until this fixture landed,
/// derived from the docs page — `cannot_delete` appears zero times in `reference/openapi.json` —
/// and the live API answered 409. A comment saying "409" would not have failed when the code said
/// 403; this does.
#[test]
fn cannot_delete_is_409_per_fixture_22() {
    let text = fixture("22-org-mount-and-delete-semantics.txt");

    // Find the DELETE that was refused, and take the status from the fixture's own arrow.
    let line = text
        .lines()
        .find(|l| l.starts_with("DELETE /v0/pods/") && l.contains("-> "))
        .expect("22 no longer records a refused pod DELETE");
    let observed: u16 = line
        .rsplit("-> ")
        .next()
        .unwrap()
        .trim()
        .parse()
        .expect("the refused DELETE line must end in a status code");

    // And the code from the verbatim envelope, so the two halves cannot drift apart.
    let envelope = verbatim_json_lines(&text)
        .into_iter()
        .find(|v| v.get("code").map(|c| c == "cannot_delete").unwrap_or(false))
        .expect("22 no longer contains the verbatim cannot_delete envelope");

    assert_eq!(observed, 409, "fixture 22 records the refusal status");
    assert_eq!(
        amk_types::error::ErrorCode::CannotDelete.status(),
        observed,
        "ErrorCode::CannotDelete must carry the status the live API returned, not the docs page's"
    );
    assert_eq!(envelope["name"].as_str().unwrap(), "CannotDeleteError");
    assert_eq!(
        amk_types::error::ErrorCode::CannotDelete.as_str(),
        envelope["code"].as_str().unwrap()
    );
}

/// Fixture 23 pinned the shape of a real minted key, superseding the `[ASSUMED]` guess the
/// api-keys dispatch shipped. `prefix` is a wire field on every `ApiKey` response, so its shape is
/// not ours to choose.
///
/// Asserted here rather than in `amk-store` because this is a wire-shape claim, and asserted by
/// reading the capture: the fixture's own `prefix` value decides the expected length, so a comment
/// saying "6" could not drift away from a constant saying 8.
#[test]
fn minted_key_prefix_shape_matches_fixture_23() {
    let text = fixture("23-inbox-defaults-and-key-shape.txt");
    let envelope = verbatim_json_lines(&text)
        .into_iter()
        .find(|v| v.get("prefix").is_some() && v.get("api_key_id").is_some())
        .expect("23 no longer contains the verbatim create-api-key response");
    let prefix = envelope["prefix"].as_str().expect("prefix is a string");

    assert!(prefix.starts_with("am_us_"), "the observed tag is am_us_: {prefix}");
    assert!(!prefix.starts_with("am_eu_"), "a minted key must never route to the EU host");
    let visible = &prefix["am_us_".len()..];
    assert_eq!(visible.len(), 6, "fixture 23 records a 6-character visible portion");
    assert!(
        visible
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "the observed visible portion is lowercase hex: {visible}"
    );
    assert!(
        !text.contains("\"api_key\":\"am_us_a"),
        "the secret must stay redacted in the fixture"
    );
}

/// Fixture 23's `POST /v0/api-keys` response carries `organization_id` as its **first** field, and
/// `amk_types::api_key::{ApiKey, CreateApiKeyResponse}` withheld it until that capture existed —
/// their own doc comment said "if a capture ever shows it, add it then". This asserts both halves:
/// that the fixture really shows it (so the type change has evidence behind it), and that the
/// types now carry it (so a later refactor cannot quietly drop the field back out and reintroduce
/// the conformance diff). The pre-dispatch review of `.claude/contracts/amk-http.md` found the gap
/// — the probe that produced the evidence did not act on it, which is why this is a test and not a
/// comment.
#[test]
fn fixture_23_pins_organization_id_on_both_api_key_response_types() {
    let text = fixture("23-inbox-defaults-and-key-shape.txt");
    let envelope = verbatim_json_lines(&text)
        .into_iter()
        .find(|v| v.get("prefix").is_some() && v.get("api_key_id").is_some())
        .expect("23 no longer contains the verbatim create-api-key response");
    assert!(
        envelope.get("organization_id").is_some(),
        "fixture 23 is the evidence for this field; if it stopped showing it, the types must be \
         re-examined rather than left carrying an unevidenced field"
    );

    // Round-trip through the real types, not a hand-built JSON blob: a field that serialises but
    // does not deserialise (or vice versa) would pass a one-directional check.
    let created = amk_types::api_key::CreateApiKeyResponse {
        organization_id: Some(amk_types::ids::OrganizationId::new("org-23")),
        api_key_id: amk_types::ids::ApiKeyId::new("11111111-1111-1111-1111-111111111111"),
        api_key: "am_us_deadbeef".into(),
        prefix: "am_us_dead".into(),
        name: "k".into(),
        pod_id: None,
        inbox_id: None,
        permissions: None,
        created_at: amk_types::Timestamp::from(
            chrono::DateTime::parse_from_rfc3339("2026-08-16T03:56:42.259Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        ),
    };
    let json = serde_json::to_value(&created).expect("serialises");
    assert_eq!(json["organization_id"], "org-23");
    let back: amk_types::api_key::CreateApiKeyResponse =
        serde_json::from_value(json).expect("round-trips");
    assert_eq!(back.organization_id, created.organization_id);

    // And the absent case still omits rather than emitting null — the rule the whole crate turns on.
    let anonymous = amk_types::api_key::CreateApiKeyResponse { organization_id: None, ..created };
    let json = serde_json::to_string(&anonymous).expect("serialises");
    assert!(
        !json.contains("organization_id"),
        "an absent optional is omitted, never null: {json}"
    );
}
