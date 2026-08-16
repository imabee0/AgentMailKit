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

use std::fmt;

use crate::ids::{ApiKeyId, InboxId, OrganizationId, PodId};
use crate::{list_response, Timestamp};
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
    /// Read the key owner's email address. **Live-only** — emitted by the reference API and absent
    /// from `openapi.json`'s 36-property schema, found by the P1 conformance gate
    /// (`reference/fixtures/25-p1-gate-conformance.txt`). Third time the live capture has beaten
    /// the spec, after fixture 19's system labels and the DELETE statuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_email: Option<bool>,
    /// Read the key owner's profile. Live-only, same provenance as [`Self::owner_email`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_profile: Option<bool>,
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
            "owner_email" => self.owner_email,
            "owner_profile" => self.owner_profile,
            _ => return None,
        };
        Some(v.unwrap_or(false))
    }
}

pub const WIRE_NAMES: [&str; 38] = [
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
    "owner_email",
    "owner_profile",
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

/// An API key as returned by the read endpoints.
///
/// **The secret is not in this type.** Only [`CreateApiKeyResponse`] carries an `api_key` field,
/// and the reference API returns it exactly once, at creation. A key that could be re-read would
/// make every later leak permanent, so the absence here is the security property — do not add it
/// "for convenience".
///
/// `pod_id` and `inbox_id` say where the key is bound: both absent is an organization-scoped key.
/// They are typed as ids rather than the bare `string` `openapi.json` declares, matching how
/// [`crate::inbox::Inbox`] already types the same values.
///
/// Fields and optionality from `reference/openapi.json` (`type_api-keys:ApiKey`), plus
/// `organization_id` from `reference/fixtures/23-inbox-defaults-and-key-shape.txt:43`.
///
/// This doc used to read *"no live capture exists … `organization_id` is not modelled here …
/// **if a capture ever shows it, add it then**"*. Fixture 23 is that capture — a real
/// `POST /v0/api-keys` response, `organization_id` first field — and this is the "add it then".
/// Recorded rather than quietly edited because the instruction worked exactly as intended: the
/// field was withheld while unevidenced and added the moment evidence arrived, which is the
/// opposite of inventing it. The gap it left was caught by the pre-dispatch review of
/// `.claude/contracts/amk-http.md`, not by the probe that produced the evidence — writing a
/// fixture and acting on it are two obligations, and only the first was met at the time.
///
/// `Option`, matching [`crate::pod::Pod`]'s own `organization_id`: fixture 23 observed it on the
/// **create** response, and no capture of `GET /v0/api-keys` exists to say whether the list
/// envelope's items carry it too. Optional-and-omitted claims exactly what was seen; making it
/// required would claim more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    pub api_key_id: ApiKeyId,
    /// Leading identifying segment of the key, safe to display and to index for O(1) lookup.
    pub prefix: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_id: Option<InboxId>,
    /// Absent on a key that has never been used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_at: Option<Timestamp>,
    /// Absent grants everything; see [`KeyGrants::from_wire`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ApiKeyPermissions>,
    pub created_at: Timestamp,
}

/// `openapi.json` marks both fields optional (`type_api-keys:CreateApiKeyRequest`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateApiKeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Omitted grants everything the parent holds; present-but-empty grants nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ApiKeyPermissions>,
}

/// The one response that carries the secret.
///
/// Deliberately a separate type from [`ApiKey`] rather than an `Option<String>` on it: making the
/// secret unrepresentable outside creation is stronger than remembering to leave it `None`. Note
/// it also has **no `used_at`** — a key returned at creation has never been used.
/// `Debug` is **hand-written and redacts `api_key`** — see the impl below. It is deliberately not
/// derived: this type exists to carry a plaintext credential, and `#[derive(Debug)]` would print it
/// into any log line, `assert_eq!` failure, panic message or CI transcript that ever formatted the
/// struct. The field's own doc comment said "never log it" while the derive did exactly that; a
/// comment is not a mechanism. Found by the review panel on the amk-store api-keys dispatch, and
/// independently re-raised by a second lens as "a live footgun for whoever wires this into
/// amk-http next" — which is the point: the next crate to touch this type must not have to
/// remember.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateApiKeyResponse {
    /// Observed first in the live create response, `reference/fixtures/23-...txt:43`. See
    /// [`ApiKey::organization_id`] for why it is `Option` rather than required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    pub api_key_id: ApiKeyId,
    /// The secret, returned exactly once. Never log it, never store it in plaintext — the store
    /// keeps an argon2id hash and looks up by `prefix`.
    pub api_key: String,
    pub prefix: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_id: Option<InboxId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ApiKeyPermissions>,
    pub created_at: Timestamp,
}

