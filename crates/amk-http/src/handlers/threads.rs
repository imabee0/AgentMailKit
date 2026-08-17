//! `/v0/threads`, `/v0/pods/{pod_id}/threads` and `/v0/inboxes/{inbox_id}/threads` — the thread
//! READ surface, written once and mounted three times.
//!
//! `[SPEC:.claude/contracts/amk-http-message-thread-reads.md]`. Two operations per mount, because
//! `amk-store::threads` offers exactly two: `list` and `get_with_messages`. `PATCH`, `DELETE`,
//! `search` and the attachment download are named out of scope in the contract — `PATCH`/`DELETE`
//! because the store has no update or delete, and mounting them first would invert the
//! amk-store -> amk-http write order.
//!
//! Mount handling mirrors `handlers::inboxes` exactly: `organization_window` for the org mount,
//! `settle_pod_mount` for the pod mount, `settle_inbox_mount` for the inbox mount.

use axum::extract::State;
use axum::Json;

use amk_core::labels::{excluded_labels, LabelAccess};
use amk_core::permissions;
use amk_core::scope::{ResourceKind, ScopeFilter};
use amk_store::pagination::ThreadCursor;
use amk_store::threads::{self, ListThreadsQuery};
use amk_store::StoreError;
use amk_types::ids::{InboxId, ThreadId};
use amk_types::thread::ListThreadsResponse;
use amk_types::Thread;

use crate::auth::AuthContext;
use crate::body::QueryParams;
use crate::error::AppError;
use crate::ids::{decode_segment, PathPodId, PathPodIdString};
use crate::pagination::ListMailQuery;
use crate::scope_ext::{organization_window, settle_inbox_mount, settle_pod_mount};
use crate::AppState;

/// Threads are gated by **`message_read`**, not by a flag of their own.
///
/// `amk_types::api_key::WIRE_NAMES` is the whole permission vocabulary — 38 flags — and there is no
/// `thread_read` in it; the field doc on `message_read` says so outright: *"Read messages. Also
/// required to read threads."* An earlier draft of this module invented `thread_read`, which
/// `permissions::require` faithfully refused for every credential including an unrestricted one's
/// restricted siblings. Non-negotiable 3: a flag that is not in `amk-types` does not get added.
const THREAD_READ: &str = "message_read";

// ---- list ---------------------------------------------------------------------------------

pub async fn list_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    QueryParams(q): QueryParams<ListMailQuery>,
) -> Result<Json<ListThreadsResponse>, AppError> {
    permissions::require(&ctx.grants, THREAD_READ)?;
    let filter = organization_window(&ctx.scope);
    list_threads(&state, &ctx, &filter, &q).await
}

pub async fn list_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodId(pod_id): PathPodId,
    QueryParams(q): QueryParams<ListMailQuery>,
) -> Result<Json<ListThreadsResponse>, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    permissions::require(&ctx.grants, THREAD_READ)?;
    list_threads(&state, &ctx, &filter, &q).await
}

pub async fn list_inbox(
    State(state): State<AppState>,
    ctx: AuthContext,
    axum::extract::Path(raw_inbox_id): axum::extract::Path<String>,
    QueryParams(q): QueryParams<ListMailQuery>,
) -> Result<Json<ListThreadsResponse>, AppError> {
    let inbox_id = inbox_from_path(&raw_inbox_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, THREAD_READ)?;
    list_threads(&state, &ctx, &filter, &q).await
}

// ---- shared ----

fn inbox_from_path(raw: &str) -> Result<InboxId, AppError> {
    match decode_segment(raw) {
        Ok(s) => Ok(InboxId::new(s)),
        Err(_) => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Thread).into()),
    }
}

async fn list_threads(
    state: &AppState,
    ctx: &AuthContext,
    filter: &ScopeFilter,
    q: &ListMailQuery,
) -> Result<Json<ListThreadsResponse>, AppError> {
    let resolved = q.resolve()?;
    // `[SPEC:reference/fixtures/20-search-and-label-precedence.txt]` + register B3: a LIST path
    // needs both the `label_*_read` permission and the matching `include_*` flag, and the excluded
    // set is pushed into the SQL rather than applied to a fetched page — post-filtering discloses
    // the hidden rows' count and cursors.
    let access = LabelAccess::list(&ctx.grants, q.include_flags());
    let excluded = excluded_labels(&access);

    let cursor = match &resolved.page_token {
        Some(t) => Some(
            ThreadCursor::decode(t, filter.inbox_id())
                .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?,
        ),
        None => None,
    };
    let page = threads::list(
        &state.pool,
        filter,
        &excluded,
        ListThreadsQuery { limit: resolved.limit, direction: resolved.direction, cursor },
    )
    .await?;
    Ok(Json(ListThreadsResponse::new(page.items, resolved.echo_limit, page.next)))
}

// ---- get by id ----------------------------------------------------------------------------

pub async fn get_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    axum::extract::Path(raw_thread_id): axum::extract::Path<String>,
) -> Result<Json<Thread>, AppError> {
    let thread_id = thread_from_path(&raw_thread_id)?;
    permissions::require(&ctx.grants, THREAD_READ)?;
    let filter = organization_window(&ctx.scope);
    get_thread(&state, &ctx, &filter, thread_id).await
}

pub async fn get_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdString(pod_id, raw_thread_id): PathPodIdString,
) -> Result<Json<Thread>, AppError> {
    let thread_id = thread_from_path(&raw_thread_id)?;
    // A pod-mounted resource fetched by id needs no probe — the lookup below is itself the proof.
    let filter = crate::handlers::inboxes::window_for_pod_own_resource(&ctx.scope, pod_id)?;
    permissions::require(&ctx.grants, THREAD_READ)?;
    get_thread(&state, &ctx, &filter, thread_id).await
}

pub async fn get_inbox(
    State(state): State<AppState>,
    ctx: AuthContext,
    axum::extract::Path((raw_inbox_id, raw_thread_id)): axum::extract::Path<(String, String)>,
) -> Result<Json<Thread>, AppError> {
    let inbox_id = inbox_from_path(&raw_inbox_id)?;
    let thread_id = thread_from_path(&raw_thread_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, THREAD_READ)?;
    get_thread(&state, &ctx, &filter, thread_id).await
}

// ---- shared ----

/// `thread_id` is a UUID. A segment that is not one names no row, so it masks as not-found rather
/// than as a distinct "malformed" shape — the same rule `crate::ids::PathPodId` applies to
/// `pod_id`, and the reason a scope miss and an absent row are indistinguishable here.
fn thread_from_path(raw: &str) -> Result<ThreadId, AppError> {
    let decoded =
        decode_segment(raw).map_err(|_| amk_core::scope::ScopeDenial::new(ResourceKind::Thread))?;
    match decoded.parse::<uuid::Uuid>() {
        Ok(u) => Ok(ThreadId::from(u)),
        Err(_) => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Thread).into()),
    }
}

async fn get_thread(
    state: &AppState,
    ctx: &AuthContext,
    filter: &ScopeFilter,
    thread_id: ThreadId,
) -> Result<Json<Thread>, AppError> {
    // Get-by-id carries no `include_*` parameter, so the permission alone decides — fixture 09b's
    // asymmetry, the same one `handlers::messages::get` relies on.
    let access = LabelAccess::by_id(&ctx.grants);
    match threads::get_with_messages(&state.pool, filter, thread_id, &access).await? {
        Some(t) => Ok(Json(t)),
        None => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Thread).into()),
    }
}
