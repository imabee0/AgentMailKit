//! Building the STARTTLS acceptor from PEM files on disk.
//!
//! Lives here rather than in `amk-cli` because the rustls types stay inside the boundary crate
//! that owns TLS -- `amk-cli` names only the resulting [`TlsAcceptor`]. `scripts/shape-provenance.sh`
//! polices the same boundary for the stalwart-labs crates, and the reasoning is identical.

use std::path::Path;
use std::sync::Arc;

use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// What a caller outside this crate holds. An alias, so `amk-cli` configures TLS without naming
/// `tokio_rustls` -- the same containment `shape-provenance.sh` enforces for the stalwart-labs
/// crates, applied by convention to the one other third-party type that would otherwise leak out.
pub type TlsAcceptorHandle = TlsAcceptor;

/// A certificate or key that is present but unusable.
///
/// Deliberately carries no file CONTENT -- only the path and a category. The key file's bytes are
/// private material, and an error string is the least controlled thing in a program: it reaches
/// logs, transcripts and bug reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsConfigError {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for TlsConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for TlsConfigError {}

/// Load a PEM certificate chain and private key into a ready acceptor.
///
/// Called at STARTUP, so a bad certificate stops the daemon in front of an operator rather than
/// failing on the first sender that offers STARTTLS -- where the symptom is intermittent delivery
/// failure at other people's servers, which is a far more expensive thing to diagnose.
///
/// PKCS#8, PKCS#1 and SEC1 keys are all accepted, because all three are what `openssl` and ACME
/// clients actually emit depending on version and flags. Rejecting two of the three would be a
/// fail-closed that fails on a perfectly good key -- the same trap `amk-outbound::signing`'s
/// `parse_der` records having already hit once.
pub fn acceptor_from_pem(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, TlsConfigError> {
    let err = |p: &Path, reason: &str| TlsConfigError {
        path: p.display().to_string(),
        reason: reason.to_owned(),
    };

    let cert_bytes =
        std::fs::read(cert_path).map_err(|e| err(cert_path, &format!("cannot read: {e}")))?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_bytes.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|_| err(cert_path, "not a PEM certificate chain"))?;
    if certs.is_empty() {
        return Err(err(cert_path, "contains no certificates"));
    }

    let key_bytes =
        std::fs::read(key_path).map_err(|e| err(key_path, &format!("cannot read: {e}")))?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_bytes.as_slice())
        .map_err(|_| err(key_path, "not a PEM private key"))?
        .ok_or_else(|| err(key_path, "contains no private key"))?;

    // The provider is installed by `amk_outbound::smtp::install_crypto_provider` at startup. Both
    // `ring` and `aws-lc-rs` are in this workspace's graph through feature unification, so rustls
    // cannot choose one on its own -- it panics rather than guessing. That panic was found by
    // binary-smoke.sh on the outbound path; this is the same graph.
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        // The message names the CERTIFICATE, never the key file, and never the rustls error's own
        // text -- a key-parsing failure can echo bytes.
        .map_err(|e| err(cert_path, &format!("certificate and key are not a usable pair: {e}")))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("amk-tls-{}-{}", std::process::id(), name));
        std::fs::write(&p, body).expect("write");
        p
    }

    #[test]
    fn a_missing_certificate_names_the_path_and_not_its_contents() {
        // `expect_err` needs `T: Debug` and `TlsAcceptor` deliberately is not one (it holds a
        // server key). Match rather than weaken the type.
        let e = match acceptor_from_pem(
            Path::new("/nonexistent/amk-cert.pem"),
            Path::new("/nonexistent/amk-key.pem"),
        ) {
            Err(e) => e,
            Ok(_) => panic!("a missing file must not yield an acceptor"),
        };
        assert!(e.path.contains("amk-cert.pem"));
        assert!(e.reason.contains("cannot read"));
    }

    #[test]
    fn a_certificate_that_is_not_pem_is_rejected_without_echoing_the_file() {
        let c = tmp("garbage.pem", b"this is not a certificate, it is prose\n");
        let k = tmp("garbage.key", b"neither is this\n");
        let e = match acceptor_from_pem(&c, &k) {
            Err(e) => e,
            Ok(_) => panic!("garbage must not parse"),
        };
        assert!(!e.reason.contains("prose"), "the error echoed file contents: {e}");
        std::fs::remove_file(&c).ok();
        std::fs::remove_file(&k).ok();
    }

    #[test]
    fn an_empty_pem_is_rejected_rather_than_producing_an_empty_chain() {
        // `rustls_pemfile::certs` on an empty file yields Ok(vec![]) -- an acceptor built from it
        // would fail on every handshake instead of at startup.
        let c = tmp("empty.pem", b"");
        let k = tmp("empty.key", b"");
        let e = match acceptor_from_pem(&c, &k) {
            Err(e) => e,
            Ok(_) => panic!("an empty file must not parse"),
        };
        assert!(e.reason.contains("no certificates"), "{e}");
        std::fs::remove_file(&c).ok();
        std::fs::remove_file(&k).ok();
    }
}
