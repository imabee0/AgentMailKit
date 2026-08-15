//! Webhook / WebSocket event envelopes.
//!
//! Envelope shape (live captures 09, 09b, 17):
//! `{"type":"event","event_type":<name>,"event_id":<id>,<payload-key>:{…}}`
//! where the payload key varies by type: `message`+`thread`, `send`, `delivery`, `bounce`,
//! `complaint`, `reject`, `domain`.
//!
//! Two observations that constrain the implementation:
//! * `event_id` has **two** live formats — UUID (`message.delivered`, `message.complained`) and
//!   32-hex-no-dashes (`message.sent`) — so it stays an opaque string and is never parsed.
//! * The restricted variants (`.spam` / `.blocked` / `.unauthenticated`) **replace**
//!   `message.received` for a subscriber; the plain event is not also delivered.

use crate::{ids::*, message::Message, thread::ThreadItem, Timestamp};
use serde::{Deserialize, Serialize};

/// The 10 event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "message.received")]
    MessageReceived,
    #[serde(rename = "message.received.spam")]
    MessageReceivedSpam,
    #[serde(rename = "message.received.blocked")]
    MessageReceivedBlocked,
    #[serde(rename = "message.received.unauthenticated")]
    MessageReceivedUnauthenticated,
    #[serde(rename = "message.sent")]
    MessageSent,
    #[serde(rename = "message.delivered")]
    MessageDelivered,
    #[serde(rename = "message.bounced")]
    MessageBounced,
    #[serde(rename = "message.complained")]
    MessageComplained,
    #[serde(rename = "message.rejected")]
    MessageRejected,
    #[serde(rename = "domain.verified")]
    DomainVerified,
}

impl EventType {
    /// Every event type, exhaustively.
    ///
    /// Exists so downstream crates iterate *this* rather than a hand-copied array of their own.
    /// `amk-core` kept one, and its "totality" test iterated the copy — so when a reviewer added a
    /// variant in a sandbox, the test passed while the new event fell through a `_ => None` arm and
    /// became subscribable by a credential holding no label permission. A tripwire that iterates
    /// the thing it is meant to catch drifting cannot fire.
    ///
    /// [`EventType::ordinal`] is wildcard-free, so adding a variant fails to COMPILE until it is
    /// listed here too. A runtime length check could not manage that: the array and the enum would
    /// both have to be edited for it to notice, which is the edit it is guarding.
    pub const ALL: [EventType; 10] = [
        EventType::MessageReceived,
        EventType::MessageReceivedSpam,
        EventType::MessageReceivedBlocked,
        EventType::MessageReceivedUnauthenticated,
        EventType::MessageSent,
        EventType::MessageDelivered,
        EventType::MessageBounced,
        EventType::MessageComplained,
        EventType::MessageRejected,
        EventType::DomainVerified,
    ];

    /// This variant's index in [`EventType::ALL`]. Wildcard-free on purpose — see `ALL`.
    pub const fn ordinal(self) -> usize {
        match self {
            EventType::MessageReceived => 0,
            EventType::MessageReceivedSpam => 1,
            EventType::MessageReceivedBlocked => 2,
            EventType::MessageReceivedUnauthenticated => 3,
            EventType::MessageSent => 4,
            EventType::MessageDelivered => 5,
            EventType::MessageBounced => 6,
            EventType::MessageComplained => 7,
            EventType::MessageRejected => 8,
            EventType::DomainVerified => 9,
        }
    }

    /// The restricted receive variants require the matching label-read permission and replace
    /// `message.received` rather than duplicating it.
    pub fn is_restricted_receive(self) -> bool {
        matches!(
            self,
            EventType::MessageReceivedSpam
                | EventType::MessageReceivedBlocked
                | EventType::MessageReceivedUnauthenticated
        )
    }
}

/// Fields shared by every delivery-side payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    pub inbox_id: InboxId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Send {
    #[serde(flatten)]
    pub reference: DeliveryRef,
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    #[serde(flatten)]
    pub reference: DeliveryRef,
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipient {
    pub address: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bounce {
    #[serde(flatten)]
    pub reference: DeliveryRef,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_type: Option<String>,
    pub recipients: Vec<Recipient>,
}

/// Complaint payload. Live capture (fixture 17, Outlook JMRP): `type: "abuse"`, and
/// **`sub_type` absent** — omit it rather than emitting an empty string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Complaint {
    #[serde(flatten)]
    pub reference: DeliveryRef,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_type: Option<String>,
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reject {
    #[serde(flatten)]
    pub reference: DeliveryRef,
    pub reason: String,
}

/// The per-type payload, discriminated by `event_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum EventPayload {
    #[serde(rename = "message.received")]
    MessageReceived {
        message: Box<Message>,
        thread: Box<ThreadItem>,
    },
    #[serde(rename = "message.received.spam")]
    MessageReceivedSpam {
        message: Box<Message>,
        thread: Box<ThreadItem>,
    },
    #[serde(rename = "message.received.blocked")]
    MessageReceivedBlocked {
        message: Box<Message>,
        thread: Box<ThreadItem>,
    },
    #[serde(rename = "message.received.unauthenticated")]
    MessageReceivedUnauthenticated {
        message: Box<Message>,
        thread: Box<ThreadItem>,
    },
    #[serde(rename = "message.sent")]
    MessageSent { send: Send },
    #[serde(rename = "message.delivered")]
    MessageDelivered { delivery: Delivery },
    #[serde(rename = "message.bounced")]
    MessageBounced { bounce: Bounce },
    #[serde(rename = "message.complained")]
    MessageComplained { complaint: Complaint },
    #[serde(rename = "message.rejected")]
    MessageRejected { reject: Reject },
    #[serde(rename = "domain.verified")]
    DomainVerified { domain: serde_json::Value },
}

