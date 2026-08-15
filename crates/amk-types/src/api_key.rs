//! API-key wire types.
//!
//! `ApiKeyPermissions` lives here, in the wire-type crate, for a reason discovered the hard way:
//! when it was absent, two amk-core modules written in parallel each invented their own
//! representation of the same four `label_*_read` flags — and the two disagreed about whether a
//! restricted label needs the permission alone or the permission AND the caller's `include_*`
//! flag. Same crate, same method names, opposite verdicts. One authoritative shape here means the
//! question can only be answered once.
//!
//! Field names and order are generated from `reference/openapi.json`
//! (`type_api-keys:ApiKeyPermissions`), not transcribed.

use serde::{Deserialize, Serialize};

/// Granular permissions for the API key.
///
/// **Absent means unrestricted.** The spec is explicit: *"When ommitted all permissions are
/// granted. Otherwise, only permissions set to true are granted."* So `None` for the whole object
/// grants everything, while a present-but-empty object grants nothing — the most permissive and
/// the most restrictive states differ by a single `null`, which is exactly the kind of distinction
/// that gets flattened by a careless `unwrap_or_default()`. [`KeyGrants`] exists so that
/// distinction has a type rather than a convention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyPermissions {
    /// Read inbox details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_read: Option<bool>,
    /// Create new inboxes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_create: Option<bool>,
    /// Update inbox settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_update: Option<bool>,
    /// Delete inboxes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_delete: Option<bool>,
    /// Read messages. Also required to read threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_read: Option<bool>,
    /// Send messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_send: Option<bool>,
    /// Update message labels. Also required to update threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_update: Option<bool>,
    /// Delete messages. Also required to delete threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_delete: Option<bool>,
    /// Access messages labeled spam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_spam_read: Option<bool>,
    /// Access messages labeled blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_blocked_read: Option<bool>,
    /// Access messages labeled unauthenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_unauthenticated_read: Option<bool>,
    /// Access messages labeled trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_trash_read: Option<bool>,
    /// Read drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_read: Option<bool>,
    /// Create drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_create: Option<bool>,
    /// Update drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_update: Option<bool>,
    /// Delete drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_delete: Option<bool>,
    /// Send drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_send: Option<bool>,
    /// Read webhook configurations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_read: Option<bool>,
    /// Create webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_create: Option<bool>,
    /// Update webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_update: Option<bool>,
    /// Delete webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_delete: Option<bool>,
    /// Read domain details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_read: Option<bool>,
    /// Create domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_create: Option<bool>,
    /// Update domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_update: Option<bool>,
    /// Delete domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_delete: Option<bool>,
    /// Read list entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_entry_read: Option<bool>,
    /// Create list entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_entry_create: Option<bool>,
    /// Delete list entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_entry_delete: Option<bool>,
    /// Read metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_read: Option<bool>,
    /// Read API keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_read: Option<bool>,
    /// Create API keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_create: Option<bool>,
    /// Update API keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_update: Option<bool>,
    /// Delete API keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_delete: Option<bool>,
    /// Read pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_read: Option<bool>,
    /// Create pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_create: Option<bool>,
    /// Delete pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_delete: Option<bool>,
}

/// Whether a credential is restricted at all, and if so by which flags.
///
/// Modelled as an enum rather than an `Option<ApiKeyPermissions>` so that "unrestricted" is a
/// state a reader must handle explicitly, instead of a `None` that invites a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyGrants {
    /// The permissions object was omitted entirely: every permission is granted.
    Unrestricted,
    /// The object was present: only flags explicitly set to `true` are granted.
    Restricted(ApiKeyPermissions),
}

impl KeyGrants {
    pub fn from_wire(permissions: Option<ApiKeyPermissions>) -> Self {
        match permissions {
            None => Self::Unrestricted,
            Some(p) => Self::Restricted(p),
        }
    }

    /// Whether one named flag is granted.
    ///
    /// An unknown name is **not** granted. A restricted credential naming a flag we do not model
    /// must not thereby gain it, and a typo must fail closed.
    pub fn allows(&self, wire_name: &str) -> bool {
        match self {
            Self::Unrestricted => WIRE_NAMES.contains(&wire_name),
            Self::Restricted(p) => p.get(wire_name) == Some(true),
        }
    }
}

impl ApiKeyPermissions {
    /// Look up a flag by its wire name. `None` when the name is not part of the catalog.
    pub fn get(&self, wire_name: &str) -> Option<bool> {
        let v = match wire_name {
            "inbox_read" => self.inbox_read,
            "inbox_create" => self.inbox_create,
            "inbox_update" => self.inbox_update,
            "inbox_delete" => self.inbox_delete,
            "message_read" => self.message_read,
            "message_send" => self.message_send,
            "message_update" => self.message_update,
            "message_delete" => self.message_delete,
            "label_spam_read" => self.label_spam_read,
            "label_blocked_read" => self.label_blocked_read,
            "label_unauthenticated_read" => self.label_unauthenticated_read,
            "label_trash_read" => self.label_trash_read,
            "draft_read" => self.draft_read,
            "draft_create" => self.draft_create,
            "draft_update" => self.draft_update,
            "draft_delete" => self.draft_delete,
            "draft_send" => self.draft_send,
            "webhook_read" => self.webhook_read,
            "webhook_create" => self.webhook_create,
            "webhook_update" => self.webhook_update,
            "webhook_delete" => self.webhook_delete,
            "domain_read" => self.domain_read,
            "domain_create" => self.domain_create,
            "domain_update" => self.domain_update,
            "domain_delete" => self.domain_delete,
            "list_entry_read" => self.list_entry_read,
            "list_entry_create" => self.list_entry_create,
            "list_entry_delete" => self.list_entry_delete,
            "metrics_read" => self.metrics_read,
            "api_key_read" => self.api_key_read,
            "api_key_create" => self.api_key_create,
            "api_key_update" => self.api_key_update,
            "api_key_delete" => self.api_key_delete,
            "pod_read" => self.pod_read,
            "pod_create" => self.pod_create,
            "pod_delete" => self.pod_delete,
            _ => return None,
        };
        Some(v.unwrap_or(false))
    }
}

