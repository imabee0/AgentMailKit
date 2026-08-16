//! Small combinators over `amk_core::scope`, shared by every handler in this dispatch.
//!
//! # Two different shapes of "resolve the scope", and they must not be confused
//!
//! * **A sub-collection under a *named* pod/inbox mount** (list/create under
//!   `/v0/pods/{pod_id}/…` or `/v0/inboxes/{inbox_id}/…`) *must* prove the mount's own resource
//!   first — [`settle_pod_mount`]/[`settle_inbox_mount`], which discharge a
//!   [`amk_core::scope::MountProbe`] with a lookup whose only purpose is that proof. Skipping this
//!   is the exact bug `amk_core::scope`'s own doc warns about: a foreign or absent mount resource
//!   would otherwise answer `200 {"count":0}` instead of `404`.
//! * **The mount's own resource, fetched by id** (`GET /v0/pods/{pod_id}`,
//!   `GET /v0/pods/{pod_id}/inboxes/{inbox_id}`) does *not* need that separate proof — the lookup
//!   *is* the proof, so those handlers call `Resolved::window()` directly rather than reaching for
//!   this module. See `amk_core::scope::Resolved::window`'s own doc for why that is correct there
//!   and would be the bug everywhere else.

use amk_core::scope::{Mount, Resolved, Scope, ScopeFilter};
use amk_store::api_keys::KeyScope;
use amk_types::ids::{InboxId, PodId};
use sqlx::PgPool;

use crate::error::AppError;

/// Resolve (proving, if necessary) the window for a sub-collection under a *named* pod mount.
pub async fn settle_pod_mount(
    pool: &PgPool,
    scope: &Scope,
    pod_id: PodId,
) -> Result<ScopeFilter, AppError> {
    match scope.resolve(&Mount::Pod(pod_id)) {
        Ok(Resolved::Ready(filter)) => Ok(filter),
        Ok(Resolved::Probe(probe)) => {
            let row = amk_store::pods::get(pool, probe.window().organization_id(), pod_id).await?;
            probe.settle(row).map_err(AppError::from)
        }
        Err(denial) => Err(denial.into()),
    }
}

/// As [`settle_pod_mount`], for a sub-collection under a *named* inbox mount.
pub async fn settle_inbox_mount(
    pool: &PgPool,
    scope: &Scope,
    inbox_id: &InboxId,
) -> Result<ScopeFilter, AppError> {
    match scope.resolve(&Mount::Inbox(inbox_id.clone())) {
        Ok(Resolved::Ready(filter)) => Ok(filter),
        Ok(Resolved::Probe(probe)) => {
            // Pin the pod too when the window already knows it (a pod-scoped credential probing
            // an inbox address), for the same storage-layer-predicate reason every list query
            // pins what it can rather than fetching wider and filtering after — even though
            // `MountProbe::settle` would also catch a mismatch via `ScopeFilter::admits`.
            let pod_pin = probe.window().pod_id().copied();
            let row =
                amk_store::inboxes::get(pool, probe.window().organization_id(), pod_pin, inbox_id)
                    .await?;
            probe.settle(row).map_err(AppError::from)
        }
        Err(denial) => Err(denial.into()),
    }
}

/// The window for the organization mount. Never a probe and never denied for any credential — see
/// `amk_core::scope::Scope::resolve`'s own doc (the organization mount names nothing narrower than
/// the credential's own scope) — so this never touches the database.
pub fn organization_window(scope: &Scope) -> ScopeFilter {
    match scope.resolve(&Mount::Organization) {
        Ok(Resolved::Ready(filter)) => filter,
        Ok(Resolved::Probe(_)) => {
            unreachable!("invariant: the organization mount never yields a probe")
        }
        Err(_) => unreachable!("invariant: the organization mount is never denied"),
    }
}

/// The `amk_store::api_keys::KeyScope` a resolved [`ScopeFilter`] maps to: inbox first
/// (narrowest), then pod, else organization. Mirrors `KeyScope`'s own doc: an inbox-scoped
/// key's stored `pod_id` column is always NULL, so a filter pinning both an inbox and a pod must
/// still resolve to `KeyScope::Inbox`, never `KeyScope::Pod` — the pod pin exists only to narrow
/// the *proving* lookup in [`settle_inbox_mount`], not to select the api-keys scope column.
pub fn key_scope_for(filter: &ScopeFilter) -> KeyScope {
    if let Some(inbox) = filter.inbox_id() {
        KeyScope::Inbox(inbox.clone())
    } else if let Some(pod) = filter.pod_id() {
        KeyScope::Pod(*pod)
    } else {
        KeyScope::Organization
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amk_types::ids::OrganizationId;
    use amk_types::{Identity, ScopeType};

    fn org() -> OrganizationId {
        OrganizationId::new("133c9cbe-f996-4094-a8d5-0c6603e022ea")
    }

    fn org_scope() -> Scope {
        let identity = Identity {
            api_key_id: None,
            organization_id: org(),
            scope_id: org().as_str().to_owned(),
            scope_type: ScopeType::Organization,
            pod_id: None,
            inbox_id: None,
        };
        Scope::from_identity(&identity).unwrap()
    }

    fn pod_scope(pod: PodId) -> Scope {
        let identity = Identity {
            api_key_id: None,
            organization_id: org(),
            scope_id: pod.to_string(),
            scope_type: ScopeType::Pod,
            pod_id: Some(pod),
            inbox_id: None,
        };
        Scope::from_identity(&identity).unwrap()
    }

    fn inbox_scope(pod: PodId, inbox: &str) -> Scope {
        let identity = Identity {
            api_key_id: None,
            organization_id: org(),
            scope_id: inbox.to_owned(),
            scope_type: ScopeType::Inbox,
            pod_id: Some(pod),
            inbox_id: Some(InboxId::new(inbox)),
        };
        Scope::from_identity(&identity).unwrap()
    }

    #[test]
    fn organization_window_never_panics_for_any_scope_shape() {
        let pod = PodId::new_random();
        for scope in [org_scope(), pod_scope(pod), inbox_scope(pod, "a@b.c")] {
            let _ = organization_window(&scope);
        }
    }

    #[test]
    fn key_scope_for_prefers_inbox_over_pod() {
        let filter = organization_window(&inbox_scope(PodId::new_random(), "a@b.c"));
        assert!(matches!(key_scope_for(&filter), KeyScope::Inbox(_)));
    }

    #[test]
    fn key_scope_for_pod_only_window_is_pod() {
        let filter = organization_window(&pod_scope(PodId::new_random()));
        assert!(matches!(key_scope_for(&filter), KeyScope::Pod(_)));
    }

    #[test]
    fn key_scope_for_organization_window_is_organization() {
        let filter = organization_window(&org_scope());
        assert_eq!(key_scope_for(&filter), KeyScope::Organization);
    }
}
