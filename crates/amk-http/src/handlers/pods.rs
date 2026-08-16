//! `/v0/pods` — the only mount pods have (there is no `/v0/pods/{pod_id}/pods`, so this handler
//! set is written once and mounted once, unlike inboxes and api-keys).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use amk_core::permissions;
use amk_core::scope::ResourceKind;
use amk_store::pagination::{Page, PodCursor};
use amk_store::pods::{self, ListPodsQuery, NewPod};
use amk_store::StoreError;
use amk_types::ids::{OrganizationId, PodId};
use amk_types::pod::{CreatePodRequest, ListPodsResponse, Pod};

use crate::auth::AuthContext;
use crate::error::AppError;
use crate::pagination::{ListQuery, Resolved};
use crate::scope_ext::organization_window;
use crate::AppState;

/// `[ASSUMED]` — no fixture covers `POST /v0/pods` with an omitted `name`, unlike inbox creation
/// (fixture 23), so there is no observed shape to reproduce. This is a plain, unconditional
/// default rather than a generated one: a pod name is not a wire-visible identifier the way an
/// inbox's local part is, so there is nothing here for a generated name's *shape* to match.
const DEFAULT_NAME: &str = "New Pod";

// ---- list --------------------------------------------------------------------------------

pub async fn list(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListPodsResponse>, AppError> {
    // The organization mount is always Ready (never a probe, never denied — see
    // `amk_core::scope::Scope::resolve`'s own doc), so there is no scope-masking concern that
    // would require running the permission check after a lookup; check it first.
    permissions::require(&ctx.grants, "pod_read")?;
    let filter = organization_window(&ctx.scope);
    let resolved = q.resolve();

    let page = match filter.pod_id() {
        // A pod- or inbox-scoped credential's window pins exactly one pod. `pods::list` has no
        // single-pod filter — `GET /v0/pods` is its only mount (its own doc) — so post-filtering
        // an org-wide scan would be the exact leak the dispatch contract's pagination section
        // forbids (`count`/`next_page_token` computed after dropping rows). Degenerate to an
        // at-most-one-item page instead.
        Some(pod_id) => {
            degenerate_single_pod(&state.pool, filter.organization_id(), *pod_id, &resolved).await?
        }
        None => {
            let cursor = match &resolved.page_token {
                Some(t) => Some(
                    PodCursor::decode(t)
                        .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?,
                ),
                None => None,
            };
            pods::list(
                &state.pool,
                filter.organization_id(),
                ListPodsQuery { limit: resolved.limit, direction: resolved.direction, cursor },
            )
            .await?
        }
    };

    Ok(Json(ListPodsResponse::new(page.items, resolved.echo_limit, page.next)))
}

async fn degenerate_single_pod(
    pool: &sqlx::PgPool,
    organization_id: &OrganizationId,
    pod_id: PodId,
    resolved: &Resolved,
) -> Result<Page<Pod>, AppError> {
    if resolved.limit == 0 {
        return Ok(Page { items: vec![], next: None });
    }
    if let Some(token) = &resolved.page_token {
        // Validate the token's shape (catches tampered/truncated/wrong-scope input), but a
        // single-pod window's second page is always empty: there is nothing after the one item a
        // first page would already have returned.
        PodCursor::decode(token).map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?;
        return Ok(Page { items: vec![], next: None });
    }
    let item = pods::get(pool, organization_id, pod_id).await?;
    Ok(Page { items: item.into_iter().collect(), next: None })
}

// ---- create --------------------------------------------------------------------------------

pub async fn create(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(req): Json<CreatePodRequest>,
) -> Result<Json<Pod>, AppError> {
    permissions::require(&ctx.grants, "pod_create")?;
    let name = req.name.unwrap_or_else(|| DEFAULT_NAME.to_owned());
    let pod = pods::create(
        &state.pool,
        NewPod {
            organization_id: ctx.identity.organization_id.clone(),
            pod_id: PodId::new_random(),
            client_id: req.client_id,
            name,
        },
    )
    .await?;
    Ok(Json(pod))
}

// ---- get by id -------------------------------------------------------------------------------

pub async fn get(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(pod_id): Path<Uuid>,
) -> Result<Json<Pod>, AppError> {
    // Permission decided BEFORE scope, deliberately the reverse of the ordering this file used to
    // use here (and the opposite of the comment that used to sit on this line). Checking the flag
    // first still satisfies "a 403 must never confirm a foreign pod exists" — it fires before any
    // lookup, so it discloses nothing about ANY pod, foreign or otherwise — and it additionally
    // closes an existence oracle scope-first left open: a credential lacking `pod_read` got 403
    // for an in-scope pod but 404 for a foreign/absent one, letting it learn which ids exist
    // without ever being allowed to read them. Permission-first discloses strictly less; matches
    // every sibling handler in this crate (both creates, all three deletes, all three lists,
    // inboxes::update). `[INFERRED]`: no fixture observes which error the reference API returns
    // for "lacks the read flag AND the pod doesn't exist" — this is a fail-closed reading, not an
    // observation.
    permissions::require(&ctx.grants, "pod_read")?;
    let window = organization_window(&ctx.scope);
    let pod_id = PodId::from(pod_id);
    let row = pods::get(&state.pool, window.organization_id(), pod_id).await?;
    let pod = match row {
        Some(p) => window.check(p)?,
        None => return Err(window.not_found(ResourceKind::Pod).into()),
    };
    Ok(Json(pod))
}

// ---- delete ------------------------------------------------------------------------------

pub async fn delete(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(pod_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let window = organization_window(&ctx.scope);
    let pod_id = PodId::from(pod_id);
    // `pods::delete` pins only `organization_id` and the exact target `pod_id` — it has no
    // narrower "and belongs to pod X" pin the way `inboxes::*` takes an optional `pod_id`, so a
    // pod- or inbox-scoped credential's OWN pod pin has to be checked here, before the delete
    // itself: otherwise it could delete a *different* pod in the same organization.
    if let Some(mine) = window.pod_id() {
        if *mine != pod_id {
            return Err(window.not_found(ResourceKind::Pod).into());
        }
    }
    permissions::require(&ctx.grants, "pod_delete")?;
    // `StoreError::PodNotEmpty` -> `AppError` maps to `cannot_delete`/409 via `ErrorCode::status()`
    // (fixture 22); the `?` below already applies that mapping.
    let deleted = pods::delete(&state.pool, window.organization_id(), pod_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(window.not_found(ResourceKind::Pod).into())
    }
}
