//! [`JsonBody`] and [`QueryParams`] — divergence 3's `PathPodId`/`PathPodIdString` pattern
//! (`crate::ids`), extended from path segments to the request body and the query string. Same
//! mechanism: wrap axum's own extractor, set `type Rejection = AppError`, so every rejection this
//! crate can produce becomes the ordinary envelope instead of axum's plain-text body.
//!
//! `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` is the one source of truth for
//! what every malformed request below must produce: 400, `application/json`, the full envelope,
//! exactly one `errors[]` entry — never axum's own `text/plain` rejection, never a status the
//! error catalog does not contain (415/422/413), and never serde's own message verbatim (it names
//! our internal Rust types — the fixture's own example is
//! `data did not match any variant of untagged enum MetadataValue`, and this crate's
//! `amk_types::inbox::MetadataValue` is a real `#[serde(untagged)]` enum a malformed `metadata`
//! value can reach). The one thing that IS taken from a rejection is the offending field's own
//! name: the client supplied it, so echoing it back discloses nothing this server did not already
//! receive from the caller.
//!
//! # Why this crate hand-parses a rendered rejection string at all
//!
//! The field path and the reference's `expected`/`received` words come from
//! `serde_path_to_error`, which axum uses *internally* (`Json::from_bytes`, `Query::try_from_uri`)
//! but does not expose structurally — `JsonDataError`/`FailedToDeserializeQueryString` carry only
//! an opaque `Display`. Adding `serde_path_to_error` as this crate's own dependency to get
//! structured access is the obvious fix and is exactly what the dispatch contract's dependency
//! section forbids ("No new dependency... already available" names only
//! `axum::extract::rejection::*` and `axum::extract::DefaultBodyLimit"). So the functions below
//! parse the one shape axum's own rendering is known, by direct probe against this crate's own
//! request types, to produce — `"<field>: invalid type: <received>, expected <expected> at line L
//! column C"` — through a closed whitelist of the six JSON value kinds (JSON has exactly six; this
//! is not an open-ended guess), and fall back to a generic, un-specific issue for every other
//! shape (missing field, duplicate key, an untagged-enum mismatch, a nested/`[N]`-indexed path)
//! rather than echo or guess at a shape that has not been observed. See `json_value_kind`'s own
//! doc for the concrete case this protects: `{"permissions":"nope"}` renders `expected struct
//! ApiKeyPermissions` — a Rust type name — and the whitelist's job is specifically to let that one
//! fall through to the generic issue instead of being echoed.

use amk_types::{ErrorCode, ErrorEnvelope, ValidationIssue};
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody, JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::Json as AxumJson;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// Wraps `axum::Json<T>`. Unlike `Json<T>` itself, this never checks `Content-Type` — fixture 27
/// §2/§3(d): `POST /v0/pods` with `Content-Type: text/plain` or no `Content-Type` at all both
/// return 200 on the reference, and every request type this dispatch's 8 body sites use has
/// all-optional fields, so an absent body is simply `{}`.
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // `[INFERRED]`: no fixture probes an explicitly-JSON-typed EMPTY body. This is what
        // reconciles fixture 27 §2's "no body, no Content-Type at all -> 200" with this dispatch's
        // own edge case 1, which groups a truly empty body carrying an explicit
        // `Content-Type: application/json` with `not json`/a NUL byte — i.e. treated as a genuine
        // syntax failure — rather than with the no-header/wrong-header case, which synthesizes
        // `{}`. A client that explicitly asserts JSON and then sends nothing gets the ordinary
        // "not valid JSON" answer; a client that gives no signal at all gets the "absent body"
        // answer.
        let synthesize_empty_object = !looks_like_json_content_type(req.headers());
        let bytes = match Bytes::from_request(req, state).await {
            Ok(b) => b,
            Err(rejection) => return Err(json_rejection_to_app_error(rejection.into())),
        };
        let effective: &[u8] = if bytes.is_empty() && synthesize_empty_object {
            b"{}"
        } else {
            &bytes
        };
        match AxumJson::<T>::from_bytes(effective) {
            Ok(AxumJson(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection_to_app_error(rejection)),
        }
    }
}

/// Wraps `axum::Query<T>` — same mechanism as [`JsonBody`], for the six query-string sites.
pub struct QueryParams<T>(pub T);

impl<S, T> FromRequestParts<S> for QueryParams<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::try_from_uri(&parts.uri) {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(query_rejection_to_app_error(rejection)),
        }
    }
}

