//! Inbound mail: SMTP state machine and persist.
//!
//! `[SPEC:.claude/contracts/amk-ingest.md]`.
//!
//! `smtp-proto` parses commands; this crate owns the session. `mail-parser` and `mail-auth`
//! stay inside [`accept`] / the session and are converted at the edge — no `mail_parser::` /
//! `mail_auth::` / `smtp_proto::` type appears in a public signature.
//! `./scripts/shape-provenance.sh` section 4 checks that.

pub mod accept;
pub mod error;
pub mod lookup;
pub mod smtp;
pub mod tls;

pub use accept::{
    accept, AcceptRequest, Accepted, Authenticator, Delivery, Envelope, Persist, StorePersist,
};
pub use error::IngestError;
pub use lookup::{FixedInboxLookup, InboxLookup};
pub use smtp::{serve_session, IngestConfig};