pub const WIRE_NAMES: [&str; 36] = [
    "inbox_read",
    "inbox_create",
    "inbox_update",
    "inbox_delete",
    "message_read",
    "message_send",
    "message_update",
    "message_delete",
    "label_spam_read",
    "label_blocked_read",
    "label_unauthenticated_read",
    "label_trash_read",
    "draft_read",
    "draft_create",
    "draft_update",
    "draft_delete",
    "draft_send",
    "webhook_read",
    "webhook_create",
    "webhook_update",
    "webhook_delete",
    "domain_read",
    "domain_create",
    "domain_update",
    "domain_delete",
    "list_entry_read",
    "list_entry_create",
    "list_entry_delete",
    "metrics_read",
    "api_key_read",
    "api_key_create",
    "api_key_update",
    "api_key_delete",
    "pod_read",
    "pod_create",
    "pod_delete",
];

/// The four flags that gate restricted labels, paired with the label each one unlocks.
///
/// Kept next to the catalog so a reader can see the pairing is total: every restricted label in
/// [`crate::message::labels::RESTRICTED`] has exactly one flag, and vice versa. A test enforces it.
pub const LABEL_READ_FLAGS: [(&str, &str); 4] = [
    ("label_spam_read", crate::message::labels::SPAM),
    ("label_blocked_read", crate::message::labels::BLOCKED),
    ("label_unauthenticated_read", crate::message::labels::UNAUTHENTICATED),
    ("label_trash_read", crate::message::labels::TRASH),
];

/// The `label_*_read` flag that gates a given label, if that label is restricted.
pub fn label_read_flag(label: &str) -> Option<&'static str> {
    LABEL_READ_FLAGS
        .iter()
        .find(|(_, l)| *l == label)
        .map(|(f, _)| *f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::labels;

    #[test]
    fn catalog_matches_the_spec_exactly() {
        // 36, not 34 — the plan carried a stale count until two reviewers counted the schema.
        assert_eq!(WIRE_NAMES.len(), 36);
        // Every name resolves through get(); a typo in either list breaks this.
        let p = ApiKeyPermissions::default();
        for name in WIRE_NAMES {
            assert_eq!(p.get(name), Some(false), "{name} must be addressable");
        }
        assert_eq!(p.get("not_a_flag"), None);
    }

    #[test]
    fn an_omitted_object_grants_everything_and_an_empty_one_grants_nothing() {
        // The spec: "When ommitted all permissions are granted. Otherwise, only permissions set to
        // true are granted." The two states are one `null` apart, so they get separate assertions.
        let omitted = KeyGrants::from_wire(None);
        let empty = KeyGrants::from_wire(Some(ApiKeyPermissions::default()));
        for name in WIRE_NAMES {
            assert!(omitted.allows(name), "omitted object must grant {name}");
            assert!(!empty.allows(name), "present-but-empty must deny {name}");
        }
    }

    #[test]
    fn an_unknown_flag_name_is_never_granted() {
        // Fails closed in both directions: an unrestricted key cannot be talked into a permission
        // that does not exist, and a restricted one cannot gain one by naming it.
        assert!(!KeyGrants::from_wire(None).allows("label_everything_read"));
        let p = ApiKeyPermissions { inbox_read: Some(true), ..Default::default() };
        assert!(!KeyGrants::from_wire(Some(p)).allows("inbox_write"));
    }

    #[test]
    fn only_true_grants_a_flag() {
        let p = ApiKeyPermissions {
            inbox_read: Some(true),
            inbox_delete: Some(false),
            ..Default::default()
        };
        let g = KeyGrants::from_wire(Some(p));
        assert!(g.allows("inbox_read"));
        assert!(!g.allows("inbox_delete"), "an explicit false is a denial");
        assert!(!g.allows("inbox_create"), "an absent flag is a denial");
    }

    #[test]
    fn absent_flags_are_omitted_from_the_wire_not_sent_as_null() {
        let p = ApiKeyPermissions { message_read: Some(true), ..Default::default() };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"message_read":true}"#);
    }

    #[test]
    fn every_restricted_label_has_exactly_one_read_flag() {
        // The pairing must be total in both directions. When it was not modelled at all, two
        // parallel workers each invented it and disagreed; this is the tripwire for that.
        assert_eq!(LABEL_READ_FLAGS.len(), labels::RESTRICTED.len());
        for label in labels::RESTRICTED {
            let flag = label_read_flag(label)
                .unwrap_or_else(|| panic!("restricted label {label} has no read flag"));
            assert!(WIRE_NAMES.contains(&flag), "{flag} is not in the catalog");
        }
        for (flag, label) in LABEL_READ_FLAGS {
            assert!(labels::is_restricted(label), "{label} is gated but not restricted");
            assert!(WIRE_NAMES.contains(&flag));
        }
        assert_eq!(label_read_flag(labels::RECEIVED), None, "unrestricted labels are not gated");
    }
}