impl fmt::Debug for CreateApiKeyResponse {
    /// Every field except the secret, which prints as `<redacted>`.
    ///
    /// `prefix` is deliberately kept: it is the non-secret, displayable half of the key and the
    /// thing you actually need in a log to identify *which* key a line is about. Redacting the
    /// whole struct would make it useless for diagnosis and invite someone to reach for the raw
    /// field instead.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateApiKeyResponse")
            .field("api_key_id", &self.api_key_id)
            .field("api_key", &"<redacted>")
            .field("prefix", &self.prefix)
            .field("name", &self.name)
            .field("pod_id", &self.pod_id)
            .field("inbox_id", &self.inbox_id)
            .field("permissions", &self.permissions)
            .field("created_at", &self.created_at)
            .finish()
    }
}

list_response!(ListApiKeysResponse, api_keys, ApiKey);

#[cfg(test)]
mod tests {
    /// The secret must never appear in `Debug` output. This type exists to carry a plaintext
    /// credential exactly once, and `Debug` is the format that reaches logs, `assert_eq!` failure
    /// messages, panic output and CI transcripts without anyone deciding to send it there.
    ///
    /// Asserted on the RENDERED STRING rather than by reading the impl: a derived `Debug`
    /// reinstated by a careless edit would pass any test that only checks the type compiles.
    #[test]
    fn debug_redacts_the_secret_but_keeps_the_prefix() {
        let resp = CreateApiKeyResponse {
            organization_id: Some(OrganizationId::new("org-1")),
            api_key_id: ApiKeyId::new("3c5547b5-e7ff-474e-9871-83e82251568e"),
            api_key: "am_us_SUPERSECRETVALUE0000000000000000".into(),
            prefix: "am_us_SUPERSECR".into(),
            name: "test".into(),
            pod_id: None,
            inbox_id: None,
            permissions: None,
            created_at: Timestamp::now(),
        };
        let rendered = format!("{resp:?}");
        assert!(
            !rendered.contains("SUPERSECRETVALUE"),
            "the plaintext secret must not appear in Debug output: {rendered}"
        );
        assert!(
            rendered.contains("<redacted>"),
            "the secret field must render redacted: {rendered}"
        );
        // The non-secret half stays, or the output is useless for saying WHICH key a log line is
        // about — and someone reaches for the raw field instead.
        assert!(rendered.contains("am_us_SUPERSECR"), "prefix must survive: {rendered}");
        assert!(rendered.contains("3c5547b5"), "api_key_id must survive: {rendered}");
    }

    use super::*;
    use crate::message::labels;

    #[test]
    fn catalog_matches_the_spec_exactly() {
        // 38, not 36, and not the 34 the plan carried before two reviewers counted the schema.
        // 36 is what `openapi.json` documents; the LIVE api emits two more (`owner_email`,
        // `owner_profile`), observed by the P1 conformance gate. The live capture wins — this is
        // the same rule fixture 19 and the DELETE statuses already established.
        assert_eq!(WIRE_NAMES.len(), 38);
        // Every name resolves through get(); a typo in either list breaks this.
        let p = ApiKeyPermissions::default();
        for name in WIRE_NAMES {
            assert_eq!(p.get(name), Some(false), "{name} must be addressable");
        }
        assert_eq!(p.get("not_a_flag"), None);
    }