/// A conservative, dependency-free stand-in for axum's own (private) `json_content_type` check —
/// used only to decide whether an empty body should be synthesized as `{}` or treated as a genuine
/// syntax failure (`JsonBody::from_request`'s own doc). Not a security or routing decision: this
/// crate never rejects on `Content-Type` (fixture 27), so an imprecise match here only changes
/// which shape of the SAME 400 envelope an edge case gets, never whether the request is accepted.
fn looks_like_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    essence == "application/json" || essence.ends_with("+json")
}

/// One `validation_error` envelope carrying exactly `issue` in `errors[]`.
///
/// `pub(crate)` for [`crate::pagination`], which classifies `limit` structurally rather than by
/// matching a rejection's rendered text and so needs to raise an issue of its own through the same
/// single constructor. One envelope shape, one place that builds it.
pub(crate) fn validation_error(issue: ValidationIssue) -> AppError {
    // `ErrorEnvelope::new` special-cases `ValidationError` by boxing its `message` argument into a
    // synthesized `errors[0]` (`amk_types::error`'s own doc on `ErrorEnvelope::new`) — passed
    // through here only to be immediately replaced by `with_issues`, so the placeholder text below
    // is never observed on the wire.
    AppError::from(
        ErrorEnvelope::new(ErrorCode::ValidationError, "validation_error").with_issues(vec![issue]),
    )
}

// ---- JSON body rejections ------------------------------------------------------------------

/// Every variant `JsonRejection` can hold today, matched exhaustively, plus the wildcard
/// `#[non_exhaustive]` forces on any match written outside axum-core's own crate.
fn json_rejection_to_app_error(rejection: JsonRejection) -> AppError {
    let issue = match rejection {
        JsonRejection::JsonSyntaxError(_) => syntactically_invalid_json(),
        JsonRejection::JsonDataError(inner) => json_data_error_to_issue(&inner.body_text()),
        // Structurally unreachable through `JsonBody::from_request` above: this wrapper never
        // calls axum's own content-type-checking `Json::from_request`, only `Bytes::from_request`
        // + `Json::from_bytes` — neither of which ever constructs this variant (content-type is
        // not enforced at all, fixture 27 §2/§3(d)). Kept as its own explicit arm, rather than
        // folded into the wildcard below, so a future refactor that reintroduces a content-type
        // check does not have to rediscover this mapping. Verified only by the match compiling
        // exhaustively, not by a reachable test — see the dispatch report's mutation-pass notes.
        JsonRejection::MissingJsonContentType(_) => syntactically_invalid_json(),
        JsonRejection::BytesRejection(inner) => bytes_rejection_to_issue(inner),
        // `JsonRejection` is `#[non_exhaustive]` (axum-core's `composite_rejection!` macro always
        // adds it), so a match over it outside axum-core must carry a wildcard even though every
        // variant above is already enumerated. This is that arm, not a silent catch-all for a
        // variant this file forgot: it only fires if axum-core ships a new variant this crate has
        // not been updated for, and it still produces the correct envelope shape rather than
        // panicking or leaking axum's own rejection text.
        _ => syntactically_invalid_json(),
    };
    validation_error(issue)
}

/// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §2: a body that is not valid JSON
/// at all — `path: []`, the one case where an empty path is correct (§3(b): empty is for a
/// whole-body rule, and "this body has no JSON in it" is exactly that).
fn syntactically_invalid_json() -> ValidationIssue {
    ValidationIssue::invalid_format("json_string", None, "Invalid JSON string")
}

fn bytes_rejection_to_issue(rejection: BytesRejection) -> ValidationIssue {
    match rejection {
        BytesRejection::FailedToBufferBody(inner) => failed_to_buffer_body_to_issue(inner),
        // Same `#[non_exhaustive]` situation as `JsonRejection`'s own wildcard above —
        // `BytesRejection` has exactly one variant today.
        _ => syntactically_invalid_json(),
    }
}

