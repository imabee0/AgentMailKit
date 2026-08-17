//! The two error shapes, wired to axum's `IntoResponse`.
//!
//! `[SPEC:fixture 05-error-catalog.http]`: auth-layer failures ([`GatewayFailure`]) are a **bare**
//! `{"message":…}` body at 401/403 — no `name`, no `code`, no `fix`, no `docs` — even for a
//! well-formed-but-unknown key. Application failures ([`AppError`]) are the full envelope. The
//! two never share a type: a handler that wanted to return the bare shape would have to reach for
//! [`GatewayFailure`] explicitly, and every handler in this crate returns [`AppError`] only — the
//! bare shape is emitted exclusively by the auth layer (`crate::auth`), before a handler runs.

use amk_store::StoreError;
use amk_types::{ErrorCode, ErrorEnvelope, GatewayError};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// The auth layer's bare body. Constructed only by `crate::auth` and the 404 fallback never
/// reaches for it — the fallback is an application failure (`not_found`), not an auth failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayFailure {
    pub status: StatusCode,
    pub body: GatewayError,
}

impl GatewayFailure {
    pub fn unauthorized() -> Self {
        Self { status: StatusCode::UNAUTHORIZED, body: GatewayError::unauthorized() }
    }
    pub fn forbidden() -> Self {
        Self { status: StatusCode::FORBIDDEN, body: GatewayError::forbidden() }
    }
}

impl IntoResponse for GatewayFailure {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// The full application-layer envelope, status derived from `ErrorCode::status()` — never
/// re-derived at a call site.
///
/// Boxed rather than inline: `ErrorEnvelope` carries several `String`/`Vec` fields (clippy's
/// `result_large_err`, `-D warnings` under `./scripts/check.sh`), and every fallible handler in
/// this crate returns `Result<_, AppError>`, so its size is the size every `?` propagates.
#[derive(Debug, Clone, PartialEq)]
pub struct AppError(pub Box<ErrorEnvelope>);

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(Box::new(ErrorEnvelope::new(code, message)))
    }

    pub fn internal(context: &str) -> Self {
        eprintln!("amk-http: internal error: {context}");
        Self::new(ErrorCode::InternalError, "Internal error.")
    }

    /// Override the per-code default [`fix_for`] would otherwise backfill — see
    /// `IntoResponse for AppError`'s own doc for why a handler only ever needs this when it has
    /// more specific guidance than the generic default (`lib.rs`'s `not_found_fallback` is the one
    /// caller today).
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.0.fix = Some(fix.into());
        self
    }
}

