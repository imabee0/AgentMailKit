//! Outbound mail: MIME assembly, DKIM signing and SMTP delivery.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`.
//!
//! # The boundary this crate exists to hold
//!
//! `mail-send`, `mail-builder` and `mail-auth` are stalwart-labs crates in one of the plan's two
//! sanctioned roles: libraries consumed like any third party. They live **inside** this crate and
//! are converted at its edge — **no `mail_send::` / `mail_builder::` / `mail_auth::` type appears
//! in any public signature or re-export here**, which `./scripts/shape-provenance.sh` checks. This
//! crate's public API speaks `amk-types` and its own error enum, nothing else.
//!
//! That is not ceremony. Those types are ergonomic and right there, and the plan's naming lint
//! plus the dependency-direction check exist precisely because a shape that leaks in at the
//! boundary is invisible in review afterwards.
//!
//! # Delivery is a trait, on purpose
//!
//! [`Transport`] is the seam between "we built and signed a message" and "it left the building".
//! Tests use [`RecordingTransport`] and assert on the assembled MIME; **no test sends real mail**.
//! The live send is P2's R-phys gate half, run from the OVH box against a Gmail account, and it is
//! deliberately not reachable from `cargo test`.

pub mod assemble;
pub mod build;
pub mod signing;

use std::sync::{Arc, Mutex};

/// Everything that can go wrong on the way out, in this crate's own vocabulary.
///
/// Deliberately not a re-export of any `mail_*` error: those are the types the boundary rule above
/// forbids in a public signature, and a caller matching on one would couple `amk-http` to
/// stalwart-labs through the back door.
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    /// No DKIM key is configured for the sending domain.
    ///
    /// **Fails closed**: a message is never sent unsigned. The precedent is `amk-http`'s
    /// `AppConfig`, which refuses inbox creation rather than inventing a domain — the same choice,
    /// for the same reason, and here the cost of guessing is mail that fails DMARC at the
    /// recipient rather than a wrong default in a database.
    #[error("no DKIM key configured for domain {0}")]
    NoSigningKey(String),
    /// The DKIM key is present but unusable. `mail-auth` wants **DER**, not PEM — recorded in
    /// `CLAUDE.md`'s contract-facts list because it has already cost time once.
    #[error("DKIM key for domain {0} could not be loaded (mail-auth wants DER, not PEM)")]
    UnusableSigningKey(String),
    /// A caller-supplied header tried to inject structure — a CR/LF, or a second copy of a header
    /// the envelope owns. Refused rather than sanitised silently, so the caller learns.
    #[error("header {0} is not permitted from a caller")]
    ForbiddenHeader(String),
    /// MIME assembly failed.
    #[error("could not assemble the message: {0}")]
    Assembly(String),
    /// The remote refused, or could not be reached.
    #[error("delivery failed: {0}")]
    Delivery(String),
}

/// One assembled, signed message, ready to hand to a [`Transport`].
///
/// `raw` is the full RFC 5322 byte stream **including** the DKIM-Signature header. `message_id` is
/// the id that actually went on the wire — `[SPEC:reference/fixtures/03-id-formats.http]`: a sent
/// message's `message_id` IS its RFC 5322 Message-ID, never a synthesised surrogate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMessage {
    pub message_id: String,
    pub envelope_from: String,
    pub envelope_to: Vec<String>,
    pub raw: Vec<u8>,
}

/// The seam between assembly and the network.
///
/// A trait rather than a concrete SMTP client so the whole send path is testable without sending,
/// and so direct-to-MX and a smarthost are two implementations of one interface rather than a
/// branch inside the send logic.
pub trait Transport: Send + Sync {
    fn deliver(
        &self,
        message: &SignedMessage,
    ) -> impl std::future::Future<Output = Result<(), OutboundError>> + Send;
}

/// The transport every test uses: records what would have been sent and delivers nothing.
///
/// Assertions go against [`Self::sent`], which holds the assembled MIME — so a test that claims
/// "the reply carried In-Reply-To" is reading the bytes that would have left, not a struct field
/// upstream of the header writer.
#[derive(Debug, Clone, Default)]
pub struct RecordingTransport {
    sent: Arc<Mutex<Vec<SignedMessage>>>,
}

impl RecordingTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything handed to [`Transport::deliver`], in order.
    pub fn sent(&self) -> Vec<SignedMessage> {
        self.sent.lock().expect("recording transport mutex").clone()
    }
}

impl Transport for RecordingTransport {
    async fn deliver(&self, message: &SignedMessage) -> Result<(), OutboundError> {
        self.sent
            .lock()
            .expect("recording transport mutex")
            .push(message.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_recording_transport_captures_in_order_and_delivers_nothing() {
        let t = RecordingTransport::new();
        assert!(t.sent().is_empty());
        for id in ["<a@x.test>", "<b@x.test>"] {
            t.deliver(&SignedMessage {
                message_id: id.into(),
                envelope_from: "me@x.test".into(),
                envelope_to: vec!["you@y.test".into()],
                raw: b"raw".to_vec(),
            })
            .await
            .unwrap();
        }
        let sent = t.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].message_id, "<a@x.test>");
        assert_eq!(sent[1].message_id, "<b@x.test>", "order is preserved");
    }
}