fn failed_to_buffer_body_to_issue(rejection: FailedToBufferBody) -> ValidationIssue {
    match rejection {
        // `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §5: the reference BUFFERS
        // AND PARSES an oversized (3 MB, invalid-JSON) body and answers with the ordinary syntax
        // error, not a size-specific one — axum-core marks this rejection
        // `#[status = PAYLOAD_TOO_LARGE]` (413, a status the error catalog does not contain).
        // Collapsing it to the same envelope every other unparseable body gets is also the correct
        // outcome for the fixture's own construction: an invalid-JSON body large enough to hit the
        // length limit must never be treated as valid just because a smaller prefix of it slipped
        // under the limit.
        FailedToBufferBody::LengthLimitError(_) => syntactically_invalid_json(),
        FailedToBufferBody::UnknownBodyError(_) => syntactically_invalid_json(),
        // Same `#[non_exhaustive]` situation as above.
        _ => syntactically_invalid_json(),
    }
}

/// `JsonDataError` covers every JSON body that parsed syntactically but failed to match `T` —
/// wrong-typed field, missing required field, duplicate key, an untagged-enum mismatch, a wrong
/// top-level shape. Only the ONE shape fixture 27 actually observed (a scalar field given the
/// wrong JSON type) is translated into the reference's `invalid_type` extras; every other shape
/// still produces the correct envelope — field name included when it is safely derivable — but
/// without extras this crate has never seen the reference emit, rather than guessing at zod's
/// vocabulary for a message it has never shown us.
fn json_data_error_to_issue(rendered: &str) -> ValidationIssue {
    // axum's `JsonDataError::body_text()` is `"Failed to deserialize the JSON body into the
    // target type: {serde_path_to_error's own Display}"` — stripping axum's own fixed prefix
    // first keeps everything below decoupled from wording only axum controls.
    const PREFIX: &str = "Failed to deserialize the JSON body into the target type: ";
    let inner = rendered.strip_prefix(PREFIX).unwrap_or(rendered);
    let (field, message) = split_field_prefix(inner);
    match parse_invalid_type(message) {
        Some((received, expected)) => ValidationIssue::invalid_type(
            expected,
            // The reference OMITS `received` for a body type mismatch — only the query path's
            // `NaN` case carries it (fixture 27 §3(a)'s own table). The word still appears inside
            // `message` below, matching the observed `{"name":123}` capture verbatim.
            None,
            field,
            format!("Invalid input: expected {expected}, received {received}"),
        ),
        None => unattributed_data_error(field),
    }
}

/// A field-level (or, with `field: None`, whole-body) JSON data failure this crate cannot
/// classify more specifically without risking a disclosure it has not verified is safe. Still the
/// correct envelope: 400, `validation_error`, one `errors[]` entry, field named when known.
fn unattributed_data_error(field: Option<&str>) -> ValidationIssue {
    let mut issue = ValidationIssue::custom(
        "The value provided for this field could not be parsed into the expected shape.",
    );
    if let Some(name) = field {
        issue.path = vec![serde_json::Value::String(name.to_owned())];
    }
    issue
}

/// The narrow `"invalid type: <received>, expected <expected> at line L column C"` shape
/// `serde_json` produces for a scalar-field mismatch, translated into zod's own vocabulary for the
/// JSON value kinds it can safely name. `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
/// directly observed only the `expected:"string"` cell of this table (`{"name":123}`); the rest
/// reuses the SAME zod keywords `too_small`'s `origin` field already carries elsewhere in this
/// crate — JSON has exactly six value kinds, so this is the complete table for what `expected`/
/// `received` can safely be, not an open-ended guess. `[INFERRED]` beyond the one observed cell.
fn parse_invalid_type(message: &str) -> Option<(&'static str, &'static str)> {
    let rest = message.strip_prefix("invalid type: ")?;
    let (received_raw, rest) = rest.split_once(", expected ")?;
    let expected_raw = match rest.split_once(" at line ") {
        Some((expected, _)) => expected,
        None => rest,
    };
    let received = json_value_kind(received_raw)?;
    let expected = json_value_kind(expected_raw)?;
    Some((received, expected))
}

/// `serde_json`'s `Unexpected`/`Expected` renderings for the six JSON value kinds, matched by
/// exact prefix so nothing outside this closed set is ever treated as safe to echo. This is
/// precisely the guard against the disclosure this module exists to close: `{"permissions":"nope"}`
/// against `amk_types::api_key::ApiKeyPermissions` renders `expected struct ApiKeyPermissions` — a
/// Rust type name — which matches none of the arms below and therefore correctly falls through to
/// [`unattributed_data_error`] instead of being echoed.
fn json_value_kind(raw: &str) -> Option<&'static str> {
    if raw == "a string" || raw.starts_with("string ") {
        Some("string")
    } else if raw.starts_with("integer ") || raw.starts_with("floating point ") {
        Some("number")
    } else if raw.starts_with("boolean ") {
        Some("boolean")
    } else if raw == "sequence" {
        Some("array")
    } else if raw == "map" {
        Some("object")
    } else if raw == "null" {
        Some("null")
    } else {
        None
    }
}

