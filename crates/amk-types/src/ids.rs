//! Identifier newtypes.
//!
//! Formats are observed, not guessed — see `reference/fixtures/03-id-formats.http`:
//! * `inbox_id` IS the email address (`amk-probe@agentmail.to`), used verbatim as a path param.
//! * `message_id` IS an RFC 5322 angle-bracket Message-ID (`<...@email.amazonses.com>`) —
//!   header-derived, not minted, and therefore contains `<`, `>` and `@`, all of which must be
//!   percent-encoded in a path segment.
//! * `pod_id` / `thread_id` / `attachment_id` are UUIDs; `domain_id` is the domain name.
//! * `event_id` has TWO observed forms (UUID and 32-hex-no-dashes), so it stays an opaque string.

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Failure decoding an identifier out of a URL path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdDecodeError {
    #[error("path segment is not valid UTF-8 after percent-decoding")]
    Utf8,
    #[error("identifier contains a NUL byte")]
    Nul,
}

/// Whether a decoded identifier carries a byte no identifier may contain.
///
/// **Exactly one rule: a NUL byte.** `%00` percent-decodes to a perfectly valid UTF-8 string, so
/// the UTF-8 check above passes it — and PostgreSQL `text` cannot represent `0x00` at all. A
/// NUL-bearing id therefore fails at *parameter encoding* (`SQLSTATE 22021`), before any
/// comparison, surfacing as a database error where every other unresolvable id returns not-found.
/// That is two defects: a 500 on caller-controlled input, and a side channel that distinguishes
/// "malformed" from "absent" in a codebase whose contract requires denial to mask as `not_found`.
///
/// Rejected, never sanitised. Stripping the byte would silently make two different ids equal, and
/// this project has already spent three review rounds on the difference between rejecting a value
/// and redefining what makes two values the same.
///
/// **Deliberately no wider than NUL.** Control characters, newlines and over-long ids are all
/// arguable, but `message_id` is an RFC 5322 grammar that permits a great deal, no fixture governs
/// any of them, and an over-broad rule here rejects legitimate ids — a worse failure than the one
/// being fixed. `[ASSUMED]`, and narrow on purpose.
pub fn has_forbidden_byte(s: &str) -> bool {
    s.contains('\0')
}

