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

/// Organization counters, limits and policy.
///
/// **The live response carries seventeen fields; `openapi.json` documents twelve.** The P1
/// conformance gate diffed both and `reference/fixtures/25-p1-gate-conformance.txt` records the
/// result — the live capture wins, as it has every previous time the two disagreed (fixture 19's
/// system labels, and `openapi.json` going 0-for-3 on DELETE statuses).
///
/// Everything the live API emits is here **except two fields, excluded by decision, not oversight**:
/// `billing_plan_id` and `clerk_organization_id`. Both are billing/auth-vendor surface —
/// AgentMailKit ships no billing surface and does not use Clerk — so a self-hosted deployment has
/// nothing truthful to put in them and emitting an invented value would be worse than omitting it.
/// The spec's own `billing_id`/`billing_type`/`billing_subscription_id` are kept for wire
/// compatibility and are likewise always `None`. This is the project's one deliberate divergence
/// from 1:1, and it is recorded here rather than discovered later from a failing diff.
///
/// Every optional is **omitted when absent**, never `null` — so a deployment that configures no
/// limits emits no limit fields, which is correct and will read as a diff against a reference
/// account that has them. The gate seeds them for exactly that reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Organization {
    pub organization_id: OrganizationId,
    pub inbox_count: u64,
    pub domain_count: u64,
    /// The organization's display name. `amk init` sets it; live-only (absent from `openapi.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_limit: Option<u64>,
    /// Send and recipient throttles. Operator configuration here, plan-derived upstream; all four
    /// are live-only and absent from `openapi.json`. Absent = no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_send_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_minute_send_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day_recipient_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_week_recipient_limit: Option<u64>,
    /// Whether open/click tracking is permitted for this organization. Live-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_allowed: Option<bool>,
    /// Documented in `openapi.json` and emitted live. Identifies the authentication mechanism, not
    /// the vendor — distinct from `clerk_organization_id`, which is vendor-specific and excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_type: Option<String>,
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

    fn org() -> Organization {
        Organization {
            organization_id: OrganizationId::new("org"),
            inbox_count: 1,
            domain_count: 0,
            name: None,
            inbox_limit: None,
            domain_limit: None,
            daily_send_limit: None,
            five_minute_send_limit: None,
            first_day_recipient_limit: None,
            first_week_recipient_limit: None,
            tracking_allowed: None,
            authentication_id: None,
            authentication_type: None,
            billing_id: None,
            billing_type: None,
            billing_subscription_id: None,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn billing_fields_are_never_emitted() {
        let s = serde_json::to_string(&org()).unwrap();
        assert!(!s.contains("billing"), "no billing surface: {s}");
    }

    /// The P1 gate found `GET /v0/organizations` emitting 5 of the reference's 17 fields
    /// (`reference/fixtures/25-p1-gate-conformance.txt`). This pins the field set so a future
    /// change cannot quietly drop one back out — and pins the two deliberate exclusions, so that
    /// re-adding billing surface fails a test rather than passing a review.
    #[test]
    fn organization_carries_every_live_field_except_the_two_excluded_by_decision() {
        let mut full = org();
        full.name = Some("Acme".into());
        full.inbox_limit = Some(10);
        full.domain_limit = Some(2);
        full.daily_send_limit = Some(100);
        full.five_minute_send_limit = Some(5);
        full.first_day_recipient_limit = Some(50);
        full.first_week_recipient_limit = Some(500);
        full.tracking_allowed = Some(true);
        full.authentication_id = Some("auth".into());
        full.authentication_type = Some("api_key".into());

        let v: serde_json::Value = serde_json::to_value(&full).unwrap();
        let emitted: std::collections::BTreeSet<&str> =
            v.as_object().unwrap().keys().map(String::as_str).collect();

        // Observed live, 2026-08-16, minus the two excluded. Not a wish list: every name here was
        // read off the reference response by the gate.
        let expected: std::collections::BTreeSet<&str> = [
            "organization_id",
            "inbox_count",
            "domain_count",
            "name",
            "inbox_limit",
            "domain_limit",
            "daily_send_limit",
            "five_minute_send_limit",
            "first_day_recipient_limit",
            "first_week_recipient_limit",
            "tracking_allowed",
            "authentication_id",
            "authentication_type",
            "updated_at",
            "created_at",
        ]
        .into_iter()
        .collect();
        assert_eq!(emitted, expected, "organization field set drifted from the live capture");

        for excluded in ["billing_plan_id", "clerk_organization_id"] {
            assert!(
                !emitted.contains(excluded),
                "{excluded} is excluded by decision — no billing surface, no auth vendor"
            );
        }
    }
}
