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
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0)).into_response()
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

    /// Attach one `validation_error` issue naming `path` as a single string segment. Small
    /// convenience — the full `errors[]` shape (mixed string/integer path) lives on
    /// `amk_types::ValidationIssue` directly for call sites that need more than one segment.
    fn with_issue(mut self, path: &str, message: &str) -> Self {
        self.0.errors.push(amk_types::ValidationIssue {
            code: "custom".into(),
            path: vec![serde_json::Value::String(path.into())],
            message: message.into(),
        });
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