/// Characters that must be escaped inside a single URL path segment.
/// `<`, `>` and `@` matter for `message_id`; the rest are standard path-segment reserved chars.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'@')
    .add(b'+');

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            /// Percent-encoded form for use as a single URL path segment.
            pub fn to_path_segment(&self) -> String {
                utf8_percent_encode(&self.0, PATH_SEGMENT).to_string()
            }
            /// Inverse of [`Self::to_path_segment`].
            ///
            /// Handlers normally receive an already-decoded parameter (axum percent-decodes path
            /// params), so this is not the request path. It exists because the encoding has to be
            /// *reversible* — `message_id` carries `<`, `>` and `@`, and any code that builds a URL
            /// must be able to read its own output back, in tests and in the conformance harness.
            ///
            /// Rejects a NUL byte — see [`has_forbidden_byte`]. `%00` survives percent-decoding as
            /// valid UTF-8, and this is one of only two wire-reachable ways an untrusted byte
            /// becomes an id; the other is the page-token cursor decoder, which does not route
            /// through here and needs its own check.
            pub fn from_path_segment(segment: &str) -> Result<Self, IdDecodeError> {
                let decoded = percent_decode_str(segment)
                    .decode_utf8()
                    .map_err(|_| IdDecodeError::Utf8)?;
                if has_forbidden_byte(&decoded) {
                    return Err(IdDecodeError::Nul);
                }
                Ok(Self(decoded.into_owned()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new_random() -> Self {
                Self(Uuid::new_v4())
            }
            pub fn to_path_segment(&self) -> String {
                self.0.to_string()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }
    };
}

string_id! {
    /// The inbox's email address. Doubles as the path parameter.
    InboxId
}
string_id! {
    /// RFC 5322 Message-ID *including* angle brackets, e.g. `<abc@email.amazonses.com>`.
    MessageId
}
string_id! {
    /// The domain name itself (e.g. `example.com`).
    DomainId
}
string_id! {
    /// Svix endpoint id, e.g. `ep_3HwIKMKzmbCirmu4p74HOjg3PZq`.
    WebhookId
}
string_id! {
    /// Opaque; two live formats observed (UUID and 32-hex), so never parsed.
    EventId
}
string_id! {
    /// Opaque organization identifier (UUID in practice, but not relied upon).
    OrganizationId
}
string_id! {
    /// Opaque draft identifier.
    DraftId
}
string_id! {
    /// Opaque API key identifier.
    ApiKeyId
}

uuid_id! {
    /// Pod identifier (UUID v4).
    PodId
}
uuid_id! {
    /// Thread identifier (UUID v4).
    ThreadId
}
uuid_id! {
    /// Attachment identifier (UUID v4).
    AttachmentId
}

impl InboxId {
    /// The form used for every comparison: ASCII-lowercased.
    ///
    /// Observed (`reference/fixtures/18-inbox-case-normalization.txt`): AgentMail lowercases the
    /// username at creation — `{"username":"AmkCase"}` is stored and returned as
    /// `amkcase@agentmail.to` — and resolves lookups case-insensitively, with `AMKCASE@…` and
    /// `AmKcAsE@…` both returning 200.
    ///
    /// So exact comparison is a defect, not a conservative default: a path parameter
    /// `Victim@agentmail.to` would miss an exact-match scope rule that upstream would have
    /// matched, and the divergence lands on exactly the input a caller controls.
    ///
    /// ASCII-only folding on purpose. Unicode `to_lowercase` would fold characters an address
    /// never contains (domains are punycode by the time they reach us) and can equate distinct
    /// strings under some locales' rules — a source of false equality in a security comparison.
    pub fn normalized(&self) -> Self {
        Self(self.0.to_ascii_lowercase())
    }

    /// Case-insensitive equality, matching how the live API resolves an inbox.
    pub fn eq_normalized(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl MessageId {
    /// True when the value is wrapped in angle brackets, as every observed id is.
    pub fn is_bracketed(&self) -> bool {
        self.0.starts_with('<') && self.0.ends_with('>')
    }

    /// The id without its angle brackets (the raw `addr-spec`).
    pub fn unbracketed(&self) -> &str {
        self.0
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(&self.0)
    }

    /// Wrap a bare `addr-spec` in angle brackets, leaving already-bracketed input alone.
    pub fn bracketed(s: impl AsRef<str>) -> Self {
        let s = s.as_ref();
        if s.starts_with('<') && s.ends_with('>') {
            Self(s.to_owned())
        } else {
            Self(format!("<{s}>"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live capture, reference/fixtures/03-id-formats.http.
    const OBSERVED: &str =
        "<010001a003ef6970-1732f5b7-5c17-485b-92fa-52ccd78a0004-000000@email.amazonses.com>";
    const OBSERVED_ENCODED: &str =
        "%3C010001a003ef6970-1732f5b7-5c17-485b-92fa-52ccd78a0004-000000%40email.amazonses.com%3E";

    #[test]
    fn message_id_path_encoding_matches_live_capture() {
        let id = MessageId::new(OBSERVED);
        assert_eq!(id.to_path_segment(), OBSERVED_ENCODED);
        assert!(id.is_bracketed());
    }

    #[test]
    fn message_id_round_trips_through_path_segment() {
        let id = MessageId::new(OBSERVED);
        let decoded = percent_encoding::percent_decode_str(&id.to_path_segment())
            .decode_utf8()
            .unwrap()
            .to_string();
        assert_eq!(decoded, OBSERVED);
    }

    #[test]
    fn message_id_bracket_helpers() {
        let bare = "abc@example.com";
        assert_eq!(MessageId::bracketed(bare).as_str(), "<abc@example.com>");
        assert_eq!(MessageId::bracketed("<abc@example.com>").as_str(), "<abc@example.com>");
        assert_eq!(MessageId::new(OBSERVED).unbracketed(), &OBSERVED[1..OBSERVED.len() - 1]);
    }

    #[test]
    fn inbox_ids_compare_case_insensitively_per_fixture_18() {
        // Live: {"username":"AmkCase"} was stored and returned as "amkcase@agentmail.to", and
        // GET resolved AMKCASE@… and AmKcAsE@… with 200. Exact comparison would miss exactly the
        // input a caller controls, so the scope layer compares normalized forms.
        let stored = InboxId::new("amkcase@agentmail.to");
        for variant in [
            "amkcase@agentmail.to",
            "AMKCASE@agentmail.to",
            "AmKcAsE@agentmail.to",
        ] {
            let incoming = InboxId::new(variant);
            assert!(stored.eq_normalized(&incoming), "{variant} must resolve to the same inbox");
            assert_eq!(incoming.normalized(), stored, "{variant} must normalize to the stored id");
        }
        // Different inboxes stay different — folding case must not merge distinct addresses.
        assert!(!stored.eq_normalized(&InboxId::new("amkcase2@agentmail.to")));
        assert!(!stored.eq_normalized(&InboxId::new("amkcase@other.to")));
    }

    #[test]
    fn normalization_is_ascii_only() {
        // Unicode folding can equate strings that are distinct addresses under some locales'
        // rules; a false equality here is a cross-inbox read. Turkish dotted capital I is the
        // classic case: Unicode lowercases it to "i̇" (i + combining dot), ASCII leaves it alone.
        let turkish = InboxId::new("İ@example.com");
        assert_eq!(turkish.normalized().as_str(), "İ@example.com");
        assert!(!turkish.eq_normalized(&InboxId::new("i@example.com")));
    }

    #[test]
    fn inbox_id_is_an_email_and_encodes_its_at_sign() {
        let id = InboxId::new("amk-probe@agentmail.to");
        assert_eq!(id.to_path_segment(), "amk-probe%40agentmail.to");
    }

    #[test]
    fn plus_addressing_is_encoded_not_treated_as_space() {
        let id = InboxId::new("user+tag@example.com");
        assert_eq!(id.to_path_segment(), "user%2Btag%40example.com");
    }

    #[test]
    fn ids_serialize_transparently() {
        assert_eq!(serde_json::to_string(&InboxId::new("a@b.c")).unwrap(), "\"a@b.c\"");
        let t = ThreadId::from(uuid::uuid!("c1197a89-02ad-4bdf-8461-c03136b481aa"));
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"c1197a89-02ad-4bdf-8461-c03136b481aa\"");
    }

    /// `%00` percent-decodes to valid UTF-8, so the UTF-8 check alone passes it — and PostgreSQL
    /// `text` cannot hold `0x00`, so it fails at parameter encoding (`22021`) rather than
    /// resolving to nothing. Rejected here instead, at the boundary, for every id type.
    #[test]
    fn a_nul_byte_is_rejected_at_the_path_boundary_for_every_id_type() {
        for encoded in ["%00", "abc%00def", "%00abc", "abc%00"] {
            assert!(
                matches!(InboxId::from_path_segment(encoded), Err(IdDecodeError::Nul)),
                "InboxId must reject {encoded:?} as Nul"
            );
            assert!(
                matches!(MessageId::from_path_segment(encoded), Err(IdDecodeError::Nul)),
                "MessageId must reject {encoded:?} as Nul"
            );
            assert!(
                matches!(ApiKeyId::from_path_segment(encoded), Err(IdDecodeError::Nul)),
                "ApiKeyId must reject {encoded:?} as Nul"
            );
        }
        // The error must be distinguishable from a UTF-8 failure: they have different causes and a
        // caller that collapses them cannot report either accurately.
        assert!(matches!(InboxId::from_path_segment("%FF%FE"), Err(IdDecodeError::Utf8)));
    }

    /// The regression that matters more than the fix: an over-broad rejection breaks real ids.
    /// `message_id` is an RFC 5322 angle-bracket value carrying `<`, `>` and `@`
    /// (`reference/fixtures/03-id-formats.http`), and `inbox_id` is an email address that folds
    /// ASCII case (`reference/fixtures/18-inbox-case-normalization.txt`). Both must still round
    /// trip untouched.
    #[test]
    fn rejecting_nul_does_not_disturb_any_legitimate_id() {
        let msg = MessageId::new("<0100019891f3ab2c-abc@email.amazonses.com>");
        assert_eq!(MessageId::from_path_segment(&msg.to_path_segment()).expect("round trips"), msg);
        let inbox = InboxId::new("AmkCase+tag@agentmail.to");
        assert_eq!(
            InboxId::from_path_segment(&inbox.to_path_segment()).expect("round trips"),
            inbox
        );
        // Control characters other than NUL are NOT rejected — narrow on purpose, since no fixture
        // governs them and message_id's grammar is permissive.
        assert!(MessageId::from_path_segment("a%09b").is_ok(), "tab must not be rejected");
    }
}
