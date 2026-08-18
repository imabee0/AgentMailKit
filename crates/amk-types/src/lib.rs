//! Wire types for the AgentMail-compatible API surface.
//!
//! # Shape provenance (hard rule)
//!
//! Every type here derives from AgentMail's own artifacts — `reference/openapi.json`, the
//! official Fern SDKs, and the live captures in `reference/fixtures/`. **Nothing in this crate
//! may derive from Stalwart or JMAP**, not even as an optional or legacy field: this crate is
//! the contract the whole server is judged against, and a plausible-looking foreign shape is a
//! defect regardless of how reasonable it looks.
//!
//! Where the live API and the published spec disagree, **the live capture wins** and the
//! divergence is recorded next to the type.
//!
//! # Live divergences already folded in
//!
//! * Responses carry `organization_id` / `pod_id` (and `smtp_id` on messages) that the SDK
//!   types omit — we emit them, because the conformance diff compares against the live API.
//! * Optional members are omitted entirely when absent, never `null` or `""`.
//! * Timestamps are RFC 3339 with **exactly three** fractional digits and a `Z` suffix.

pub mod api_key;
pub mod error;
pub mod event;
pub mod ids;
pub mod inbox;
pub mod message;
pub mod page;
pub mod pod;
pub mod thread;

pub use api_key::{ApiKeyPermissions, KeyGrants};
pub use error::{ErrorCode, ErrorEnvelope, GatewayError, ValidationIssue};
pub use event::{Delivery, Event, EventType, Send};
pub use ids::{
    ApiKeyId, AttachmentId, DomainId, DraftId, EventId, InboxId, MessageId, OrganizationId, PodId,
    ThreadId, WebhookId,
};
pub use inbox::{CreateInboxRequest, Inbox, Metadata, MetadataValue, UpdateInboxRequest};
pub use message::{
    Attachment, Message, MessageItem, ReplyAllMessageRequest, ReplyToMessageRequest,
    SendMessageRequest, SendMessageResponse,
};
pub use page::{Cursor, ListParams};
pub use pod::{Identity, Organization, Pod, ScopeType};
pub use thread::{Thread, ThreadItem};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An RFC 3339 timestamp rendered exactly as the upstream API renders it:
/// millisecond precision with a `Z` suffix, e.g. `2026-08-15T05:40:16.825Z`.
///
/// chrono's default serializer elides a zero fraction, which would make our output drift from
/// the reference on whole-second timestamps — hence the explicit format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    /// Now, truncated to milliseconds (see [`Timestamp::truncate`]).
    pub fn now() -> Self {
        Self::truncate(Utc::now())
    }

    /// Truncate to millisecond precision.
    ///
    /// The wire format carries exactly three fractional digits, so a `Timestamp` is kept
    /// wire-exact at all times: a value in memory always equals the value that will be
    /// serialized, and every timestamp round-trips to itself. Without this, sub-millisecond
    /// precision from `Utc::now()` or a database column would silently vanish on the way out
    /// and compare unequal on the way back.
    pub fn truncate(dt: DateTime<Utc>) -> Self {
        let millis = dt.timestamp_millis();
        Self(DateTime::from_timestamp_millis(millis).unwrap_or(dt))
    }

    pub fn into_inner(self) -> DateTime<Utc> {
        self.0
    }

    pub fn to_rfc3339_millis(self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Millis, true)
    }
}

impl From<DateTime<Utc>> for Timestamp {
    /// Truncates to milliseconds to preserve the wire-exactness invariant.
    fn from(dt: DateTime<Utc>) -> Self {
        Self::truncate(dt)
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_rfc3339_millis())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339_millis())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&raw)
            .map(|dt| Timestamp(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_renders_millis_and_z_like_the_live_api() {
        let live = "2026-08-15T05:40:16.825Z";
        let ts: Timestamp = serde_json::from_str(&format!("\"{live}\"")).unwrap();
        assert_eq!(serde_json::to_string(&ts).unwrap(), format!("\"{live}\""));
    }

    #[test]
    fn whole_second_timestamps_keep_three_decimals() {
        // chrono's default would emit `...:16Z`; the reference emits `...:16.000Z`.
        let ts: Timestamp = serde_json::from_str("\"2026-08-15T05:40:16Z\"").unwrap();
        assert_eq!(serde_json::to_string(&ts).unwrap(), "\"2026-08-15T05:40:16.000Z\"");
    }

    #[test]
    fn minted_timestamps_are_wire_exact_and_round_trip() {
        // Utc::now() carries nanoseconds; a Timestamp must already be what it will serialize as,
        // so that in-memory equality and wire equality never diverge.
        let t = Timestamp::now();
        let back: Timestamp = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(t, back);
        assert_eq!(t.into_inner().timestamp_subsec_nanos() % 1_000_000, 0);
    }

    #[test]
    fn offset_timestamps_normalize_to_utc() {
        let ts: Timestamp = serde_json::from_str("\"2026-08-15T07:40:16.825+02:00\"").unwrap();
        assert_eq!(serde_json::to_string(&ts).unwrap(), "\"2026-08-15T05:40:16.825Z\"");
    }
}
