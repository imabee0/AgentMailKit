//! The store's error type.
//!
//! `StoreError` is deliberately not an HTTP shape: mapping a variant to a wire `ErrorEnvelope`
//! and an HTTP status is amk-http's job. This crate only distinguishes the cases a caller needs
//! to branch on — a collision, an invalid page token, or "something else went wrong".

/// Failures from persistence.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An unmapped database failure — connection loss, a syntax error, a constraint violation
    /// this crate does not give its own variant.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A second inbox was created with the same (normalized) `inbox_id`. Maps to
    /// `already_exists` / HTTP 403 at the boundary that owns wire shapes; this crate exposes only
    /// the distinguishable variant.
    #[error("inbox username already exists")]
    InboxAlreadyExists,

    /// `pods::delete` was refused because a row still references the pod (an inbox, thread,
    /// message or api-key foreign key naming it — `pods::is_pod_reference_violation` matches the
    /// exact constraint name, never a bare SQLSTATE). Fixture 22: `cannot_delete` / HTTP 409, and
    /// the refusal is total — neither the pod nor the referencing row is touched. This crate
    /// exposes only the distinguishable variant; mapping it to a wire status is amk-http's job.
    #[error("pod is not empty: a row still references it")]
    PodNotEmpty,

    /// A page token failed validation before any query ran.
    #[error("invalid page token: {0}")]
    InvalidPageToken(#[from] PageTokenError),

    /// A caller-supplied value cannot be persisted: it carries a byte Postgres cannot encode as
    /// `text` — a NUL, `0x00` — which would otherwise reach an `INSERT`'s bound parameter and
    /// fail at encoding (SQLSTATE `22021`) rather than as a clear, typed error. Returned in place
    /// of that raw [`StoreError::Database`], the same way [`PageTokenError::ForbiddenByte`] is
    /// returned in place of a database error on a lookup. The `&'static str` names the field.
    #[error("invalid value for {0}: contains a forbidden byte")]
    InvalidValue(&'static str),
}

/// Why a page token was rejected.
///
/// Structural failures (bad base64, bad JSON, wrong shape) are caught by
/// [`amk_types::page::Cursor::decode`] before any query runs. [`PageTokenError::WrongScope`] is
/// this crate's own check: the token's own `inbox_id` must agree with the inbox this request is
/// pinned to, so a cursor minted while paging one inbox cannot be replayed against another.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageTokenError {
    #[error("page token is not valid base64")]
    Base64,
    #[error("page token is not valid JSON")]
    Json,
    #[error("page token is not a JSON object")]
    NotAnObject,
    #[error("page token is missing required field {0:?}")]
    MissingField(&'static str),
    #[error("page token field {0:?} has the wrong type")]
    WrongType(&'static str),
    /// The token's own scope coordinate disagrees with the request's pinned scope — e.g. a token
    /// minted while paging `inbox-a@…` replayed against a request pinned to `inbox-b@…`.
    #[error("page token does not belong to this request's scope")]
    WrongScope,
    /// A decoded field carries a byte no identifier may contain — see
    /// `amk_types::ids::has_forbidden_byte`. Distinct from [`Self::WrongType`]: the field is a
    /// syntactically plausible string, so the *content*, not the shape, is what is rejected. A
    /// `%00`-bearing `inbox_id`/`message_id` reaches this check before the query it would
    /// otherwise fail at parameter encoding (SQLSTATE `22021`).
    #[error("page token field {0:?} contains a forbidden byte")]
    ForbiddenByte(&'static str),
}

impl From<amk_types::page::CursorError> for PageTokenError {
    fn from(e: amk_types::page::CursorError) -> Self {
        use amk_types::page::CursorError as E;
        match e {
            E::Base64 => Self::Base64,
            E::Json => Self::Json,
            E::NotAnObject => Self::NotAnObject,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_error_maps_onto_page_token_error() {
        use amk_types::page::CursorError as E;
        assert_eq!(PageTokenError::from(E::Base64), PageTokenError::Base64);
        assert_eq!(PageTokenError::from(E::Json), PageTokenError::Json);
        assert_eq!(PageTokenError::from(E::NotAnObject), PageTokenError::NotAnObject);
    }
}
