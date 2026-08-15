//! What a credential is allowed to *do* — the flag half of authorization.
//!
//! # The catalog is not ours
//!
//! The 36 permission flags, their names, and the pairing between a restricted label and the
//! `label_*_read` flag that unlocks it all live in [`amk_types::api_key`], generated from
//! `reference/openapi.json` (`type_api-keys:ApiKeyPermissions`). This module holds **policy** —
//! which flag an action needs, how a child key is bounded by its parent, which error a refusal
//! surfaces as — and never a second copy of the catalog. An earlier revision of amk-core kept its
//! own flag enum *and* its own label→flag map; a sibling module kept a third; the two disagreed
//! about what a restricted label actually requires. One catalog, upstream, is the fix.
//!
//! # The rule
//!
//! * `permissions` **absent** → [`KeyGrants::Unrestricted`]: every flag, within the key's scope
//!   (*"When ommitted all permissions are granted"*, openapi schema description).
//! * `permissions` **present** → only flags explicitly `true` are granted. Absent, `false`, and
//!   unknown names are all denials. [`KeyGrants::allows`] already fails closed on each.
//!
//! Scope is checked **first and separately** — see [`authorize`]. Scope is not a set of flags: it
//! is an organization/pod/inbox containment question that `crate::scope` answers as a boolean, so
//! there is no set intersection anywhere in this module.
//!
//! # This module cannot decide visibility
//!
//! A `label_*_read` flag is only **half** of the restricted-label rule: a list result also
//! requires the caller to have set the matching `include_*` query flag (fixture `09b`: a
//! credential that *held* `label_unauthenticated_read` still got `count=0` from every list
//! endpoint). The composed verdict is owned by `crate::labels`. What this module exposes is
//! [`allows_label_read`], named so it cannot be mistaken for the whole check, and it deliberately
//! offers no `is_visible` / `retain_visible` / `visible_count`.
//!
//! # Failure direction
//!
//! Every choice here fails towards denial: an unknown flag name grants nothing, a permission a
//! subscription implies is required rather than assumed, and a refusal that would confirm a
//! resource exists is remapped to `not_found` ([`Denial::Hidden`]).

use amk_types::api_key::{self, KeyGrants, WIRE_NAMES};
use amk_types::event::EventType;
use amk_types::message::labels;
use amk_types::ErrorCode;

/// The flag names this module's own policy refers to. Every other flag is named by the handler
/// that needs it, as a literal; [`is_known_flag`] lets that handler's test prove the literal is
/// real, because a typo would silently deny rather than loudly fail.
///
/// The label-read flags are deliberately **not** here — they come from
/// [`amk_types::api_key::label_read_flag`], which is the one place the label↔flag pairing exists.
pub mod flags {
    pub const MESSAGE_READ: &str = "message_read";
    pub const DOMAIN_READ: &str = "domain_read";
    pub const API_KEY_CREATE: &str = "api_key_create";
}

/// Whether `name` is a flag the catalog defines. Intended for route-table tests, not for
/// request handling — handling goes through [`require`], which denies unknown names anyway.
pub fn is_known_flag(name: &str) -> bool {
    WIRE_NAMES.contains(&name)
}

// ---------------------------------------------------------------------------------------------
// Action authorization
// ---------------------------------------------------------------------------------------------

/// `missing_permission` unless the credential holds `flag`.
///
/// Scope containment is a separate, earlier check — call [`authorize`] rather than this when the
/// action targets a resource.
pub fn require(grants: &KeyGrants, flag: &'static str) -> Result<(), Denial> {
    if grants.allows(flag) {
        Ok(())
    } else {
        Err(Denial::MissingPermission(flag))
    }
}

/// Gate for the paths that admit only a credential with no `permissions` object at all.
pub fn require_unrestricted(grants: &KeyGrants) -> Result<(), Denial> {
    match grants {
        KeyGrants::Unrestricted => Ok(()),
        KeyGrants::Restricted(_) => Err(Denial::UnrestrictedKeyRequired),
    }
}

/// Full check for one action on one resource: **scope first, then the flag**.
///
/// `resource_in_scope` is the verdict of scope resolution (`crate::scope`). It is evaluated first
/// so that an out-of-scope resource always answers `not_found`: were the flag checked first, the
/// 403/404 split would tell a caller whether a resource it cannot reach exists.
pub fn authorize(
    grants: &KeyGrants,
    required: &'static str,
    resource_in_scope: bool,
) -> Result<(), Denial> {
    if !resource_in_scope {
        return Err(Denial::Hidden);
    }
    require(grants, required)
}

