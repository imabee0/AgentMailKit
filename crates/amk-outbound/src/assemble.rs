//! MIME assembly and the header sanitiser that guards it.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`. `mail-builder` does the assembly; nothing it
//! produces or consumes crosses this module's public edge — the boundary rule in [`crate`]'s own
//! doc, checked by `./scripts/shape-provenance.sh` section 4.

use std::collections::BTreeMap;

use crate::OutboundError;

/// Headers the envelope owns. A caller-supplied copy of any of these is refused rather than
/// merged, because a second `From` or a smuggled `Bcc` changes who the message is from and who
/// silently receives it — and `mail-builder` would happily write both.
///
/// Matched case-insensitively: header names are case-insensitive per RFC 5322 §3.6.8, so a
/// `from:` that slipped through a case-sensitive check would be exactly as effective as `From:`.
const RESERVED_HEADERS: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "reply-to",
    "subject",
    "message-id",
    "date",
    "in-reply-to",
    "references",
    "dkim-signature",
    "mime-version",
    "content-type",
    "content-transfer-encoding",
];

/// Rejects a caller header map that would inject structure.
///
/// Two vectors, and they are different:
///
/// 1. **A reserved name.** The envelope owns it; a caller copy is refused outright.
/// 2. **CR or LF anywhere in a name or value.** This is header injection: `X-Foo: a\r\nBcc:
///    attacker@evil.test` is one header to a naive writer and two headers to every parser that
///    reads the result. Refused rather than stripped, so the caller learns their input was wrong
///    instead of silently getting a different message than they asked for.
///
/// A NUL is refused with them — it cannot appear in a header and its presence means the value came
/// from somewhere that did not check, which is the same signal.
pub fn check_headers(headers: &BTreeMap<String, String>) -> Result<(), OutboundError> {
    for (name, value) in headers {
        let lowered = name.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) {
            return Err(OutboundError::ForbiddenHeader(name.clone()));
        }
        if has_structural_byte(name) || has_structural_byte(value) {
            return Err(OutboundError::ForbiddenHeader(name.clone()));
        }
        if name.trim().is_empty() {
            return Err(OutboundError::ForbiddenHeader(name.clone()));
        }
    }
    Ok(())
}

fn has_structural_byte(s: &str) -> bool {
    s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn an_ordinary_custom_header_is_allowed() {
        assert!(check_headers(&map(&[("X-Campaign", "spring"), ("X-Trace", "abc123")])).is_ok());
    }

    /// Every reserved name, in the case a caller would most plausibly send and in the case a
    /// case-sensitive check would miss. Asserting the whole list rather than a sample: a name
    /// dropped from `RESERVED_HEADERS` is exactly the kind of edit that looks harmless.
    #[test]
    fn every_reserved_header_is_refused_in_any_case() {
        for name in RESERVED_HEADERS {
            for spelling in [name.to_string(), name.to_uppercase(), title_case(name)] {
                let err = check_headers(&map(&[(&spelling, "x")]))
                    .expect_err("{spelling} must be refused");
                assert!(
                    matches!(err, OutboundError::ForbiddenHeader(ref n) if n == &spelling),
                    "{spelling}: {err}"
                );
            }
        }
    }

    /// The injection vector, in both halves of the pair. A check on the value alone leaves the
    /// name open and vice versa, so both are asserted.
    #[test]
    fn cr_lf_or_nul_in_a_name_or_a_value_is_refused() {
        for (name, value) in [
            ("X-Ok", "a\r\nBcc: attacker@evil.test"),
            ("X-Ok", "a\nBcc: attacker@evil.test"),
            ("X-Ok", "a\rb"),
            ("X-Ok", "a\0b"),
            ("X-Bad\r\nBcc", "x"),
            ("X-Bad\n", "x"),
            ("X-Bad\0", "x"),
        ] {
            assert!(
                check_headers(&map(&[(name, value)])).is_err(),
                "{name:?}: {value:?} must be refused"
            );
        }
    }

    #[test]
    fn an_empty_or_whitespace_name_is_refused() {
        assert!(check_headers(&map(&[("", "x")])).is_err());
        assert!(check_headers(&map(&[("   ", "x")])).is_err());
    }

    fn title_case(s: &str) -> String {
        s.split('-')
            .map(|part| {
                let mut c = part.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join("-")
    }
}
