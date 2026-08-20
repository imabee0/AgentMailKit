//! Signed download tokens: the local stand-in for the reference's CloudFront URLs.
//!
//! `[SPEC:reference/fixtures/06-download-url-expiry.txt]` measured what has to be reproduced.
//! A `download_url` is a CloudFront signed URL (`Expires` + `Key-Pair-Id` + `Signature`), it lasts
//! **~1 hour**, and after expiry the CDN answers **403** with an `AccessDenied` body. Two GETs
//! against the same URL, 939 seconds apart, established both halves.
//!
//! We are not CloudFront, and the fixture's HOST (`cdn.agentmail.to`) and signature scheme cannot
//! be reproduced -- that is a divergence in the URL's opaque interior, not in the contract, which
//! is: a bearer-free URL that stops working at a stated time. What must match is the SHAPE
//! (`download_url` string, `expires_at` timestamp) and the BEHAVIOUR (works, then 403s).
//!
//! # Why the token carries the resource and not just an expiry
//!
//! An expiry-only token is a skeleton key: anyone holding one for their own attachment could
//! rewrite the path and read somebody else's. The MAC covers the blob id, so a token is valid for
//! exactly one object -- swapping the path invalidates the signature rather than authorising a
//! different read.
//!
//! # Why it is unauthenticated
//!
//! Deliberately, and this is the point of a signed URL: it is handed to a client that then fetches
//! it without credentials -- an `<img src>`, a download manager, a mail client. That is the whole
//! reason the reference issues one. The consequences are real and bounded by the TTL: a leaked URL
//! is a leaked object until it expires. It is why the TTL is short, why the token names one blob,
//! and why nothing but blob bytes is ever served this way.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// One hour, matching the ~1h the fixture measured.
pub const DEFAULT_TTL_SECS: u64 = 3600;

/// Why a token was refused.
///
/// The variants exist for LOGGING, not for the client: every one of them is answered with the same
/// 403 and the same body. Telling a caller "expired" rather than "bad signature" hands it an
/// oracle for probing what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
    /// The token is valid but names a different object than the one being fetched.
    WrongResource,
}

/// Mint a token for `blob_id`, valid until `expires_at_unix`.
///
/// The expiry is INSIDE the signed payload rather than a separate query parameter, so a caller
/// cannot extend its own access by editing the URL -- the classic mistake in home-grown signed
/// links, where `?expires=` sits next to `?sig=` and only the latter is covered.
pub fn mint(key: &[u8], blob_id: &str, expires_at_unix: u64) -> String {
    let payload = format!("{blob_id}:{expires_at_unix}");
    let mac = sign(key, payload.as_bytes());
    format!("{}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()), URL_SAFE_NO_PAD.encode(mac))
}

/// Verify a token against `blob_id` and `now_unix`. `Ok(())` means serve the bytes.
///
/// Order matters: the signature is checked BEFORE the expiry and before the resource comparison.
/// Reading fields out of an unauthenticated payload and acting on them -- even to reject -- is how
/// parsing turns into a gadget; nothing here trusts the payload until the MAC says it is ours.
pub fn verify(key: &[u8], token: &str, blob_id: &str, now_unix: u64) -> Result<(), TokenError> {
    let (payload_b64, mac_b64) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| TokenError::Malformed)?;
    let presented = URL_SAFE_NO_PAD
        .decode(mac_b64)
        .map_err(|_| TokenError::Malformed)?;

    // Constant-time. `==` on a MAC leaks how many leading bytes matched, which is enough to forge
    // one byte at a time -- the same reasoning as `api_keys::authenticate`'s single argon2 verify.
    let expected = sign(key, &payload);
    if !bool::from(expected.ct_eq(&presented)) {
        return Err(TokenError::BadSignature);
    }

    // Authenticated from here, so the payload can be parsed.
    let text = std::str::from_utf8(&payload).map_err(|_| TokenError::Malformed)?;
    let (signed_blob, expires) = text.rsplit_once(':').ok_or(TokenError::Malformed)?;
    let expires: u64 = expires.parse().map_err(|_| TokenError::Malformed)?;

    // Resource before expiry: a token for the wrong object is wrong whether or not it is current.
    if signed_blob != blob_id {
        return Err(TokenError::WrongResource);
    }
    if now_unix >= expires {
        return Err(TokenError::Expired);
    }
    Ok(())
}

