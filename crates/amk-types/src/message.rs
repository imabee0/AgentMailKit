//! Message resource, attachments, and the send surface.

use crate::{ids::*, list_response, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Send-side recipient fields accept either a single address or a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Addresses {
    One(String),
    Many(Vec<String>),
}

impl Addresses {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Addresses::One(s) => vec![s],
            Addresses::Many(v) => v,
        }
    }
    pub fn is_empty(&self) -> bool {
        match self {
            Addresses::One(s) => s.is_empty(),
            Addresses::Many(v) => v.is_empty(),
        }
    }
}

/// System labels. Restricted ones are hidden from list results unless explicitly requested
/// *and* the credential holds the matching label-read permission.
pub mod labels {
    pub const RECEIVED: &str = "received";
    pub const SENT: &str = "sent";
    pub const UNREAD: &str = "unread";
    pub const SCHEDULED: &str = "scheduled";
    // Restricted:
    pub const SPAM: &str = "spam";
    pub const BLOCKED: &str = "blocked";
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    pub const TRASH: &str = "trash";

    pub const RESTRICTED: [&str; 4] = [SPAM, BLOCKED, UNAUTHENTICATED, TRASH];

    pub fn is_restricted(label: &str) -> bool {
        RESTRICTED.contains(&label)
    }
}

/// Stored attachment metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub attachment_id: AttachmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

/// Attachment fetch response: metadata plus a time-limited signed URL.
///
/// P-1 item 6 measured the upstream TTL at **~1 hour**, after which the URL returns
/// `403 AccessDenied`. Our signed-download endpoint mirrors both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentResponse {
    #[serde(flatten)]
    pub attachment: Attachment,
    pub download_url: String,
    pub expires_at: Timestamp,
}

/// Outbound attachment: exactly one of `content` (base64) or `url`.
///
/// The attachments docs page claims `content` is required; the toolkit schema and the
/// unfetchable-URL error case prove otherwise. The toolkit wins (recorded so it is not
/// re-litigated).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SendAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// Base64 payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Fetched server-side. Must be https and must not redirect into a private range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl SendAttachment {
    /// `content` XOR `url` — the rule the API enforces.
    pub fn has_exactly_one_source(&self) -> bool {
        self.content.is_some() ^ self.url.is_some()
    }
}

/// List-view message (no bodies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageItem {
    /// Live-only field (absent from the SDK type) — emitted for conformance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    /// Live-only field — emitted for conformance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    pub inbox_id: InboxId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<MessageId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Live-only field: the transport-side id, also used in the raw-message CDN path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp_id: Option<String>,
    pub size: u64,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

/// Full message, including bodies and quoted-reply extraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(flatten)]
    pub item: MessageItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// New content with quoted history stripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_html: Option<String>,
}

/// Raw RFC822 fetch response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMessageResponse {
    pub message_id: MessageId,
    pub size: u64,
    pub download_url: String,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SendMessageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Addresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<Addresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Addresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Addresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SendAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

