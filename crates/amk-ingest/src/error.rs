//! Ingest failures in this crate's own vocabulary — never a `mail_*` / `smtp_proto` type.

use amk_store::StoreError;

/// Everything that can go wrong on the way in.
///
/// SMTP replies are derived from this; callers never see a parser or authenticator type.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// A permanent or transient SMTP reject. `code` is the three-digit reply.
    #[error("{code} {message}")]
    Rejected { code: u16, message: String },
    /// Persistence failed after the message was accepted at the protocol layer.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The socket or resolver failed.
    #[error("{0}")]
    Io(String),
}

impl IngestError {
    pub fn rejected(code: u16, message: impl Into<String>) -> Self {
        Self::Rejected { code, message: message.into() }
    }

    pub fn smtp_code(&self) -> u16 {
        match self {
            Self::Rejected { code, .. } => *code,
            Self::Store(_) => 451,
            Self::Io(_) => 421,
        }
    }

    pub fn smtp_text(&self) -> String {
        match self {
            Self::Rejected { message, .. } => message.clone(),
            Self::Store(_) => "Temporary local problem".into(),
            Self::Io(_) => "Closing connection".into(),
        }
    }
}
