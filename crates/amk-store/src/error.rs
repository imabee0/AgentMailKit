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

    /// A page token failed validation before any query ran.
    #[error("invalid page token: {0}")]
    InvalidPageToken(#[from] PageTokenError),
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