// ---------------------------------------------------------------------------------------------
// Restricted labels — the PARTIAL gate
// ---------------------------------------------------------------------------------------------

/// Does the credential hold the `label_*_read` flag that gates `label`?
///
/// **This is one half of a two-part rule and is not a visibility decision.** A list result also
/// requires the matching `include_*` query flag; the composed verdict belongs to
/// `crate::labels::admit`. Calling this on a list path and treating `true` as "show the row" is
/// the exact defect this signature is shaped to prevent — fixture `09b` recorded a credential
/// that held `label_unauthenticated_read` and still saw `count=0` from every list endpoint.
///
/// A label that is not restricted is not gated, so the answer is `true`: there is no flag to hold.
pub fn allows_label_read(grants: &KeyGrants, label: &str) -> bool {
    match api_key::label_read_flag(label) {
        Some(flag) => grants.allows(flag),
        None => true,
    }
}

// ---------------------------------------------------------------------------------------------
// Child keys
// ---------------------------------------------------------------------------------------------

/// Validate the permissions requested for a child key against this (the parent's) authority.
///
/// A child may never exceed its parent, at any depth: because every key is minted through this
/// check, the bound holds transitively — a grandchild cannot recover a flag its parent dropped,
/// even when the grandparent held it.
///
/// **[INFERRED]** — a *restricted* parent may not mint an **unrestricted** child. No fixture
/// exercises key derivation; the reasoning is that an unrestricted child's authority is unbounded
/// and grows with the catalog, so a parent holding every flag defined today would be handing out
/// flags that do not exist yet. It refuses with [`Denial::UnboundedChild`] rather than
/// [`Denial::PermissionEscalation`], because the set of flags the parent "lacks" in that case is
/// empty and an escalation message listing nothing explains nothing.
///
/// The caller still needs [`flags::API_KEY_CREATE`] and the child's scope must sit inside the
/// parent's — both are the handler's checks, not this one.
pub fn derive_child(parent: &KeyGrants, requested: &KeyGrants) -> Result<KeyGrants, Denial> {
    match (parent, requested) {
        (KeyGrants::Unrestricted, child) => Ok(child.clone()),
        (KeyGrants::Restricted(_), KeyGrants::Unrestricted) => Err(Denial::UnboundedChild),
        (KeyGrants::Restricted(_), child) => {
            let missing: Vec<&'static str> = WIRE_NAMES
                .into_iter()
                .filter(|name| child.allows(name) && !parent.allows(name))
                .collect();
            if missing.is_empty() {
                Ok(child.clone())
            } else {
                Err(Denial::PermissionEscalation { missing })
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Event subscription
// ---------------------------------------------------------------------------------------------

/// The restricted label carried by a `message.received.*` variant, if it is one.
///
/// Kept as its own function so the tripwire test can assert it is total over
/// [`EventType::is_restricted_receive`].
fn restricted_receive_label(event: EventType) -> Option<&'static str> {
    match event {
        EventType::MessageReceivedSpam => Some(labels::SPAM),
        EventType::MessageReceivedBlocked => Some(labels::BLOCKED),
        EventType::MessageReceivedUnauthenticated => Some(labels::UNAUTHENTICATED),
        _ => None,
    }
}

/// Every flag a credential must hold to subscribe to `event`.
///
/// **[INFERRED]** — no fixture records a webhook-creation refusal. The rule is derived from what
/// the payload contains: fixture `09b` captured a `message.received.unauthenticated` delivery
/// carrying `text`, `preview`, `extracted_text`, `headers`, `from`, `to`, `subject` and the whole
/// thread. A subscription is therefore a standing read of message content, and it requires
/// [`flags::MESSAGE_READ`] for the same reason `GET /messages/{id}` does — otherwise a key holding
/// only `webhook_create` reads by webhook everything the API would refuse it. For the restricted
/// variants the matching `label_*_read` flag is required **in addition**, not instead.
///
/// `domain.verified` is gated on [`flags::DOMAIN_READ`] on the same argument; that extension is
/// [INFERRED] too, and is the one part of this rule that the payload evidence does not reach.
pub fn subscription_flags(event: EventType) -> Vec<&'static str> {
    if event == EventType::DomainVerified {
        return vec![flags::DOMAIN_READ];
    }
    let mut required = vec![flags::MESSAGE_READ];
    if let Some(label) = restricted_receive_label(event) {
        let flag = api_key::label_read_flag(label)
            .expect("invariant: every restricted label has a read flag (amk-types enforces it)");
        required.push(flag);
    }
    required
}

/// `missing_permission` naming the first flag the credential lacks for this subscription.
pub fn require_subscribe(grants: &KeyGrants, event: EventType) -> Result<(), Denial> {
    for flag in subscription_flags(event) {
        require(grants, flag)?;
    }
    Ok(())
}

/// [`require_subscribe`] as a predicate.
pub fn may_subscribe(grants: &KeyGrants, event: EventType) -> bool {
    require_subscribe(grants, event).is_ok()
}

// ---------------------------------------------------------------------------------------------
// Denials
// ---------------------------------------------------------------------------------------------

/// Why an operation was refused, and which error code it must surface as.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denial {
    /// The credential does not hold the required flag.
    #[error("missing permission `{0}`")]
    MissingPermission(&'static str),
    /// A child key was asked for flags the parent does not hold.
    #[error("permission escalation: parent lacks {missing:?}")]
    PermissionEscalation { missing: Vec<&'static str> },
    /// A restricted parent tried to mint a child with no `permissions` object.
    #[error("permission escalation: a restricted key may not mint an unrestricted key")]
    UnboundedChild,
    /// The path admits only a credential with no permission restrictions.
    #[error("this operation requires an unrestricted API key")]
    UnrestrictedKeyRequired,
    /// Out of scope. **Surfaces as `not_found`** — a 403 would confirm the resource exists
    /// (`reference/fixtures/05-error-catalog.http`: the `not_found` `fix` string documents
    /// exactly this masking).
    #[error("resource is not visible to this credential")]
    Hidden,
}

impl Denial {
    pub fn code(&self) -> ErrorCode {
        match self {
            Denial::MissingPermission(_) => ErrorCode::MissingPermission,
            Denial::PermissionEscalation { .. } | Denial::UnboundedChild => {
                ErrorCode::PermissionEscalation
            }
            Denial::UnrestrictedKeyRequired => ErrorCode::UnrestrictedKeyRequired,
            Denial::Hidden => ErrorCode::NotFound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amk_types::api_key::ApiKeyPermissions;

    /// A restricted credential granting exactly the named flags.
    fn whitelist(granted: &[&str]) -> KeyGrants {
        let mut p = ApiKeyPermissions::default();
        for name in granted {
            match *name {
                "inbox_read" => p.inbox_read = Some(true),
                "inbox_create" => p.inbox_create = Some(true),
                "message_read" => p.message_read = Some(true),
                "message_send" => p.message_send = Some(true),
                "label_spam_read" => p.label_spam_read = Some(true),
                "label_blocked_read" => p.label_blocked_read = Some(true),
                "label_unauthenticated_read" => p.label_unauthenticated_read = Some(true),
                "label_trash_read" => p.label_trash_read = Some(true),
                "webhook_create" => p.webhook_create = Some(true),
                "domain_read" => p.domain_read = Some(true),
                "api_key_create" => p.api_key_create = Some(true),
                "pod_create" => p.pod_create = Some(true),
                other => panic!("test helper does not know flag {other}"),
            }
        }
        KeyGrants::Restricted(p)
    }

    /// Every flag the catalog defines, as a restricted credential — distinct from `Unrestricted`,
    /// which also acquires flags added later.
    fn full_whitelist() -> KeyGrants {
        let json: serde_json::Value = WIRE_NAMES
            .into_iter()
            .map(|n| (n.to_owned(), serde_json::Value::Bool(true)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        KeyGrants::Restricted(serde_json::from_value(json).unwrap())
    }

    // ------------------------------------------------------------------ the catalog is upstream

    #[test]
    fn this_module_names_no_flag_the_catalog_does_not_define() {
        // The failure this catches is a policy referring to a flag that does not exist: it would
        // deny silently forever instead of failing loudly.
        for name in [
            flags::MESSAGE_READ,
            flags::DOMAIN_READ,
            flags::API_KEY_CREATE,
        ] {
            assert!(is_known_flag(name), "{name} is not in ApiKeyPermissions");
        }
        assert!(!is_known_flag("inbox_teleport"));
        assert!(!is_known_flag("Message_Read"));
    }

    #[test]
    fn a_flag_named_twice_in_one_object_is_rejected_rather_than_resolved() {
        // Regression: amk-core used to fold the wire object into a bitset itself, and its loop
        // treated an explicit `false` as a no-op instead of a removal — so `inbox_read` true then
        // false left the flag GRANTED, the opposite of last-write-wins. Deleting that loop in
        // favour of the amk-types type removes the ambiguity entirely: serde refuses a duplicate
        // field, so the request fails and nothing is granted.
        let err =
            serde_json::from_str::<ApiKeyPermissions>(r#"{"inbox_read":true,"inbox_read":false}"#)
                .unwrap_err();
        assert!(err.to_string().contains("duplicate field"), "{err}");

        // And an explicit `false` on its own is a denial, not a no-op.
        let single: ApiKeyPermissions = serde_json::from_str(r#"{"inbox_read":false}"#).unwrap();
        assert!(!KeyGrants::from_wire(Some(single)).allows("inbox_read"));
    }

    #[test]
    fn an_unknown_flag_name_is_never_granted_in_either_direction() {
        assert!(!KeyGrants::Unrestricted.allows("label_everything_read"));
        assert!(!whitelist(&["inbox_read"]).allows("inbox_write"));
        assert_eq!(
            require(&KeyGrants::Unrestricted, "inbox_write").unwrap_err(),
            Denial::MissingPermission("inbox_write"),
            "even an unrestricted key is denied a flag that does not exist"
        );
    }

    // ------------------------------------------------------------------------- action gating

    #[test]
    fn an_absent_permissions_object_grants_every_action_and_an_empty_one_none() {
        let unrestricted = KeyGrants::from_wire(None);
        let empty = KeyGrants::from_wire(Some(ApiKeyPermissions::default()));
        for name in WIRE_NAMES {
            assert!(unrestricted.allows(name), "{name} denied by an unrestricted key");
            assert!(!empty.allows(name), "{name} granted by an empty whitelist");
        }
        assert!(require(&unrestricted, flags::MESSAGE_READ).is_ok());
        assert_eq!(
            require(&empty, flags::MESSAGE_READ).unwrap_err(),
            Denial::MissingPermission(flags::MESSAGE_READ)
        );
        assert_eq!(
            require(&empty, flags::MESSAGE_READ).unwrap_err().code(),
            ErrorCode::MissingPermission
        );
    }

    #[test]
    fn an_out_of_scope_resource_is_not_found_even_when_the_flag_is_held() {
        // Scope is decided BEFORE the flag, so a 403 can never confirm that an out-of-scope
        // resource exists.
        let key = KeyGrants::from_wire(None);
        let err = authorize(&key, flags::MESSAGE_READ, false).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotFound);
        assert_eq!(err.code().status(), 404);

        let blind = whitelist(&[]);
        assert_eq!(
            authorize(&blind, flags::MESSAGE_READ, false)
                .unwrap_err()
                .code(),
            ErrorCode::NotFound
        );
        assert_eq!(
            authorize(&blind, flags::MESSAGE_READ, true)
                .unwrap_err()
                .code(),
            ErrorCode::MissingPermission
        );
        assert!(authorize(&key, flags::MESSAGE_READ, true).is_ok());
    }

    #[test]
    fn unrestricted_key_required_admits_only_a_key_with_no_permissions_object() {
        assert!(require_unrestricted(&KeyGrants::from_wire(None)).is_ok());
        for restricted in [whitelist(&[]), whitelist(&["inbox_read"]), full_whitelist()] {
            let err = require_unrestricted(&restricted).unwrap_err();
            assert_eq!(err, Denial::UnrestrictedKeyRequired);
            assert_eq!(err.code().status(), 403);
        }
    }

    // --------------------------------------------------------- the label gate is only a half

    #[test]
    fn allows_label_read_answers_only_the_permission_half() {
        let spam_only = whitelist(&["label_spam_read"]);
        assert!(allows_label_read(&spam_only, labels::SPAM));
        assert!(!allows_label_read(&spam_only, labels::TRASH));
        assert!(!allows_label_read(&whitelist(&[]), labels::UNAUTHENTICATED));
        assert!(allows_label_read(&KeyGrants::from_wire(None), labels::TRASH));
        // Nothing gates an unrestricted label, so there is no flag to lack.
        for label in [
            labels::RECEIVED,
            labels::SENT,
            labels::UNREAD,
            labels::BOUNCED,
            "project-x",
        ] {
            assert!(allows_label_read(&whitelist(&[]), label), "{label} must not be gated");
        }
    }

    #[test]
    fn this_module_exposes_no_visibility_verdict() {
        // A compile-level tripwire would be better, but the real guard is that these names do not
        // exist here at all; this test documents the reason so a future edit does not re-add them.
        // The composed rule (permission AND include_* flag) lives in crate::labels, and fixture
        // 09b is why: the credential HELD label_unauthenticated_read and the list endpoints still
        // returned count=0. A permission-only `retain_visible` here would have returned the row.
        let permitted = whitelist(&["message_read", "label_unauthenticated_read"]);
        assert!(allows_label_read(&permitted, labels::UNAUTHENTICATED));
        let access =
            crate::labels::LabelAccess::list(&permitted, crate::labels::IncludeFlags::NONE);
        assert!(
            !crate::labels::admits(&[labels::UNAUTHENTICATED], &access),
            "the permission alone must not admit a row to a list"
        );
    }

    #[test]
    fn every_restricted_label_is_gated_by_a_flag_this_module_can_reach() {
        // Tripwire: a fifth restricted label added upstream without a flag would open ungated.
        for label in labels::RESTRICTED {
            let flag = api_key::label_read_flag(label).unwrap_or_else(|| panic!("{label} ungated"));
            assert!(is_known_flag(flag));
            assert!(!allows_label_read(&whitelist(&[]), label));
        }
    }

    // ------------------------------------------------------------------------- child keys

    #[test]
    fn a_child_may_not_request_a_permission_the_parent_lacks() {
        let parent = whitelist(&["inbox_read", "message_read"]);
        let err = derive_child(&parent, &whitelist(&["inbox_read", "message_send"])).unwrap_err();
        assert_eq!(err, Denial::PermissionEscalation { missing: vec!["message_send"] });
        assert_eq!(err.code(), ErrorCode::PermissionEscalation);
        assert_eq!(err.code().status(), 403);
    }

    #[test]
    fn boundary_child_equal_to_parent_is_allowed_and_one_flag_over_is_not() {
        let parent = whitelist(&["inbox_read", "message_read"]);
        assert_eq!(derive_child(&parent, &parent).unwrap(), parent);
        assert!(derive_child(&parent, &whitelist(&["inbox_read"])).is_ok());
        assert!(
            derive_child(&parent, &whitelist(&["inbox_read", "message_read", "inbox_create"]))
                .is_err()
        );
    }

    #[test]
    fn a_restricted_parent_minting_an_unrestricted_child_is_refused_with_a_message_that_says_why() {
        // Regression: computing `missing = every_flag - parent` yields the EMPTY set when the
        // parent holds every flag defined today, so the refusal rendered as "parent lacks {}".
        for parent in [whitelist(&["inbox_read"]), full_whitelist()] {
            let err = derive_child(&parent, &KeyGrants::Unrestricted).unwrap_err();
            assert_eq!(err, Denial::UnboundedChild);
            assert_eq!(err.code(), ErrorCode::PermissionEscalation);
            let rendered = err.to_string();
            assert!(
                rendered.contains("restricted key may not mint an unrestricted key"),
                "{rendered}"
            );
            assert!(!rendered.contains("[]"), "an empty flag list explains nothing: {rendered}");
        }
    }

    #[test]
    fn an_unrestricted_parent_may_mint_anything() {
        let parent = KeyGrants::Unrestricted;
        assert_eq!(
            derive_child(&parent, &KeyGrants::Unrestricted).unwrap(),
            KeyGrants::Unrestricted
        );
        let child = whitelist(&["pod_create"]);
        assert_eq!(derive_child(&parent, &child).unwrap(), child);
    }

    #[test]
    fn escalation_is_enforced_at_every_level_of_the_chain() {
        let root = KeyGrants::Unrestricted;
        let parent = derive_child(&root, &whitelist(&["message_read", "inbox_read"])).unwrap();
        assert!(derive_child(&parent, &whitelist(&["message_send"])).is_err());
        let grandchild = derive_child(&parent, &whitelist(&["message_read"])).unwrap();
        assert!(derive_child(&grandchild, &whitelist(&["inbox_read"])).is_err());
    }

    // ------------------------------------------------------------- event subscription

    /// Every variant of `EventType`, so a new one cannot slip past the gating tests.
    const ALL_EVENTS: [EventType; 10] = [
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

    #[test]
    fn the_deny_everything_credential_may_subscribe_to_nothing() {
        // Inverted from an earlier test that asserted the opposite. A key holding only
        // webhook_create used to be allowed to subscribe to message.received and every other
        // unrestricted variant — and fixture 09b shows that payload carries text, preview,
        // extracted_text, headers, from, to, subject and the whole thread, i.e. everything
        // GET /messages/{id} would have refused it.
        let webhook_only = whitelist(&["webhook_create"]);
        for event in ALL_EVENTS {
            assert!(
                !may_subscribe(&webhook_only, event),
                "{event:?} was open to a key with no read flag"
            );
        }
        for event in ALL_EVENTS {
            assert!(
                !may_subscribe(&whitelist(&[]), event),
                "{event:?} open to the empty whitelist"
            );
            assert!(
                may_subscribe(&KeyGrants::Unrestricted, event),
                "{event:?} closed to an unrestricted key"
            );
        }
    }

    #[test]
    fn every_message_event_requires_message_read() {
        let reader = whitelist(&["message_read"]);
        for event in ALL_EVENTS {
            if event == EventType::DomainVerified {
                continue;
            }
            assert!(subscription_flags(event).contains(&flags::MESSAGE_READ), "{event:?}");
            if !event.is_restricted_receive() {
                assert!(may_subscribe(&reader, event), "{event:?} needs only message_read");
            }
        }
        assert_eq!(
            require_subscribe(&whitelist(&["label_spam_read"]), EventType::MessageReceivedSpam)
                .unwrap_err(),
            Denial::MissingPermission(flags::MESSAGE_READ),
            "the label flag does not substitute for message_read"
        );
    }

    #[test]
    fn restricted_receive_events_need_the_label_flag_in_addition() {
        let cases = [
            (EventType::MessageReceivedSpam, "label_spam_read"),
            (EventType::MessageReceivedBlocked, "label_blocked_read"),
            (EventType::MessageReceivedUnauthenticated, "label_unauthenticated_read"),
        ];
        for (event, flag) in cases {
            let reader = whitelist(&["message_read"]);
            assert_eq!(
                require_subscribe(&reader, event).unwrap_err(),
                Denial::MissingPermission(flag),
                "{event:?} was open without its label flag"
            );
            assert!(may_subscribe(&whitelist(&["message_read", flag]), event));
        }
        // The wrong label flag does not open a restricted event.
        assert!(!may_subscribe(
            &whitelist(&["message_read", "label_spam_read"]),
            EventType::MessageReceivedUnauthenticated
        ));
    }

    #[test]
    fn domain_verified_is_gated_on_domain_read() {
        // [INFERRED], and the weakest link in the subscription rule: no payload capture exists for
        // domain.verified, so the requirement is reasoned from the resource it describes.
        assert_eq!(subscription_flags(EventType::DomainVerified), vec![flags::DOMAIN_READ]);
        assert!(may_subscribe(&whitelist(&["domain_read"]), EventType::DomainVerified));
        assert!(!may_subscribe(&whitelist(&["message_read"]), EventType::DomainVerified));
    }

    #[test]
    fn the_restricted_receive_label_map_is_total() {
        // Tripwire: a new restricted receive variant upstream must not fall through to
        // "message_read is enough".
        for event in ALL_EVENTS {
            assert_eq!(
                restricted_receive_label(event).is_some(),
                event.is_restricted_receive(),
                "{event:?}"
            );
            if let Some(label) = restricted_receive_label(event) {
                assert!(labels::is_restricted(label));
            }
            for flag in subscription_flags(event) {
                assert!(is_known_flag(flag), "{event:?} requires unknown flag {flag}");
            }
        }
    }

    // ----------------------------------------------------------------- denial mapping

    #[test]
    fn denials_map_to_the_documented_codes() {
        assert_eq!(Denial::MissingPermission("inbox_read").code(), ErrorCode::MissingPermission);
        assert_eq!(
            Denial::PermissionEscalation { missing: vec!["inbox_read"] }.code(),
            ErrorCode::PermissionEscalation
        );
        assert_eq!(Denial::UnboundedChild.code(), ErrorCode::PermissionEscalation);
        assert_eq!(Denial::UnrestrictedKeyRequired.code(), ErrorCode::UnrestrictedKeyRequired);
        assert_eq!(Denial::Hidden.code(), ErrorCode::NotFound);
        for d in [
            Denial::MissingPermission("inbox_read"),
            Denial::PermissionEscalation { missing: vec!["inbox_read"] },
            Denial::UnboundedChild,
            Denial::UnrestrictedKeyRequired,
        ] {
            assert_eq!(d.code().status(), 403, "{d:?} is a 403");
        }
        assert_eq!(Denial::Hidden.code().status(), 404);
    }
}
