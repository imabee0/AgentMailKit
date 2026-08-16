//! `/v0/api-keys`, `/v0/pods/{pod_id}/api-keys`, `/v0/inboxes/{inbox_id}/api-keys` — the one
//! collection in this dispatch mounted all three ways. Written once, mounted three times.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use amk_core::permissions;
use amk_core::scope::{ResourceKind, ScopeFilter};
use amk_store::api_keys::{self, ListApiKeysQuery, NewApiKey};
use amk_store::pagination::ApiKeyCursor;
use amk_store::StoreError;
use amk_types::api_key::{
    CreateApiKeyRequest, CreateApiKeyResponse, KeyGrants, ListApiKeysResponse,
};
use amk_types::ids::ApiKeyId;

use crate::auth::AuthContext;
use crate::error::AppError;
use crate::ids::{decode_segment, PathPodId, PathPodIdString};
use crate::pagination::{ListQuery, ListQueryNoDirection, Resolved};
use crate::scope_ext::{key_scope_for, organization_window, settle_inbox_mount, settle_pod_mount};
use crate::AppState;

/// `[ASSUMED]` — no fixture covers `POST /v0/api-keys` with an omitted `name`.
const DEFAULT_KEY_NAME: &str = "API Key";

fn inbox_id_from_path(raw: &str) -> Result<amk_types::ids::InboxId, amk_core::scope::ScopeDenial> {
    match decode_segment(raw) {
        Ok(s) => Ok(amk_types::ids::InboxId::new(s)),
        Err(_) => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Inbox)),
    }
}

// ---- list ----------------------------------------------------------------------------------

/// `GET /v0/api-keys` — the one api-keys list that carries `ascending`.
pub async fn list_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListApiKeysResponse>, AppError> {
    permissions::require(&ctx.grants, "api_key_read")?;
    let filter = organization_window(&ctx.scope);
    list_keys(&state, &filter, q.resolve()).await
}

/// `GET /v0/pods/{pod_id}/api-keys` — no `ascending` (contract's derived-from-`openapi.json`
/// table).
pub async fn list_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodId(pod_id): PathPodId,
    Query(q): Query<ListQueryNoDirection>,
) -> Result<Json<ListApiKeysResponse>, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    permissions::require(&ctx.grants, "api_key_read")?;
    list_keys(&state, &filter, q.resolve()).await
}

/// `GET /v0/inboxes/{inbox_id}/api-keys` — likewise no `ascending`.
pub async fn list_inbox(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
    Query(q): Query<ListQueryNoDirection>,
) -> Result<Json<ListApiKeysResponse>, AppError> {
    let inbox_id = inbox_id_from_path(&raw_inbox_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "api_key_read")?;
    list_keys(&state, &filter, q.resolve()).await
}

async fn list_keys(
    state: &AppState,
    filter: &ScopeFilter,
    resolved: Resolved,
) -> Result<Json<ListApiKeysResponse>, AppError> {
    let key_scope = key_scope_for(filter);
    let cursor = match &resolved.page_token {
        Some(t) => Some(
            ApiKeyCursor::decode(t, &key_scope)
                .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?,
        ),
        None => None,
    };
    let page = api_keys::list(
        &state.pool,
        filter.organization_id(),
        &key_scope,
        ListApiKeysQuery { limit: resolved.limit, direction: resolved.direction, cursor },
    )
    .await?;
    Ok(Json(ListApiKeysResponse::new(page.items, resolved.echo_limit, page.next)))
}

// ---- create --------------------------------------------------------------------------------

pub async fn create_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, AppError> {
    permissions::require(&ctx.grants, "api_key_create")?;
    let filter = organization_window(&ctx.scope);
    create_key(&state, &ctx, &filter, req).await
}

pub async fn create_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodId(pod_id): PathPodId,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    permissions::require(&ctx.grants, "api_key_create")?;
    create_key(&state, &ctx, &filter, req).await
}

pub async fn create_inbox(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, AppError> {
    let inbox_id = inbox_id_from_path(&raw_inbox_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "api_key_create")?;
    create_key(&state, &ctx, &filter, req).await
}

/// The created key's own scope is exactly the mount's resolved window — `filter.pod_id()`/
/// `filter.inbox_id()` — reusing the same `amk_core::scope` intersection every list/get on this
/// mount already went through, rather than a second "does the child's scope sit inside the
/// parent's" rule. A pod- or inbox-scoped credential hitting `/v0/api-keys` (org mount) is
/// narrowed by `organization_window` exactly as a list there is (`filter.pod_id()`/
/// `inbox_id()` non-`None`), so the created key inherits that same narrowing rather than an
/// organization-wide one — the scope half of "child ⊄ parent" is enforced by construction.
async fn create_key(
    state: &AppState,
    ctx: &AuthContext,
    filter: &ScopeFilter,
    req: CreateApiKeyRequest,
) -> Result<Json<CreateApiKeyResponse>, AppError> {
    let requested = KeyGrants::from_wire(req.permissions.clone());
    // The permission half of "child ⊄ parent": a requested flag the parent itself lacks, or a
    // restricted parent minting an unrestricted child, is `permission_escalation` (403).
    permissions::derive_child(&ctx.grants, &requested)?;

    let name = req.name.unwrap_or_else(|| DEFAULT_KEY_NAME.to_owned());
    let created = api_keys::create(
        &state.pool,
        NewApiKey {
            organization_id: filter.organization_id().clone(),
            pod_id: filter.pod_id().copied(),
            inbox_id: filter.inbox_id().cloned(),
            name,
            permissions: req.permissions,
        },
    )
    .await?;
    Ok(Json(created))
}

// ---- delete ------------------------------------------------------------------------------

pub async fn delete_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_api_key_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let filter = organization_window(&ctx.scope);
    delete_key(&state, &ctx, &filter, &raw_api_key_id).await
}

pub async fn delete_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdString(pod_id, raw_api_key_id): PathPodIdString,
) -> Result<StatusCode, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    delete_key(&state, &ctx, &filter, &raw_api_key_id).await
}

pub async fn delete_inbox(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_api_key_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let inbox_id = inbox_id_from_path(&raw_inbox_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    delete_key(&state, &ctx, &filter, &raw_api_key_id).await
}

/// `amk_store::api_keys::delete` is itself pinned by `(organization_id, KeyScope)` *and* the
/// exact target id, so — unlike an inbox get/update/delete by id — no separate "does this id
/// belong to my own narrower scope" pre-check is needed here: a key outside the resolved
/// `KeyScope` simply is not deleted, `rows_affected() == 0`, masked identically to one that never
/// existed.
async fn delete_key(
    state: &AppState,
    ctx: &AuthContext,
    filter: &ScopeFilter,
    raw_api_key_id: &str,
) -> Result<StatusCode, AppError> {
    permissions::require(&ctx.grants, "api_key_delete")?;
    let api_key_id = match decode_segment(raw_api_key_id) {
        Ok(s) => ApiKeyId::new(s),
        Err(_) => return Err(filter.not_found(ResourceKind::ApiKey).into()),
    };
    let key_scope = key_scope_for(filter);
    let deleted =
        api_keys::delete(&state.pool, filter.organization_id(), &key_scope, &api_key_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(filter.not_found(ResourceKind::ApiKey).into())
    }
}