impl EventPayload {
    pub fn event_type(&self) -> EventType {
        use EventPayload as P;
        match self {
            P::MessageReceived { .. } => EventType::MessageReceived,
            P::MessageReceivedSpam { .. } => EventType::MessageReceivedSpam,
            P::MessageReceivedBlocked { .. } => EventType::MessageReceivedBlocked,
            P::MessageReceivedUnauthenticated { .. } => EventType::MessageReceivedUnauthenticated,
            P::MessageSent { .. } => EventType::MessageSent,
            P::MessageDelivered { .. } => EventType::MessageDelivered,
            P::MessageBounced { .. } => EventType::MessageBounced,
            P::MessageComplained { .. } => EventType::MessageComplained,
            P::MessageRejected { .. } => EventType::MessageRejected,
            P::DomainVerified { .. } => EventType::DomainVerified,
        }
    }
}

/// Constant `"event"` discriminator carried by every envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvelopeKind {
    Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: EnvelopeKind,
    pub event_id: EventId,
    #[serde(flatten)]
    pub payload: EventPayload,
}

impl Event {
    pub fn new(event_id: EventId, payload: EventPayload) -> Self {
        Self { kind: EnvelopeKind::Event, event_id, payload }
    }
    pub fn event_type(&self) -> EventType {
        self.payload.event_type()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_names_every_variant_exactly_once() {
        use super::EventType;
        // ordinal() is the compile-time half: a new variant breaks the build until it is listed.
        // This is the runtime half — that ALL's ORDER matches those ordinals, so a duplicated entry
        // cannot mask an omission while keeping the length at 10 and defeating a length check.
        for (i, ev) in EventType::ALL.into_iter().enumerate() {
            assert_eq!(ev.ordinal(), i, "ALL[{i}] is out of order or duplicated");
        }
        assert_eq!(EventType::ALL.len(), 10);
    }

    use super::*;

    /// Verbatim `message.complained` capture (fixture 17) — the Outlook JMRP complaint.
    const LIVE_COMPLAINT: &str = r#"{
      "type":"event","event_type":"message.complained",
      "event_id":"6813392d-e351-4392-ae72-87354eca35b4",
      "complaint":{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","inbox_id":"amk-probe@agentmail.to",
        "thread_id":"a538760f-1bcc-424b-93d3-1e67928d39df",
        "message_id":"<010001a0040a1ced-8d2b8e2c-8149-4801-93d5-39d134fd90d6-000000@email.amazonses.com>",
        "recipients":["nathant1902@outlook.com"],"timestamp":"2026-08-15T06:09:56.956Z",
        "type":"abuse"}}"#;

    /// Verbatim `message.sent` capture (fixture 09) — note the 32-hex event_id.
    const LIVE_SENT: &str = r#"{
      "type":"event","event_type":"message.sent","event_id":"1a030f46544cd4764b70e51e3cdff899",
      "send":{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","inbox_id":"amk-probe@agentmail.to",
        "thread_id":"5d52b0ae-bbf7-45d1-9a27-ee4553a27016",
        "message_id":"<010001a003f312ac-3c80349e@email.amazonses.com>",
        "recipients":["amk-probe@agentmail.to"],"timestamp":"2026-08-15T05:44:16.768Z"}}"#;

    #[test]
    fn complaint_round_trips_and_omits_absent_sub_type() {
        let ev: Event = serde_json::from_str(LIVE_COMPLAINT).unwrap();
        assert_eq!(ev.event_type(), EventType::MessageComplained);
        match &ev.payload {
            EventPayload::MessageComplained { complaint } => {
                assert_eq!(complaint.kind, "abuse");
                assert!(complaint.sub_type.is_none(), "live capture omits sub_type");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let out: serde_json::Value = serde_json::to_value(&ev).unwrap();
        let expected: serde_json::Value = serde_json::from_str(LIVE_COMPLAINT).unwrap();
        assert_eq!(out, expected);
        assert!(!serde_json::to_string(&ev).unwrap().contains("sub_type"));
    }

    #[test]
    fn sent_event_round_trips_with_hex_event_id() {
        let ev: Event = serde_json::from_str(LIVE_SENT).unwrap();
        assert_eq!(ev.event_type(), EventType::MessageSent);
        assert_eq!(ev.event_id.as_str().len(), 32, "32-hex form, not a UUID");
        let out: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(out, serde_json::from_str::<serde_json::Value>(LIVE_SENT).unwrap());
    }

    #[test]
    fn event_ids_of_both_live_formats_are_accepted() {
        // UUID form (delivered/complained) and 32-hex form (sent) must both survive round-trip.
        for id in [
            "6813392d-e351-4392-ae72-87354eca35b4",
            "1a030f46544cd4764b70e51e3cdff899",
        ] {
            let parsed: EventId = serde_json::from_str(&format!("\"{id}\"")).unwrap();
            assert_eq!(parsed.as_str(), id);
        }
    }

    #[test]
    fn restricted_receive_variants_are_flagged() {
        assert!(EventType::MessageReceivedUnauthenticated.is_restricted_receive());
        assert!(EventType::MessageReceivedSpam.is_restricted_receive());
        assert!(!EventType::MessageReceived.is_restricted_receive());
        assert!(!EventType::MessageSent.is_restricted_receive());
    }

    #[test]
    fn payload_key_is_named_per_event_type() {
        let ev: Event = serde_json::from_str(LIVE_SENT).unwrap();
        let v = serde_json::to_value(&ev).unwrap();
        assert!(v.get("send").is_some(), "message.sent carries `send`");
        assert!(v.get("delivery").is_none());
    }
}
