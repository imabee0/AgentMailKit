//! The auth layer: resolves `Authorization: Bearer <key>` into a [`Credential`], then into an
//! [`AuthContext`], before any handler runs.
//!
//! `[SPEC:fixture 05-error-catalog.http]`: a **missing** `Authorization` header is `401
//! {"message":"Unauthorized"}`; a **present** header this layer cannot turn into a usable
//! credential — malformed, unknown, or a well-formed-but-invalid `am_` key — is `403
//! {"message":"Forbidden"}`. Both are the bare [`GatewayFailure`] body, never the full envelope.

use amk_core::scope::Scope;
use amk_store::api_keys::{self, AuthenticatedKey};
use amk_types::api_key::KeyGrants;
use amk_types::{Identity, ScopeType};
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, GatewayFailure};
use crate::AppState;

/// What the auth layer resolved a presented secret to. One variant today (`ApiKey`) — the
/// dispatch contract's own words: *"This type is yours and lives in this crate. It is not in
/// amk-types and must not be added there: it is an internal resolution step, never serialised,
/// never on the wire."* Handlers never see this — only [`AuthContext`], built from it and then
/// dropped.
#[derive(Debug, Clone)]
pub enum Credential {
    ApiKey(AuthenticatedKey),
}

/// The resolved principal a handler is allowed to see: identity, grants, and the
/// `amk_core::scope::Scope` derived from them — computed once, here, so a handler that re-derives
/// scope is a defect by construction (it has no raw identity to re-derive from; `identity` is
/// kept only because `GET /v0/auth/me` echoes it verbatim).
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub identity: Identity,
    pub grants: KeyGrants,
    pub scope: Scope,
}

/// The auth layer can fail two structurally different ways: the credential is unusable (the bare
/// [`GatewayFailure`] body), or the store blew up resolving it (a genuine internal error, which
/// gets the full envelope — a database outage is not "your credential is wrong").
#[derive(Debug)]
pub enum AuthRejection {
    Gateway(GatewayFailure),
    App(AppError),
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match self {
            AuthRejection::Gateway(g) => g.into_response(),
            AuthRejection::App(a) => a.into_response(),
        }
    }
}

impl From<GatewayFailure> for AuthRejection {
    fn from(g: GatewayFailure) -> Self {
        Self::Gateway(g)
    }
}

impl From<AppError> for AuthRejection {
    fn from(e: AppError) -> Self {
        Self::App(e)
    }
}