fn sign(key: &[u8], payload: &[u8]) -> Vec<u8> {
    // `new_from_slice` accepts any length; HMAC itself handles short and long keys. The CALLER is
    // responsible for the key having enough entropy, which is why the config that loads it
    // enforces a minimum rather than leaving it to this function.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"a-test-master-key-of-sufficient-length-0123456789";
    const OTHER: &[u8] = b"a-DIFFERENT-master-key-of-sufficient-length-01234";

    #[test]
    fn a_fresh_token_verifies_for_its_own_blob() {
        let t = mint(KEY, "abc123", 1000);
        assert_eq!(verify(KEY, &t, "abc123", 999), Ok(()));
    }

    #[test]
    fn a_token_stops_working_at_its_expiry() {
        // Fixture 06: GET before expiry 200, GET after 403. This is that boundary.
        let t = mint(KEY, "abc123", 1000);
        assert_eq!(verify(KEY, &t, "abc123", 999), Ok(()));
        assert_eq!(verify(KEY, &t, "abc123", 1000), Err(TokenError::Expired));
        assert_eq!(verify(KEY, &t, "abc123", 5000), Err(TokenError::Expired));
    }

    #[test]
    fn a_token_is_valid_for_exactly_one_object() {
        // The skeleton-key case: without the blob id in the MAC, anyone with a token for their own
        // attachment could rewrite the path and read someone else's.
        let t = mint(KEY, "mine", 1000);
        assert_eq!(verify(KEY, &t, "yours", 999), Err(TokenError::WrongResource));
    }

    #[test]
    fn the_expiry_cannot_be_extended_by_editing_the_token() {
        // The classic home-grown-signed-URL bug: `?expires=` beside `?sig=`, only the latter
        // covered. Here the expiry is inside the signed payload, so rewriting it breaks the MAC.
        let t = mint(KEY, "abc", 1000);
        let (payload_b64, mac) = t.split_once('.').unwrap();
        let payload = URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let tampered = String::from_utf8(payload)
            .unwrap()
            .replace(":1000", ":99999999");
        let forged = format!("{}.{}", URL_SAFE_NO_PAD.encode(tampered.as_bytes()), mac);
        assert_eq!(verify(KEY, &forged, "abc", 5000), Err(TokenError::BadSignature));
    }

    #[test]
    fn a_token_from_another_key_is_refused() {
        let t = mint(OTHER, "abc", 1000);
        assert_eq!(verify(KEY, &t, "abc", 999), Err(TokenError::BadSignature));
    }

    #[test]
    fn malformed_tokens_are_refused_rather_than_panicking() {
        for bad in [
            "",
            ".",
            "no-dot",
            "!!!.!!!",
            "YWJj",
            "YWJj.",
            ".YWJj",
            "YWJj.YWJj",
        ] {
            let r = verify(KEY, bad, "abc", 1);
            assert!(r.is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_blob_id_containing_a_colon_still_round_trips() {
        // The payload is split on the LAST colon, so a colon-bearing id does not shift the
        // expiry field -- splitting on the first would make `a:b` parse as blob `a`, expiry `b`.
        let t = mint(KEY, "sha256:deadbeef", 1000);
        assert_eq!(verify(KEY, &t, "sha256:deadbeef", 999), Ok(()));
        assert_eq!(verify(KEY, &t, "sha256:other", 999), Err(TokenError::WrongResource));
    }

    #[test]
    fn every_refusal_is_distinguishable_here_and_nowhere_else() {
        // The variants exist so a server LOG can say why. The HTTP layer must answer all of them
        // identically -- telling a caller "expired" rather than "bad signature" is an oracle.
        let t = mint(KEY, "abc", 1000);
        let outcomes = [
            verify(KEY, &t, "abc", 5000),
            verify(OTHER, &t, "abc", 999),
            verify(KEY, &t, "zzz", 999),
            verify(KEY, "garbage", "abc", 999),
        ];
        assert!(outcomes.iter().all(|o| o.is_err()));
    }
}
