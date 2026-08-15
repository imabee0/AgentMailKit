//! Inbox resource.
//!
//! `inbox_id` IS the email address (live capture 03). Creation with a taken username returns
//! `already_exists` at **HTTP 403** with `suggestions[]` — see [`crate::ErrorCode::AlreadyExists`].

use crate::{ids::*, list_response, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Metadata values are scalars; `null` in an update request means "delete this key".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Bool(bool),
}

pub type Metadata = BTreeMap<String, MetadataValue>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inbox {
    /// Live responses include this even though the SDK type omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    pub pod_id: PodId,
    pub inbox_id: InboxId,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateInboxRequest {
    /// Randomly generated when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Defaults to the server's primary domain when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Idempotency key for creation: replay returns the original resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// The three states `UpdateInboxRequest::metadata` can be in on the wire.
///
/// `openapi.json` types the field `oneOf [UpdateMetadata, null]` and says, verbatim: *"Keys you
/// include are added or overwritten; keys you omit are left unchanged. To remove a single key,
/// send it with a null value. **To clear all metadata, send `metadata` as null.** Sending an empty
/// object is rejected; use null to clear."*
///
/// So absent, `null`, and `{…}` mean three different things. Modelled as an enum rather than
/// `Option<Option<…>>` because the states have names worth writing down, matching how
/// [`crate::api_key::KeyGrants`] already handles the same absent-versus-empty trap.
///
/// The bug this replaces: the field was `Option<BTreeMap<…>>` with `#[serde(default)]`, under
/// which `{"metadata": null}` and `{}` both deserialize to `None` — three wire states collapsed
/// into two, while the doc comment above the field asserted the distinction existed. A frozen
/// wire-type crate documenting behaviour its type cannot represent is worse than one that stays
/// silent, because downstream trusts it.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum MetadataUpdate {
    /// Field absent: metadata untouched.
    #[default]
    Unchanged,
    /// `"metadata": null` — clear every key.
    Clear,
    /// `"metadata": {…}` — merge; a key mapped to `null` deletes just that key.
    Merge(BTreeMap<String, Option<MetadataValue>>),
}

impl MetadataUpdate {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl Serialize for MetadataUpdate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            // `Unchanged` is skipped by `skip_serializing_if`, so it never reaches the wire
            // through the request type; serializing one directly is meaningless rather than
            // illegal, and `null` is the closest honest rendering.
            Self::Unchanged | Self::Clear => s.serialize_none(),
            Self::Merge(m) => m.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for MetadataUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Absence never reaches here — `#[serde(default)]` on the field supplies `Unchanged` —
        // which is precisely what makes the three states separable. A present `null` does reach
        // here, and becomes `Clear`.
        Ok(match Option::<BTreeMap<String, Option<MetadataValue>>>::deserialize(d)? {
            None => Self::Clear,
            Some(m) => Self::Merge(m),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateInboxRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Absent, `null` and `{…}` are three distinct requests — see [`MetadataUpdate`].
    #[serde(default, skip_serializing_if = "MetadataUpdate::is_unchanged")]
    pub metadata: MetadataUpdate,
}

list_response!(
    /// `{count, limit?, next_page_token?, inboxes: [...]}`
    ListInboxesResponse,
    inboxes,
    Inbox
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim create-inbox response from the live API (fixture 03).
    const LIVE: &str = r#"{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","inbox_id":"amk-probe@agentmail.to",
        "email":"amk-probe@agentmail.to","client_id":"amk-probe-inbox-1","display_name":"AMK Probe",
        "updated_at":"2026-08-15T05:39:38.948Z","created_at":"2026-08-15T05:39:38.948Z"}"#;

    #[test]
    fn parses_and_reemits_the_live_inbox_shape() {
        let inbox: Inbox = serde_json::from_str(LIVE).unwrap();
        assert_eq!(inbox.inbox_id.as_str(), "amk-probe@agentmail.to");
        assert_eq!(inbox.email, inbox.inbox_id.as_str(), "inbox_id IS the email");

        let out: serde_json::Value = serde_json::to_value(&inbox).unwrap();
        let expected: serde_json::Value = serde_json::from_str(LIVE).unwrap();
        assert_eq!(out, expected, "must round-trip the live shape exactly");
    }

    #[test]
    fn absent_metadata_is_omitted_not_null() {
        let inbox: Inbox = serde_json::from_str(LIVE).unwrap();
        let s = serde_json::to_string(&inbox).unwrap();
        assert!(!s.contains("metadata"), "absent optionals must be omitted: {s}");
    }

    #[test]
    fn update_request_distinguishes_delete_key_from_absent() {
        let req: UpdateInboxRequest =
            serde_json::from_str(r#"{"metadata":{"keep":"v","drop":null}}"#).unwrap();
        let MetadataUpdate::Merge(m) = req.metadata else {
            panic!("a present object is a merge");
        };
        assert!(m["drop"].is_none(), "null value means delete the key");
        assert!(m["keep"].is_some());
    }

    #[test]
    fn update_metadata_has_three_distinct_wire_states() {
        // openapi.json, verbatim: "To remove a single key, send it with a null value. To clear all
        // metadata, send `metadata` as null." Absent, null and {…} are three different requests,
        // and the previous Option<BTreeMap> collapsed the first two into None.
        let absent: UpdateInboxRequest = serde_json::from_str(r#"{"display_name":"x"}"#).unwrap();
        let clear: UpdateInboxRequest = serde_json::from_str(r#"{"metadata":null}"#).unwrap();
        let merge: UpdateInboxRequest = serde_json::from_str(r#"{"metadata":{"a":"1"}}"#).unwrap();

        assert_eq!(absent.metadata, MetadataUpdate::Unchanged);
        assert_eq!(clear.metadata, MetadataUpdate::Clear);
        assert!(matches!(merge.metadata, MetadataUpdate::Merge(_)));
        assert_ne!(
            absent.metadata, clear.metadata,
            "leaving metadata alone and wiping it are different requests"
        );
    }

    #[test]
    fn update_metadata_round_trips_each_state_to_the_right_json() {
        let omitted = serde_json::to_string(&UpdateInboxRequest {
            display_name: Some("x".into()),
            metadata: MetadataUpdate::Unchanged,
        })
        .unwrap();
        assert!(!omitted.contains("metadata"), "unchanged is omitted entirely: {omitted}");

        let cleared = serde_json::to_string(&UpdateInboxRequest {
            display_name: None,
            metadata: MetadataUpdate::Clear,
        })
        .unwrap();
        assert_eq!(cleared, r#"{"metadata":null}"#, "clear is the ONE place null is correct");

        let merged = serde_json::to_string(&UpdateInboxRequest {
            display_name: None,
            metadata: MetadataUpdate::Merge([("a".to_string(), None)].into_iter().collect()),
        })
        .unwrap();
        assert_eq!(merged, r#"{"metadata":{"a":null}}"#, "per-key null deletes that key");
    }

    #[test]
    fn metadata_accepts_string_number_and_bool() {
        let m: Metadata = serde_json::from_str(r#"{"s":"x","n":3,"b":true}"#).unwrap();
        assert!(matches!(m["s"], MetadataValue::String(_)));
        assert!(matches!(m["n"], MetadataValue::Number(_)));
        assert!(matches!(m["b"], MetadataValue::Bool(_)));
    }
}
