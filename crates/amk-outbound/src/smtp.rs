//! SMTP delivery: direct-to-MX and smarthost, behind [`crate::Transport`].
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`. `mail-send` types stay inside this module — the
//! boundary rule in [`crate`]'s own doc. Tests never construct a live send: they use
//! [`crate::RecordingTransport`].

use std::collections::BTreeMap;
use std::time::Duration;

use hickory_resolver::proto::rr::RData;
use hickory_resolver::Resolver;
use mail_send::smtp::message::Message as SmtpMessage;
use mail_send::SmtpClientBuilder;

use crate::{OutboundError, SignedMessage, Transport};

/// How a live [`SmtpTransport`] reaches the next hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpMode {
    /// Resolve each recipient domain's MX and deliver on port 25.
    DirectMx,
    /// Relay every envelope through one configured host:port.
    Smarthost { host: String, port: u16 },
}

/// The production [`Transport`]: `mail-send` to a smarthost or the recipient MX.
///
/// Never used by `cargo test`. Tests inject [`crate::RecordingTransport`] so nothing leaves the
/// process. A missing DKIM key is refused *before* this type is called (`build_signed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpTransport {
    mode: SmtpMode,
}

impl SmtpTransport {
    pub fn direct_mx() -> Self {
        Self { mode: SmtpMode::DirectMx }
    }

    pub fn smarthost(host: impl Into<String>, port: u16) -> Self {
        Self { mode: SmtpMode::Smarthost { host: host.into(), port } }
    }

    pub fn mode(&self) -> &SmtpMode {
        &self.mode
    }
}

impl Transport for SmtpTransport {
    async fn deliver(&self, message: &SignedMessage) -> Result<(), OutboundError> {
        match &self.mode {
            SmtpMode::Smarthost { host, port } => {
                deliver_to_host(host, *port, *port == 465, message).await
            }
            SmtpMode::DirectMx => deliver_direct_mx(message).await,
        }
    }
}

/// Install the process-wide rustls crypto provider. Idempotent; call it before serving.
///
/// # The panic this prevents
///
/// rustls 0.23 picks a provider from its own enabled features, and refuses -- by panicking, deep
/// inside `ClientConfig::builder` -- when it cannot tell which one is meant. This workspace
/// compiles BOTH: `sqlx`, `hickory-resolver` and `mail-send` each depend on rustls, and cargo
/// feature unification turns on `ring` and `aws-lc-rs` together. Nothing in the dependency tree is
/// wrong; the combination simply has no default.
///
/// So `amkd --role api` panicked on the first outbound send, in the tokio worker serving the
/// request, with "Could not automatically determine the process-level CryptoProvider". Found by
/// `scripts/binary-smoke.sh` on the run that first exercised a real send through the compiled
/// binary -- unit tests never reached it because `RecordingTransport` builds no TLS connector, so
/// the whole `amk-outbound` suite passed against a binary that could not send.
///
/// Returns `true` if this call installed the provider, `false` if one was already present. Both
/// are success: a second call losing the race is exactly what idempotent means here.
pub fn install_crypto_provider() -> bool {
    rustls::crypto::ring::default_provider()
        .install_default()
        .is_ok()
}

/// The value [`crate::AppState`](not here) holds: either the recording fake or a live SMTP hop.
///
/// An enum rather than a trait object because [`Transport::deliver`] uses RPITIT and is not
/// object-safe. Tests construct [`Self::Recording`]; production constructs [`Self::Smtp`].
#[derive(Debug, Clone)]
pub enum OutboundTransport {
    Recording(crate::RecordingTransport),
    Smtp(SmtpTransport),
}

impl OutboundTransport {
    pub fn direct_mx() -> Self {
        Self::Smtp(SmtpTransport::direct_mx())
    }

    pub fn smarthost(host: impl Into<String>, port: u16) -> Self {
        Self::Smtp(SmtpTransport::smarthost(host, port))
    }

    pub fn recording(inner: crate::RecordingTransport) -> Self {
        Self::Recording(inner)
    }

    pub fn as_recording(&self) -> Option<&crate::RecordingTransport> {
        match self {
            Self::Recording(t) => Some(t),
            Self::Smtp(_) => None,
        }
    }
}

impl Transport for OutboundTransport {
    async fn deliver(&self, message: &SignedMessage) -> Result<(), OutboundError> {
        match self {
            Self::Recording(t) => t.deliver(message).await,
            Self::Smtp(t) => t.deliver(message).await,
        }
    }
}

