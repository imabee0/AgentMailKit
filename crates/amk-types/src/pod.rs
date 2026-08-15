//! Pod, Organization and Identity — the scope spine.
//!
//! `GET /v0/auth/me` returns an [`Identity`] describing what the presented credential can reach.
//! Live capture (fixture 01) for an org-scoped key:
//! `{"api_key_id":…,"organization_id":…,"scope_id":…,"scope_type":"organization"}` — note that
//! `pod_id`/`inbox_id` are simply **absent** for an org-scoped key, not null.

use crate::{ids::*, list_response, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeType {
    Organization,
    Pod,
    Inbox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_id: Option<ApiKeyId>,
    pub organization_id: OrganizationId,
    pub scope_id: String,
    pub scope_type: ScopeType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_id: Option<InboxId>,
}

impl Identity {
    /// True when this credential may reach every pod and inbox in the organization.
    pub fn is_organization_scoped(&self) -> bool {
        self.scope_type == ScopeType::Organization
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pod {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    pub pod_id: PodId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub name: String,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreatePodRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// Organization counters and limits.
///
/// The upstream type also carries `billing_id` / `billing_type` / `billing_subscription_id`.
/// AgentMailKit ships **no billing surface**, so those fields exist for wire compatibility and
/// are always omitted; `inbox_limit` / `domain_limit` are operator configuration (absent = no
/// limit), never plan-derived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Organization {
    pub organization_id: OrganizationId,
    pub inbox_count: u64,
    pub domain_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_subscription_id: Option<String>,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

list_response!(ListPodsResponse, pods, Pod);

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `GET /v0/auth/me` body (fixture 01).
    const LIVE_IDENTITY: &str = r#"{"api_key_id":"3c5547b5-e7ff-474e-9871-83e82251568e",
        "organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "scope_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea","scope_type":"organization"}"#;

    #[test]
    fn identity_round_trips_the_live_shape() {
        let id: Identity = serde_json::from_str(LIVE_IDENTITY).unwrap();
        assert!(id.is_organization_scoped());
        assert_eq!(id.scope_id, id.organization_id.as_str());
        let out: serde_json::Value = serde_json::to_value(&id).unwrap();
        let expected: serde_json::Value = serde_json::from_str(LIVE_IDENTITY).unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn org_scoped_identity_omits_pod_and_inbox() {
        let id: Identity = serde_json::from_str(LIVE_IDENTITY).unwrap();
        let s = serde_json::to_string(&id).unwrap();
        assert!(!s.contains("pod_id") && !s.contains("inbox_id"));
    }

    #[test]
    fn pod_parses_the_live_create_response() {
        // Verbatim from the throwaway-pod creation.
        let live = r#"{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
            "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","client_id":"amk-probe-pod-1",
            "name":"amk-probe-throwaway","updated_at":"2026-08-15T05:39:29.971Z",
            "created_at":"2026-08-15T05:39:29.971Z"}"#;
        let pod: Pod = serde_json::from_str(live).unwrap();
        assert_eq!(pod.name, "amk-probe-throwaway");
        let out: serde_json::Value = serde_json::to_value(&pod).unwrap();
        assert_eq!(out, serde_json::from_str::<serde_json::Value>(live).unwrap());
    }

    #[test]
    fn billing_fields_are_never_emitted() {
        let org = Organization {
            organization_id: OrganizationId::new("org"),
            inbox_count: 1,
            domain_count: 0,
            inbox_limit: None,
            domain_limit: None,
            billing_id: None,
            billing_type: None,
            billing_subscription_id: None,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        };
        let s = serde_json::to_string(&org).unwrap();
        assert!(!s.contains("billing"), "no billing surface: {s}");
    }
}