impl SendMessageRequest {
    /// At least one recipient is required — the live API rejects an empty body with
    /// `validation_error` / "to, cc, or bcc must be specified".
    pub fn has_recipient(&self) -> bool {
        [&self.to, &self.cc, &self.bcc]
            .into_iter()
            .flatten()
            .any(|a| !a.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMessageResponse {
    pub message_id: MessageId,
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateMessageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_labels: Option<Addresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_labels: Option<Addresses>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateMessageResponse {
    pub message_id: MessageId,
    pub labels: Vec<String>,
}

list_response!(ListMessagesResponse, messages, MessageItem);

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim list item from the live API (fixture 03), including the three fields the
    /// SDK types do not declare.
    const LIVE_ITEM: &str = r#"{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","inbox_id":"amk-probe@agentmail.to",
        "thread_id":"5d52b0ae-bbf7-45d1-9a27-ee4553a27016",
        "message_id":"<010001a003f312ac-3c80349e-0a97-4915-9dea-0d2764d7729a-000000@email.amazonses.com>",
        "labels":["sent"],"timestamp":"2026-08-15T05:44:16.768Z",
        "from":"AMK Probe <amk-probe@agentmail.to>","to":["amk-probe@agentmail.to"],
        "subject":"AMK probe A2 retry trigger","preview":"retry clock start\n\n--\nSent via AgentMail",
        "smtp_id":"vcem56ic5kope1vt3uhg6vjpjgsn989vucc9p4d4","size":1241,
        "updated_at":"2026-08-15T05:44:16.768Z","created_at":"2026-08-15T05:44:16.768Z"}"#;

    #[test]
    fn message_item_round_trips_the_live_shape_including_live_only_fields() {
        let item: MessageItem = serde_json::from_str(LIVE_ITEM).unwrap();
        assert!(item.message_id.is_bracketed());
        assert_eq!(item.smtp_id.as_deref(), Some("vcem56ic5kope1vt3uhg6vjpjgsn989vucc9p4d4"));
        let out: serde_json::Value = serde_json::to_value(&item).unwrap();
        let expected: serde_json::Value = serde_json::from_str(LIVE_ITEM).unwrap();
        assert_eq!(out, expected, "emitting fewer/more fields breaks the conformance diff");
    }

    #[test]
    fn send_response_matches_live() {
        let live = r#"{"message_id":"<010001a003ef6970-1732f5b7@email.amazonses.com>",
            "thread_id":"c1197a89-02ad-4bdf-8461-c03136b481aa"}"#;
        let r: SendMessageResponse = serde_json::from_str(live).unwrap();
        assert!(r.message_id.is_bracketed());
    }

    #[test]
    fn addresses_accept_single_or_list() {
        let one: Addresses = serde_json::from_str(r#""a@b.c""#).unwrap();
        let many: Addresses = serde_json::from_str(r#"["a@b.c","d@e.f"]"#).unwrap();
        assert_eq!(one.into_vec().len(), 1);
        assert_eq!(many.into_vec().len(), 2);
    }

    #[test]
    fn empty_send_request_has_no_recipient() {
        let req: SendMessageRequest = serde_json::from_str("{}").unwrap();
        assert!(!req.has_recipient(), "empty body must fail validation");
        let req: SendMessageRequest = serde_json::from_str(r#"{"bcc":"x@y.z"}"#).unwrap();
        assert!(req.has_recipient(), "bcc alone satisfies the rule");
    }

    #[test]
    fn send_attachment_enforces_content_xor_url() {
        let base = SendAttachment::default();
        assert!(!base.has_exactly_one_source(), "neither source is invalid");
        let both = SendAttachment {
            content: Some("Zm9v".into()),
            url: Some("https://x/y".into()),
            ..Default::default()
        };
        assert!(!both.has_exactly_one_source(), "both sources is invalid");
        let ok = SendAttachment { content: Some("Zm9v".into()), ..Default::default() };
        assert!(ok.has_exactly_one_source());
    }

    #[test]
    fn restricted_labels_are_exactly_the_four() {
        assert!(labels::is_restricted("spam"));
        assert!(labels::is_restricted("unauthenticated"));
        assert!(!labels::is_restricted("received"));
        assert!(!labels::is_restricted("unread"));
    }

    #[test]
    fn full_message_flattens_item_fields() {
        let live = format!(
            r#"{{{}, "text":"body","extracted_text":"body"}}"#,
            LIVE_ITEM
                .trim()
                .trim_start_matches('{')
                .trim_end_matches('}')
        );
        let msg: Message = serde_json::from_str(&live).unwrap();
        assert_eq!(msg.item.labels, vec!["sent"]);
        assert_eq!(msg.extracted_text.as_deref(), Some("body"));
        let out = serde_json::to_value(&msg).unwrap();
        assert!(out.get("inbox_id").is_some(), "flattened item fields stay top-level");
    }
}