/// Recovers `("name", "invalid type: ...")` from `"name: invalid type: ..."` — the one path shape
/// this dispatch's eight body types and six query types can produce at the top level (a single,
/// unnested struct field; none has an array or a value with a second level of nesting a caller
/// could misname). A message that does not start with a bare lowercase identifier followed by
/// `": "` is left with `field: None` rather than guessed at — deliberately narrower than
/// `serde_path_to_error::Path`'s own grammar (which also has `[N]` and dotted segments this
/// function does not attempt), so it can never misattribute a slice of a message shape it does not
/// recognize as a field name.
fn split_field_prefix(message: &str) -> (Option<&str>, &str) {
    if let Some(idx) = message.find(": ") {
        let candidate = &message[..idx];
        let is_identifier = !candidate.is_empty()
            && candidate
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if is_identifier {
            return (Some(candidate), &message[idx + 2..]);
        }
    }
    (None, message)
}

// ---- query string rejections ----------------------------------------------------------------

/// Every variant `QueryRejection` can hold today, matched exhaustively, plus the mandatory
/// wildcard — same reasoning as [`json_rejection_to_app_error`]'s own.
fn query_rejection_to_app_error(rejection: QueryRejection) -> AppError {
    let issue = match rejection {
        QueryRejection::FailedToDeserializeQueryString(inner) => {
            query_deserialize_error_to_issue(&inner.body_text())
        }
        // `QueryRejection` is `#[non_exhaustive]` with exactly one variant today — same situation
        // as `JsonRejection`'s own wildcard.
        _ => unattributed_data_error(None),
    };
    validation_error(issue)
}

/// The reference's closed vocabulary for a boolean-shaped query parameter, verbatim from
/// `reference/fixtures/27-malformed-request-handling.txt` §1 (`?ascending=maybe`). This crate's
/// own `ascending: Option<bool>` field type only actually ACCEPTS `"true"`/`"false"` — narrower
/// than the reference's full "stringbool" schema — an existing, out-of-scope gap in
/// `crate::pagination` this dispatch does not widen (flagged in the dispatch report); the values
/// listed here describe the reference's contract, which is what a client parses `errors[]`
/// against.
const STRINGBOOL_VALUES: [&str; 12] = [
    "true", "1", "yes", "on", "y", "enabled", "false", "0", "no", "off", "n", "disabled",
];

fn stringbool_message() -> String {
    let quoted: Vec<String> = STRINGBOOL_VALUES
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect();
    format!("Invalid option: expected one of {}", quoted.join("|"))
}

/// `axum::Query<T>::try_from_uri` deserializes through `serde_urlencoded`, whose per-field errors
/// are Rust's own stable, generic `std::num::ParseIntError`/`std::str::ParseBoolError` text — not
/// internal to this crate, and matched here by that stable text rather than by field name, so this
/// does not hard-code `"limit"`/`"ascending"` specifically and keeps working if a future query
/// type reuses a `u64`/`bool` field under a different name.
fn query_deserialize_error_to_issue(rendered: &str) -> ValidationIssue {
    const PREFIX: &str = "Failed to deserialize query string: ";
    let inner = rendered.strip_prefix(PREFIX).unwrap_or(rendered);
    let (field, message) = split_field_prefix(inner);
    // NOTE: there is deliberately no `ParseIntError` arm here. `limit` — the only numeric query
    // parameter this crate has — is `Option<String>` and is classified structurally by
    // `crate::pagination::parse_limit`, precisely because the reference splits `?limit=abc`
    // (`invalid_type`/NaN) from `?limit=-1`/`?limit=`/`?limit=0` (`too_small`) and serde's integer
    // impl renders all of those the same way. An arm here could only ever collapse them again.
    if message == "provided string was not `true` or `false`" {
        ValidationIssue::invalid_value(
            "stringbool",
            STRINGBOOL_VALUES.iter().map(|s| (*s).to_owned()).collect(),
            field,
            stringbool_message(),
        )
    } else {
        unattributed_data_error(field)
    }
}