fn scope_type_of(key: &AuthenticatedKey) -> ScopeType {
    if key.inbox_id.is_some() {
        ScopeType::Inbox
    } else if key.pod_id.is_some() {
        ScopeType::Pod
    } else {
        ScopeType::Organization
    }
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .ok_or_else(GatewayFailure::unauthorized)?;
        let value = header.to_str().map_err(|_| GatewayFailure::forbidden())?;
        let presented = value
            .strip_prefix("Bearer ")
            .ok_or_else(GatewayFailure::forbidden)?;

        // O(1) lookup by the key's `prefix`, then one constant-time argon2id verify — both live
        // inside `amk_store::api_keys::authenticate`; this layer never scans the key table itself.
        let resolved = api_keys::authenticate(&state.pool, presented)
            .await
            .map_err(AppError::from)?
            .ok_or_else(GatewayFailure::forbidden)?;
        let credential = Credential::ApiKey(resolved);
        let Credential::ApiKey(key) = credential;

        // Best-effort usage tracking, independent of authentication per `authenticate`'s own doc.
        // Never blocks or fails the request on error.
        let _ = api_keys::touch_used_at(&state.pool, &key.api_key_id).await;

        let scope_type = scope_type_of(&key);

        // An inbox-scoped `api_keys` row's own `pod_id` column is NULL by design
        // (`migrations/0007_api_keys.sql`: "an inbox-scoped credential's pod is looked up through
        // its inbox at the Identity-building layer, not stored redundantly here") — this is that
        // layer. `Identity`/`Scope::from_identity` require `pod_id` whenever `scope_type` is
        // `inbox` (`openapi.json type_auth:Identity`), so it must be resolved via the inbox.
        let pod_id = match (key.pod_id, &key.inbox_id) {
            (Some(p), _) => Some(p),
            (None, Some(inbox_id)) => {
                let inbox =
                    amk_store::inboxes::get(&state.pool, &key.organization_id, None, inbox_id)
                        .await
                        .map_err(AppError::from)?;
                match inbox {
                    Some(inbox) => Some(inbox.pod_id),
                    // The key's own inbox is gone. `inboxes::delete` cascades to inbox-scoped
                    // keys (migration 0008), so a *resolved* key naming a since-deleted inbox is a
                    // narrow concurrent-delete race, not a reachable steady state. Treated as an
                    // auth-layer failure (nothing has been looked up on the request's behalf yet),
                    // the same reading `amk_core::scope::ScopeResolutionError` gives a
                    // self-contradictory resolved identity.
                    //
                    // Accepted, unverified: no test drives this arm (it needs an inbox deleted in
                    // the gap between `authenticate` and this lookup, not reproducible without an
                    // injectable delay). It sits directly above `Scope::from_identity`'s own error
                    // mapping below, which is likewise unverified — two unverified guards stacked,
                    // so a regression widening this one (e.g. accidentally returning `pod_id: None`
                    // instead of erroring) would be caught only by the second, which has no
                    // dedicated test either. The depth here is nominal, not proven.
                    None => return Err(GatewayFailure::forbidden().into()),
                }
            }
            (None, None) => None,
        };

        let scope_id = match scope_type {
            ScopeType::Inbox => key
                .inbox_id
                .as_ref()
                .expect("invariant: scope_type_of returns Inbox only when inbox_id is Some")
                .as_str()
                .to_owned(),
            ScopeType::Pod => pod_id
                .expect("invariant: an inbox or pod scope always resolves a pod_id above")
                .to_string(),
            ScopeType::Organization => key.organization_id.as_str().to_owned(),
        };

        let identity = Identity {
            api_key_id: Some(key.api_key_id.clone()),
            organization_id: key.organization_id.clone(),
            scope_id,
            scope_type,
            pod_id,
            inbox_id: key.inbox_id.clone(),
        };

        let grants = KeyGrants::from_wire(key.permissions.clone());
        // Scope resolution runs here, before any handler — a handler that re-derives it is a
        // defect (see the module doc). A self-contradictory resolved identity is an auth-layer
        // failure, never a scope denial: nothing has been looked up on the request's behalf yet.
        let scope = Scope::from_identity(&identity).map_err(|_| GatewayFailure::forbidden())?;

        Ok(AuthContext { identity, grants, scope })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_type_of_prefers_inbox_then_pod_then_organization() {
        use amk_types::ids::{ApiKeyId, InboxId, OrganizationId, PodId};
        let base = AuthenticatedKey {
            api_key_id: ApiKeyId::new("k"),
            organization_id: OrganizationId::new("o"),
            pod_id: None,
            inbox_id: None,
            permissions: None,
        };
        assert_eq!(scope_type_of(&base), ScopeType::Organization);
        let pod = AuthenticatedKey { pod_id: Some(PodId::new_random()), ..base.clone() };
        assert_eq!(scope_type_of(&pod), ScopeType::Pod);
        let inbox = AuthenticatedKey { inbox_id: Some(InboxId::new("a@b.c")), ..base.clone() };
        assert_eq!(scope_type_of(&inbox), ScopeType::Inbox);
        // Both set (never a real row, per the migration's CHECK) still prefers inbox — this is
        // the "narrower id wins" reading, matching `Scope::from_identity`'s own rejection of the
        // opposite (a wider scope_type carrying a narrower id).
        let both = AuthenticatedKey {
            pod_id: Some(PodId::new_random()),
            inbox_id: Some(InboxId::new("a@b.c")),
            ..base
        };
        assert_eq!(scope_type_of(&both), ScopeType::Inbox);
    }
}
