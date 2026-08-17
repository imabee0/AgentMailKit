//! DKIM signing, and the keyring that fails closed.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`, `[SPEC:reference/fixtures/10-dkim-keys.txt]`.
//! `mail-auth` types stay inside this module — the boundary rule in [`crate`]'s own doc, checked by
//! `./scripts/shape-provenance.sh` section 4.

use std::collections::BTreeMap;

use mail_auth::common::crypto::{RsaKey, Sha256};
use mail_auth::common::headers::HeaderWriter;
use mail_auth::dkim::DkimSigner;

use crate::OutboundError;

/// The headers every signature covers.
///
/// Oversigning `From` in particular is what stops a relay adding a second one and having the
/// signature still verify — the header the recipient's client actually shows. The list is fixed
/// rather than derived from what a given message happens to carry, so a message missing `Subject`
/// signs the same set as one that has it.
const SIGNED_HEADERS: &[&str] = &[
    "From",
    "To",
    "Cc",
    "Subject",
    "Date",
    "Message-ID",
    "In-Reply-To",
    "References",
    "MIME-Version",
    "Content-Type",
];

/// One domain's signing material.
///
/// Holds the **DER bytes**, not the parsed `RsaKey`, because `DkimSigner::from_key` takes its key
/// by value and `RsaKey` is not `Clone` — a stored parsed key could be signed with exactly once.
/// The bytes are still parsed once at [`Keyring::insert_der`] and the result discarded, so an
/// unusable key fails when an operator is watching rather than on the first send; the per-send
/// parse is the cost of that API shape, paid deliberately rather than by keeping an unvalidated
/// blob around.
pub struct SigningKey {
    selector: String,
    der: Vec<u8>,
}

impl std::fmt::Debug for SigningKey {
    /// Deliberately opaque. A `#[derive(Debug)]` here would print key material into any log line
    /// that formatted a [`Keyring`], which is the kind of disclosure that survives review because
    /// nothing in the diff looks like a leak.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("selector", &self.selector)
            .field("der", &"<redacted>")
            .finish()
    }
}

/// Parse an RSA private key from DER in **either** PKCS#8 or PKCS#1 form.
///
/// Both are tried because both are what operators actually have. `openssl genpkey -outform DER`
/// emitted PKCS#1 here — verified, not assumed: `from_pkcs8_der` returned `InvalidEncoding` on that
/// exact 1191-byte file and `from_der` accepted it — and a DKIM key exported from another mail
/// server is as likely to be the traditional "RSA PRIVATE KEY" shape as the modern one. Accepting
/// one and rejecting the other would make [`OutboundError::UnusableSigningKey`] fire on a key that
/// is perfectly good, which is a fail-closed that fails on the wrong input.
///
/// PEM is still refused, and deliberately: `mail-auth` wants DER, and a PEM file handed here parses
/// as neither. `CLAUDE.md`'s contract-facts list carries that because it has already cost time.
fn parse_der(der: &[u8]) -> Option<RsaKey<Sha256>> {
    #[allow(deprecated)]
    RsaKey::<Sha256>::from_pkcs8_der(der)
        .or_else(|_| RsaKey::<Sha256>::from_der(der))
        .ok()
}

/// Signing keys by domain. A domain with no key cannot send.
#[derive(Debug, Default)]
pub struct Keyring {
    keys: BTreeMap<String, SigningKey>,
}

impl Keyring {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load one domain's key from **DER** bytes.
    ///
    /// `mail-auth` wants DER, not PEM — recorded in `CLAUDE.md`'s contract-facts list because it
    /// has already cost this project time once. A PEM file handed here fails at load with
    /// [`OutboundError::UnusableSigningKey`] rather than producing a signature that verifies
    /// nowhere.
    ///
    /// The domain is lowercased on the way in and on lookup: DNS names are case-insensitive, so a
    /// key registered for `Example.test` must sign for `example.test`. The same normalisation trap
    /// `InboxId::eq_normalized` exists for.
    pub fn insert_der(
        &mut self,
        domain: &str,
        selector: &str,
        der: &[u8],
    ) -> Result<(), OutboundError> {
        // Parsed and dropped: this is the validation, and it runs at load.
        let _ =
            parse_der(der).ok_or_else(|| OutboundError::UnusableSigningKey(domain.to_owned()))?;
        self.keys.insert(
            domain.to_ascii_lowercase(),
            SigningKey { selector: selector.to_owned(), der: der.to_vec() },
        );
        Ok(())
    }

    fn get(&self, domain: &str) -> Option<&SigningKey> {
        self.keys.get(&domain.to_ascii_lowercase())
    }

