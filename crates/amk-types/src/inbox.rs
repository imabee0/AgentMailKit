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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateInboxRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Merge semantics: a key mapped to `null` deletes that key; the whole field `null`
    /// clears all metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Option<MetadataValue>>>,
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
        let m = req.metadata.unwrap();
        assert!(m["drop"].is_none(), "null value means delete the key");
        assert!(m["keep"].is_some());
    }

    #[test]
    fn metadata_accepts_string_number_and_bool() {
        let m: Metadata =
            serde_json::from_str(r#"{"s":"x","n":3,"b":true}"#).unwrap();
        assert!(matches!(m["s"], MetadataValue::String(_)));
        assert!(matches!(m["n"], MetadataValue::Number(_)));
        assert!(matches!(m["b"], MetadataValue::Bool(_)));
    }
}