    /// `WIRE_NAMES` must name **every** field of `ApiKeyPermissions`, not merely resolve through
    /// `get()`.
    ///
    /// The forward-only check was not enough. `amk-core::derive_child` bounds a child key by
    /// iterating `WIRE_NAMES`, so a field present on the struct but missing from the array is
    /// invisible to the escalation check — a reviewer demonstrated it by adding one field and
    /// watching a child hold a permission its parent lacked while every test still passed.
    /// Comparing the serialized key set closes the reverse direction, which is the one that
    /// decides whether escalation is detectable at all.
    #[test]
    fn wire_names_covers_every_field_of_the_struct() {
        // No `..Default::default()`: a field added to the struct fails to compile here, and one
        // added to both struct and literal but not to WIRE_NAMES fails the comparison.
        let all_true = ApiKeyPermissions {
            inbox_read: Some(true),
            inbox_create: Some(true),
            inbox_update: Some(true),
            inbox_delete: Some(true),
            message_read: Some(true),
            message_send: Some(true),
            message_update: Some(true),
            message_delete: Some(true),
            label_spam_read: Some(true),
            label_blocked_read: Some(true),
            label_unauthenticated_read: Some(true),
            label_trash_read: Some(true),
            draft_read: Some(true),
            draft_create: Some(true),
            draft_update: Some(true),
            draft_delete: Some(true),
            draft_send: Some(true),
            webhook_read: Some(true),
            webhook_create: Some(true),
            webhook_update: Some(true),
            webhook_delete: Some(true),
            domain_read: Some(true),
            domain_create: Some(true),
            domain_update: Some(true),
            domain_delete: Some(true),
            list_entry_read: Some(true),
            list_entry_create: Some(true),
            list_entry_delete: Some(true),
            metrics_read: Some(true),
            api_key_read: Some(true),
            api_key_create: Some(true),
            api_key_update: Some(true),
            api_key_delete: Some(true),
            pod_read: Some(true),
            pod_create: Some(true),
            pod_delete: Some(true),
            owner_email: Some(true),
            owner_profile: Some(true),
        };
        let json = serde_json::to_value(&all_true).unwrap();
        let mut on_wire: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        let mut catalog: Vec<String> = WIRE_NAMES.iter().map(|s| s.to_string()).collect();
        on_wire.sort();
        catalog.sort();
        assert_eq!(on_wire, catalog, "WIRE_NAMES and the struct fields must be the same set");
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

    fn key_fields(json: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn an_org_scoped_unused_key_omits_every_absent_optional() {
        // Optionals are omitted, never null and never "" — the single likeliest source of a
        // conformance diff against the reference API.
        let key = ApiKey {
            organization_id: Some(OrganizationId::new("org-1")),
            api_key_id: ApiKeyId::new("ak_1"),
            prefix: "am_us_abcd".into(),
            name: "probe".into(),
            pod_id: None,
            inbox_id: None,
            used_at: None,
            permissions: None,
            created_at: Timestamp::now(),
        };
        let s = serde_json::to_string(&key).unwrap();
        assert_eq!(
            key_fields(&s),
            [
                "api_key_id",
                "created_at",
                "name",
                "organization_id",
                "prefix"
            ],
            "organization_id is PRESENT (fixture 23 observed it); pod_id, inbox_id, \
             used_at and permissions are the absent optionals this test is about"
        );
        assert!(!s.contains("null"), "absent optionals are omitted, not null: {s}");
        assert_eq!(serde_json::from_str::<ApiKey>(&s).unwrap(), key);
    }

    #[test]
    fn the_secret_exists_only_on_the_create_response() {
        // The reference API returns the key material exactly once. Modelling it as a separate type
        // rather than an Option on ApiKey makes "read a key back" unrepresentable instead of
        // merely discouraged — so this asserts the field is absent from ApiKey's wire form, which
        // is what a leak would look like.
        let key = ApiKey {
            organization_id: Some(OrganizationId::new("org-1")),
            api_key_id: ApiKeyId::new("ak_1"),
            prefix: "am_us_abcd".into(),
            name: "probe".into(),
            pod_id: None,
            inbox_id: None,
            used_at: Some(Timestamp::now()),
            permissions: None,
            created_at: Timestamp::now(),
        };
        let fields = key_fields(&serde_json::to_string(&key).unwrap());
        assert!(!fields.iter().any(|f| f == "api_key"), "ApiKey must never carry the secret");

        let created = CreateApiKeyResponse {
            organization_id: Some(OrganizationId::new("org-1")),
            api_key_id: ApiKeyId::new("ak_1"),
            api_key: "am_us_secret".into(),
            prefix: "am_us_abcd".into(),
            name: "probe".into(),
            pod_id: None,
            inbox_id: None,
            permissions: None,
            created_at: Timestamp::now(),
        };
        let created_fields = key_fields(&serde_json::to_string(&created).unwrap());
        assert!(created_fields.iter().any(|f| f == "api_key"));
        assert!(
            !created_fields.iter().any(|f| f == "used_at"),
            "a key returned at creation has never been used; openapi omits the field"
        );
    }

    #[test]
    fn a_pod_bound_key_names_its_pod_and_an_inbox_bound_key_its_inbox() {
        let key = ApiKey {
            organization_id: Some(OrganizationId::new("org-1")),
            api_key_id: ApiKeyId::new("ak_2"),
            prefix: "am_us_efgh".into(),
            name: "pod key".into(),
            pod_id: Some(PodId::new_random()),
            inbox_id: Some(InboxId::new("amk-probe@agentmail.to")),
            used_at: None,
            permissions: Some(ApiKeyPermissions { message_read: Some(true), ..Default::default() }),
            created_at: Timestamp::now(),
        };
        let s = serde_json::to_string(&key).unwrap();
        assert_eq!(
            key_fields(&s),
            [
                "api_key_id",
                "created_at",
                "inbox_id",
                "name",
                "organization_id",
                "permissions",
                "pod_id",
                "prefix"
            ]
        );
        // A restricted permissions object grants only what it names — the empty-vs-absent
        // distinction that KeyGrants exists to preserve.
        let round: ApiKey = serde_json::from_str(&s).unwrap();
        assert_eq!(round, key);
        assert!(KeyGrants::from_wire(round.permissions).allows("message_read"));
    }

    #[test]
    fn a_key_list_omits_the_token_on_the_last_page() {
        let empty = ListApiKeysResponse::new(vec![], None, None);
        let s = serde_json::to_string(&empty).unwrap();
        assert_eq!(s, r#"{"count":0,"api_keys":[]}"#);
    }
}
