//! `GET /v0/auth/me`, `GET /v0/organizations` — the two operations with no pod/inbox mount.

use axum::extract::State;
use axum::Json;

use amk_types::{Identity, Organization};

use crate::auth::AuthContext;
use crate::error::AppError;
use crate::AppState;

/// `GET /v0/auth/me` — echoes the resolved identity verbatim (`reference/fixtures/01-auth-me.http`
/// is this shape exactly). No permission gate: any resolved credential may ask what it is.
pub async fn auth_me(ctx: AuthContext) -> Json<Identity> {
    Json(ctx.identity)
}

/// `GET /v0/organizations` — "the organization for the authenticated API key", a bare object, not
/// a list envelope despite the plural path (settled by probe — `reference/fixtures/
/// 22-org-mount-and-delete-semantics.txt`'s own contract note). `organizations::list` was deleted
/// (`.claude/contracts/amk-store-http-prereqs.md` decision 5): it took no credential and returned
/// every organization in the deployment, so this route calls `organizations::get` with the
/// resolved identity's own `organization_id`, never a listing call. No permission flag gates it —
/// none of the 36 in `amk_types::api_key::WIRE_NAMES` names this resource.
pub async fn get_organization(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Organization>, AppError> {
    let org = amk_store::organizations::get(&state.pool, &ctx.identity.organization_id)
        .await?
        .ok_or_else(|| {
            // The credential's own organization_id names no organization row: a data-integrity
            // invariant, never a caller-facing not_found — the caller did not name any id at all
            // on this route for a lookup to fail on.
            AppError::internal("resolved identity's organization_id has no organization row")
        })?;
    Ok(Json(org))
}
