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
            pub fn from_path_segment(segment: &str) -> Result<Self, IdDecodeError> {
                percent_decode_str(segment)
                    .decode_utf8()
                    .map(|s| Self(s.into_owned()))
                    .map_err(|_| IdDecodeError::Utf8)
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
}