    /// Sign `raw` for `domain`, returning the `DKIM-Signature` header line to prepend.
    ///
    /// **Fails closed**: a domain with no key is [`OutboundError::NoSigningKey`], never an unsigned
    /// send. The precedent is `amk-http`'s `AppConfig`, which refuses inbox creation rather than
    /// inventing a domain — same choice, and here the cost of guessing is mail that fails DMARC at
    /// every recipient rather than a wrong default in one row.
    pub fn sign(&self, domain: &str, raw: &[u8]) -> Result<String, OutboundError> {
        let entry = self
            .get(domain)
            .ok_or_else(|| OutboundError::NoSigningKey(domain.to_ascii_lowercase()))?;
        let key = parse_der(&entry.der)
            .ok_or_else(|| OutboundError::UnusableSigningKey(domain.to_ascii_lowercase()))?;
        let signature = DkimSigner::from_key(key)
            .domain(domain.to_ascii_lowercase())
            .selector(entry.selector.clone())
            .headers(SIGNED_HEADERS.iter().copied())
            .sign(raw)
            .map_err(|e| OutboundError::Assembly(format!("DKIM signing failed: {e}")))?;
        Ok(signature.to_header())
    }

    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway 2048-bit RSA key in PKCS#8 DER, generated for this test alone and used nowhere
    /// else. Generated with `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform
    /// DER` and embedded as base64 so the repository carries no `.key` file for the deny-list to
    /// have to cover.
    fn test_key_der() -> Vec<u8> {
        use base64::Engine as _;
        // Every whitespace byte removed, not just the ends: the file is wrapped at 100 columns,
        // and `.trim()` leaves the interior newlines that make the decode fail.
        let wrapped: String = include_str!("testdata/test-signing-key.pkcs8.b64")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(wrapped)
            .expect("the embedded test key is valid base64")
    }

    #[test]
    fn a_domain_with_no_key_fails_closed_rather_than_sending_unsigned() {
        let ring = Keyring::new();
        let err = ring
            .sign("example.test", b"From: a@example.test\r\n\r\nbody")
            .expect_err("no key means no send");
        assert!(
            matches!(err, OutboundError::NoSigningKey(ref d) if d == "example.test"),
            "{err}"
        );
    }

    #[test]
    fn a_pem_key_is_refused_at_load_because_mail_auth_wants_der() {
        let pem = b"-----BEGIN PRIVATE KEY-----\nMIIE...\n-----END PRIVATE KEY-----\n";
        let mut ring = Keyring::new();
        let err = ring
            .insert_der("example.test", "amk", pem)
            .expect_err("PEM is not DER");
        assert!(
            matches!(err, OutboundError::UnusableSigningKey(ref d) if d == "example.test"),
            "{err}"
        );
        // And the failed load left nothing behind that a later send could pick up.
        assert!(ring.sign("example.test", b"x").is_err());
    }

    #[test]
    fn a_der_key_signs_and_the_header_names_the_domain_and_selector() {
        let mut ring = Keyring::new();
        ring.insert_der("example.test", "amk2026", &test_key_der())
            .expect("a real DER key loads");
        let header = ring
            .sign("example.test", b"From: a@example.test\r\nSubject: hi\r\n\r\nbody\r\n")
            .expect("signing succeeds");
        assert!(header.starts_with("DKIM-Signature:"), "{header}");
        assert!(header.contains("d=example.test"), "{header}");
        assert!(header.contains("s=amk2026"), "{header}");
        // `mail-auth` chooses its own order for `h=` and folds the header across lines, so this
        // asserts MEMBERSHIP of the signed set rather than a literal substring. An order assertion
        // here failed against a perfectly correct signature — the code was right and the test was
        // wrong about a detail it had no reason to pin.
        let unfolded: String = header.chars().filter(|c| !c.is_whitespace()).collect();
        for name in SIGNED_HEADERS {
            assert!(unfolded.contains(name), "{name} must be in the signed set: {header}");
        }
        assert!(unfolded.contains("a=rsa-sha256"), "{header}");
    }

    /// DNS names are case-insensitive, so a key registered under one casing must sign for another.
    /// Asserted in both directions rather than one, since a normalisation applied on insert but
    /// not on lookup passes the obvious test and fails the reverse.
    #[test]
    fn domain_lookup_folds_case_in_both_directions() {
        let mut ring = Keyring::new();
        ring.insert_der("Example.TEST", "amk", &test_key_der())
            .unwrap();
        assert!(ring.sign("example.test", b"From: a@b\r\n\r\nx").is_ok());

        let mut other = Keyring::new();
        other
            .insert_der("example.test", "amk", &test_key_der())
            .unwrap();
        assert!(other.sign("EXAMPLE.TEST", b"From: a@b\r\n\r\nx").is_ok());
    }

    /// The `Debug` impl must not print key material — the reason it is hand-written.
    #[test]
    fn debug_output_never_contains_key_material() {
        let mut ring = Keyring::new();
        ring.insert_der("example.test", "amk", &test_key_der())
            .unwrap();
        let rendered = format!("{ring:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("amk"), "the selector is not secret: {rendered}");
        // The DER bytes themselves must not appear in any form a log would carry.
        assert!(!rendered.contains("der: ["), "raw key bytes in Debug output: {rendered}");
    }
}