/// Divergence 2 (`reference/fixtures/25-p1-gate-conformance.txt`): `GET /v0/no-such-route` and
/// `DELETE /v0/auth/me` both carried `fix` live; ours omitted it everywhere. `ErrorEnvelope`
/// already has the field and `ErrorCode::docs_url()` is the same per-code mechanism already
/// wired for `docs` — this is that same mechanism for `fix`, kept in this crate because
/// `amk-types` is frozen for this dispatch.
///
/// One sentence per code, exhaustive (no wildcard, so a new `ErrorCode` variant fails to compile
/// here until it is given one — the same discipline `ErrorCode::as_str` already has). `NotFound`'s
/// text is `amk_types::error`'s own pinned round-trip fixture
/// (`not_found_envelope_matches_live_capture`) verbatim, the one code with a complete (untrimmed)
/// captured sentence to reuse; every other code is written fresh, in the same register — the rest
/// of `reference/fixtures/05-error-catalog.http`'s captures are hand-trimmed with a mid-sentence
/// "..." and are not literal wire text to copy.
///
/// `NotFound`'s own text is deliberately generic (not resource-specific): this same code covers
/// both an absent resource lookup (`amk_core::scope::ScopeDenial`, which already sets a
/// resource-aware `fix` of its own — see `IntoResponse for AppError` for why that value survives
/// untouched) and a route that never matched at all (`lib.rs`'s `not_found_fallback`, which sets
/// a route-specific override via [`AppError::with_fix`]). This default only has to be honest for
/// whatever is left after both of those, which is `amk_core::permissions::Denial::Hidden`.
fn fix_for(code: ErrorCode) -> &'static str {
    use ErrorCode::*;
    match code {
        // 401 — never actually reaches this function: the auth layer emits the bare
        // `GatewayFailure` body for all four of these, which has no `fix` field to fill. Entries
        // exist only so this match stays exhaustive over every `ErrorCode` variant.
        MissingAuthorization | Unauthorized => {
            "Include an `Authorization: Bearer <api key>` header on the request."
        }
        InvalidTokenType => "Use a Bearer token; other authorization schemes are not accepted.",
        UnknownApiKey => "Check that the api key is correct and has not been revoked.",
        // 403
        MissingPermission => {
            "Grant this api key the permission the requested operation needs, \
            or use a key that already has it."
        }
        PermissionEscalation => {
            "A restricted key cannot mint or be granted a permission its \
            own key does not hold; request only a subset of this key's own permissions."
        }
        UnrestrictedKeyRequired => {
            "Use an organization-scoped key with no permission restrictions for this operation."
        }
        Forbidden => {
            "This credential is not authorized for the requested resource; use a key \
            with the required scope."
        }
        MessageRejected => {
            "The outbound message was rejected; check the sender, recipients and \
            content before retrying."
        }
        AlreadyExists | ResourceTaken => {
            "Choose a different, available identifier — see suggestions for alternatives, if \
            present — and retry."
        }
        LimitExceeded => {
            "This organization has reached its configured limit for the resource; \
            raise the limit or free up capacity before retrying."
        }
        DomainNotVerified => "Verify the domain's DNS records before using it.",
        // 400 / 404 / 422
        ValidationError => {
            "Inspect the errors array — each entry names a path and a message — \
            and correct the request body."
        }
        // Verbatim, `reference/fixtures/05-error-catalog.http`'s one complete capture — see this
        // function's own doc for why it is deliberately generic rather than resource-specific.
        NotFound => "No inbox with the given identifier is visible to this credential.",
        Unprocessable => {
            "The request was well-formed but could not be processed; check the \
            values against the documented constraints."
        }
        QueryRangeTooWide => "Narrow the query's date or id range and retry.",
        // 409
        Conflict | RaceCondition => {
            "The resource's current state conflicts with this request; reload it and retry."
        }
        ResourceDeleting => {
            "This resource is being deleted and cannot be modified; wait for \
            deletion to finish."
        }
        CannotDelete => {
            "Remove or reassign the resources that still depend on this one before \
            deleting it."
        }
        // 429 / 5xx
        RateLimitExceeded => "Slow down and retry after a short delay.",
        ServiceUnavailable => "The service is temporarily unavailable; retry after a short delay.",
        InternalError => "Retry the request; if it keeps failing, contact support.",
    }
}

impl IntoResponse for AppError {
    /// The one place every [`AppError`] becomes bytes — so it is also the one place `fix` is
    /// guaranteed filled, whichever of this crate's several construction paths built the envelope
    /// (`AppError::new`, `From<ErrorEnvelope>`, `From<ScopeDenial>`, `From<Denial>`). A
    /// construction that already set `fix` — `amk_core::scope::ScopeDenial::into_envelope`'s own
    /// resource-aware text, or `AppError::with_fix`'s explicit override — is left untouched: this
    /// only fills a `None`, never overwrites a `Some`.
    fn into_response(self) -> Response {
        let mut envelope = self.0;
        if envelope.fix.is_none() {
            envelope.fix = Some(fix_for(envelope.code).to_owned());
        }
        let status =
            StatusCode::from_u16(envelope.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(envelope)).into_response()
    }
}

impl From<ErrorEnvelope> for AppError {
    fn from(e: ErrorEnvelope) -> Self {
        Self(Box::new(e))
    }
}

impl From<amk_core::scope::ScopeDenial> for AppError {
    fn from(d: amk_core::scope::ScopeDenial) -> Self {
        Self(Box::new(d.into_envelope()))
    }
}

impl From<amk_core::permissions::Denial> for AppError {
    fn from(d: amk_core::permissions::Denial) -> Self {
        Self(Box::new(ErrorEnvelope::new(d.code(), d.to_string())))
    }
}