async fn deliver_direct_mx(message: &SignedMessage) -> Result<(), OutboundError> {
    let mut by_domain: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rcpt in &message.envelope_to {
        let domain = rcpt
            .rsplit_once('@')
            .map(|(_, d)| d.to_ascii_lowercase())
            .ok_or_else(|| OutboundError::Delivery(format!("recipient {rcpt:?} has no domain")))?;
        by_domain.entry(domain).or_default().push(rcpt.clone());
    }
    for (domain, rcpts) in by_domain {
        let hop = SignedMessage {
            message_id: message.message_id.clone(),
            envelope_from: message.envelope_from.clone(),
            envelope_to: rcpts,
            raw: message.raw.clone(),
        };
        let hosts = mx_hosts(&domain).await?;
        let mut last = OutboundError::Delivery(format!("no MX for {domain}"));
        for host in hosts {
            match deliver_to_host(&host, 25, false, &hop).await {
                Ok(()) => {
                    last = OutboundError::Delivery(String::new());
                    break;
                }
                Err(e) => last = e,
            }
        }
        if !matches!(&last, OutboundError::Delivery(s) if s.is_empty()) {
            return Err(last);
        }
    }
    Ok(())
}

async fn mx_hosts(domain: &str) -> Result<Vec<String>, OutboundError> {
    // RFC 5321 §5.1: if the lookup fails or returns nothing, deliver to the domain itself.
    let resolver = match Resolver::builder_tokio() {
        Ok(b) => match b.build() {
            Ok(r) => r,
            Err(_) => return Ok(vec![domain.to_owned()]),
        },
        Err(_) => return Ok(vec![domain.to_owned()]),
    };
    let Ok(lookup) = resolver.mx_lookup(domain).await else {
        return Ok(vec![domain.to_owned()]);
    };
    let mut ranked: Vec<(u16, String)> = Vec::new();
    for rec in lookup.answers() {
        if let RData::MX(mx) = &rec.data {
            let host = mx.exchange.to_ascii();
            let host = host.trim_end_matches('.').to_owned();
            if !host.is_empty() {
                ranked.push((mx.preference, host));
            }
        }
    }
    ranked.sort_by_key(|(pref, _)| *pref);
    let hosts: Vec<String> = ranked.into_iter().map(|(_, h)| h).collect();
    if hosts.is_empty() {
        Ok(vec![domain.to_owned()])
    } else {
        Ok(hosts)
    }
}

/// Guards the provider install on the delivery path itself.
///
/// `amkd` also calls [`install_crypto_provider`] at startup, which is where it belongs -- failing
/// at boot beats failing on the first user request. This second call is not redundancy for its own
/// sake: it makes the panic unreachable for ANY caller, including `amk-http`'s own tests and any
/// future binary, rather than depending on every entry point remembering. `Once` makes the cost a
/// single relaxed load after the first send.
static PROVIDER: std::sync::Once = std::sync::Once::new();

async fn deliver_to_host(
    host: &str,
    port: u16,
    implicit_tls: bool,
    message: &SignedMessage,
) -> Result<(), OutboundError> {
    PROVIDER.call_once(|| {
        install_crypto_provider();
    });
    let builder = SmtpClientBuilder::new(host, port)
        .map_err(OutboundError::Delivery)?
        .implicit_tls(implicit_tls)
        .allow_invalid_certs()
        .timeout(Duration::from_secs(30));
    if implicit_tls {
        builder
            .connect()
            .await
            .map_err(|e| OutboundError::Delivery(e.to_string()))?
            .send(smtp_message(message))
            .await
            .map_err(|e| OutboundError::Delivery(e.to_string()))
    } else {
        match builder.connect().await {
            Ok(mut client) => client
                .send(smtp_message(message))
                .await
                .map_err(|e| OutboundError::Delivery(e.to_string())),
            Err(_) if port == 25 => builder
                .connect_plain()
                .await
                .map_err(|e| OutboundError::Delivery(e.to_string()))?
                .send(smtp_message(message))
                .await
                .map_err(|e| OutboundError::Delivery(e.to_string())),
            Err(e) => Err(OutboundError::Delivery(e.to_string())),
        }
    }
}

fn smtp_message(message: &SignedMessage) -> SmtpMessage<'_> {
    SmtpMessage::new(
        message.envelope_from.as_str(),
        message.envelope_to.iter().map(String::as_str),
        message.raw.as_slice(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_mx_and_smarthost_are_distinct_configurable_modes() {
        let direct = SmtpTransport::direct_mx();
        assert_eq!(direct.mode(), &SmtpMode::DirectMx);
        let relay = SmtpTransport::smarthost("relay.example.test", 587);
        assert_eq!(
            relay.mode(),
            &SmtpMode::Smarthost { host: "relay.example.test".into(), port: 587 }
        );
        assert_ne!(direct.mode(), relay.mode());
    }

    #[test]
    fn outbound_transport_recording_is_the_test_seam() {
        let rec = crate::RecordingTransport::new();
        let wrapped = OutboundTransport::recording(rec.clone());
        assert!(wrapped.as_recording().is_some());
        assert!(OutboundTransport::direct_mx().as_recording().is_none());
        assert!(OutboundTransport::smarthost("h", 25)
            .as_recording()
            .is_none());
    }
}