/// Database/store failures that are not one of amk-http's own validation rules. The two
/// distinguishable variants get their documented codes; everything else — a bare `sqlx::Error`,
/// or a page-token failure that reached this far without a more specific handler — is an internal
/// error. Never leaks the underlying database error text to the client.
impl From<StoreError> for AppError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::PodNotEmpty => Self::new(
                ErrorCode::CannotDelete,
                "Cannot delete Pod: Cannot delete pod with existing inboxes",
            ),
            StoreError::InboxAlreadyExists => {
                Self::new(ErrorCode::AlreadyExists, "Inbox already exists")
            }
            StoreError::InvalidPageToken(_) => {
                Self::new(ErrorCode::ValidationError, "Invalid page_token.")
                    .with_issue("page_token", "invalid page token")
            }
            StoreError::InvalidValue(field) => {
                Self::new(ErrorCode::ValidationError, format!("Invalid value for {field}."))
                    .with_issue(field, "contains a value the server cannot store")
            }
            StoreError::Database(e) => Self::internal_from(e),
        }
    }
}

impl AppError {
    fn internal_from(e: impl std::fmt::Display) -> Self {
        Self::internal(&e.to_string())
    }

    /// Attach one `validation_error` issue naming `path` as a single string segment, REPLACING
    /// whatever `errors[]` already holds. Small convenience — the full `errors[]` shape (mixed
    /// string/integer path) lives on `amk_types::ValidationIssue` directly for call sites that
    /// need more than one segment.
    ///
    /// Both callers construct through `Self::new(ErrorCode::ValidationError, ...)` first, which
    /// (`ErrorEnvelope::new`'s own doc) already auto-synthesizes a one-item placeholder `errors`
    /// entry from that constructor's `message` argument — so replacing is the fix, not merely a
    /// style choice: `.push`ing onto that placeholder is a real defect this dispatch's own edge
    /// case 8 test caught (a `page_token` failure carried BOTH the placeholder `custom` entry and
    /// this function's own `invalid_format` one, `errors` length 2 against the reference's exactly
    /// 1), predating this dispatch and equally present on `StoreError::InvalidValue`'s call site.
    ///
    /// `page_token`'s own kind is special-cased HERE, rather than at its call site
    /// (`StoreError::InvalidPageToken`'s mapping), per the dispatch contract's writable-paths
    /// list, which grants this function alone: `reference/fixtures/27-malformed-request-handling
    /// .txt` §3(e) — the reference emits `{"code":"invalid_format","format":"base64url",
    /// "path":["page_token"]}`, not the `custom` shape this used to emit for every caller
    /// indistinguishably, including `StoreError::InvalidValue`'s "contains a value the server
    /// cannot store" (the only other caller, which keeps the unchanged generic shape below).
    fn with_issue(mut self, path: &str, message: &str) -> Self {
        // Built through the constructors rather than as a struct literal: `ValidationIssue` now
        // carries seven per-kind extras (`reference/fixtures/27-malformed-request-handling.txt`),
        // and a literal here would have to name every one of them and would break again on the
        // next kind the reference shows us.
        let issue = if path == "page_token" {
            amk_types::ValidationIssue::invalid_format(
                "base64url",
                Some(path),
                "Invalid base64url-encoded string",
            )
        } else {
            let mut issue = amk_types::ValidationIssue::custom(message);
            issue.path = vec![serde_json::Value::String(path.into())];
            issue
        };
        self.0.errors = vec![issue];
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_failure_is_the_bare_body_never_the_envelope() {
        let f = GatewayFailure::unauthorized();
        let s = serde_json::to_string(&f.body).unwrap();
        assert_eq!(s, r#"{"message":"Unauthorized"}"#);
        assert!(!s.contains("code"));
        assert!(!s.contains("name"));

        let f = GatewayFailure::forbidden();
        let s = serde_json::to_string(&f.body).unwrap();
        assert_eq!(s, r#"{"message":"Forbidden"}"#);
        assert_eq!(f.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn app_error_status_comes_from_the_error_code_not_a_second_table() {
        let e = AppError::new(ErrorCode::CannotDelete, "x");
        assert_eq!(e.0.status(), 409);
        let e = AppError::new(ErrorCode::NotFound, "x");
        assert_eq!(e.0.status(), 404);
    }

    #[test]
    fn pod_not_empty_maps_to_cannot_delete_409() {
        let e = AppError::from(StoreError::PodNotEmpty);
        assert_eq!(e.0.code, ErrorCode::CannotDelete);
        assert_eq!(e.0.status(), 409);
    }

    #[test]
    fn database_errors_never_leak_into_the_client_facing_message() {
        let e = AppError::internal_from("connection refused: host unreachable at 10.0.0.5:5432");
        assert_eq!(e.0.code, ErrorCode::InternalError);
        assert_eq!(e.0.message, "Internal error.");
        assert!(!e.0.message.contains("10.0.0.5"));
    }
}
