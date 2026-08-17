//! `/v0/inboxes` (organization mount) and `/v0/pods/{pod_id}/inboxes` (pod mount). Written once,
//! mounted twice — every `*_org` handler and its `*_pod` sibling share the functions below the
//! `// ---- shared ----` markers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use amk_core::permissions;
use amk_core::scope::{Mount, ResourceKind, Scope, ScopeFilter};
use amk_store::inboxes::{self, ListInboxesQuery, NewInbox};
use amk_store::pagination::InboxCursor;
use amk_store::pods;
use amk_store::{Page, StoreError};
use amk_types::ids::{InboxId, OrganizationId, PodId};
use amk_types::inbox::{ListInboxesResponse, MetadataUpdate};
use amk_types::{CreateInboxRequest, ErrorCode, ErrorEnvelope, Inbox, UpdateInboxRequest};

use crate::auth::AuthContext;
use crate::body::{JsonBody, QueryParams};
use crate::error::AppError;
use crate::ids::{decode_segment, PathPodId, PathPodIdString};
use crate::pagination::{ListQuery, Resolved};
use crate::scope_ext::{organization_window, settle_pod_mount};
use crate::words;
use crate::AppState;

/// A NUL-bearing `inbox_id` can never name a real row (`amk_types::ids::has_forbidden_byte`'s
/// rule, matching `amk-store`'s own lookups) — masked identically to a genuinely absent one,
/// rather than surfaced as a different failure shape that would tell a caller "malformed" from
/// "absent".
fn inbox_id_from_path(raw: &str) -> Result<InboxId, amk_core::scope::ScopeDenial> {
    match decode_segment(raw) {
        Ok(s) => Ok(InboxId::new(s)),
        Err(_) => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Inbox)),
    }
}

/// The pod mount's *own resource, fetched by id* does not need `settle_pod_mount`'s probe — the
/// lookup that follows is itself the proof (see `crate::scope_ext`'s module doc), so this takes
/// `Resolved::window()` directly.
fn window_for_pod_own_resource(scope: &Scope, pod_id: PodId) -> Result<ScopeFilter, AppError> {
    match scope.resolve(&Mount::Pod(pod_id)) {
        Ok(resolved) => Ok(resolved.window().clone()),
        Err(denial) => Err(denial.into()),
    }
}

/// Whether `target` is the one inbox an inbox-scoped window is pinned to (or the window pins no
/// inbox at all, in which case any target is a candidate). Case-folded per fixture 18.
fn bound_inbox_matches(filter: &ScopeFilter, target: &InboxId) -> bool {
    match filter.inbox_id() {
        Some(bound) => bound.eq_normalized(target),
        None => true,
    }
}

// ---- list ----------------------------------------------------------------------------------

pub async fn list_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    QueryParams(q): QueryParams<ListQuery>,
) -> Result<Json<ListInboxesResponse>, AppError> {
    permissions::require(&ctx.grants, "inbox_read")?;
    let filter = organization_window(&ctx.scope);
    list_inboxes(&state, &filter, &q).await
}

pub async fn list_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodId(pod_id): PathPodId,
    QueryParams(q): QueryParams<ListQuery>,
) -> Result<Json<ListInboxesResponse>, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    permissions::require(&ctx.grants, "inbox_read")?;
    list_inboxes(&state, &filter, &q).await
}

// ---- shared ----

async fn list_inboxes(
    state: &AppState,
    filter: &ScopeFilter,
    q: &ListQuery,
) -> Result<Json<ListInboxesResponse>, AppError> {
    let resolved = q.resolve()?;
    let page = match filter.inbox_id() {
        // An inbox-scoped credential's window pins exactly one inbox; `inboxes::list` has no
        // single-inbox filter, so this degenerates rather than post-filtering an org/pod-wide
        // scan (the pagination leak the dispatch contract forbids).
        Some(bound) => degenerate_single_inbox(&state.pool, filter, bound, &resolved).await?,
        None => {
            let cursor = match &resolved.page_token {
                Some(t) => Some(
                    InboxCursor::decode(t, filter.pod_id().copied())
                        .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?,
                ),
                None => None,
            };
            inboxes::list(
                &state.pool,
                filter.organization_id(),
                filter.pod_id().copied(),
                ListInboxesQuery { limit: resolved.limit, direction: resolved.direction, cursor },
            )
            .await?
        }
    };
    Ok(Json(ListInboxesResponse::new(page.items, resolved.echo_limit, page.next)))
}

async fn degenerate_single_inbox(
    pool: &sqlx::PgPool,
    filter: &ScopeFilter,
    bound: &InboxId,
    resolved: &Resolved,
) -> Result<Page<Inbox>, AppError> {
    if resolved.limit == 0 {
        return Ok(Page { items: vec![], next: None });
    }
    if let Some(token) = &resolved.page_token {
        InboxCursor::decode(token, filter.pod_id().copied())
            .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?;
        return Ok(Page { items: vec![], next: None });
    }
    let item =
        inboxes::get(pool, filter.organization_id(), filter.pod_id().copied(), bound).await?;
    Ok(Page { items: item.into_iter().collect(), next: None })
}

// ---- create --------------------------------------------------------------------------------

pub async fn create_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    JsonBody(req): JsonBody<CreateInboxRequest>,
) -> Result<Json<Inbox>, AppError> {
    permissions::require(&ctx.grants, "inbox_create")?;
    let filter = organization_window(&ctx.scope);
    let pod_id = match filter.pod_id() {
        Some(p) => *p,
        None => default_pod(&state.pool, filter.organization_id()).await?,
    };
    create_inbox(&state, filter.organization_id().clone(), pod_id, req).await
}

pub async fn create_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodId(pod_id): PathPodId,
    JsonBody(req): JsonBody<CreateInboxRequest>,
) -> Result<Json<Inbox>, AppError> {
    let filter = settle_pod_mount(&state.pool, &ctx.scope, pod_id).await?;
    permissions::require(&ctx.grants, "inbox_create")?;
    let pod_id = filter
        .pod_id()
        .copied()
        .expect("invariant: settle_pod_mount always pins pod_id");
    create_inbox(&state, filter.organization_id().clone(), pod_id, req).await
}

/// `POST /v0/inboxes` at the org mount resolves the pod whose `pod_id` equals the
/// `organization_id` — `amk init` mints the default pod that way
/// (`reference/fixtures/22-org-mount-and-delete-semantics.txt`, Q1). A parse failure or a missing
/// pod is an internal error: never an invented `default_pod_id` field on `Organization`, and
/// never an "oldest pod" fallback (rule 3: not in `amk-types` or a fixture, does not get added).
async fn default_pod(
    pool: &sqlx::PgPool,
    organization_id: &OrganizationId,
) -> Result<PodId, AppError> {
    let uuid: Uuid = organization_id.as_str().parse().map_err(|_| {
        AppError::internal("default-pod resolution: organization_id does not parse as a UUID")
    })?;
    let pod_id = PodId::from(uuid);
    match pods::get(pool, organization_id, pod_id).await? {
        Some(_) => Ok(pod_id),
        None => Err(AppError::internal(
            "default-pod resolution: no pod with pod_id == organization_id (amk init not run?)",
        )),
    }
}

const SUGGESTION_COUNT: usize = 3;
/// How many candidate draws [`collision_suggestions`] makes before returning whatever it found
/// (possibly fewer than [`SUGGESTION_COUNT`]) — see the module doc's random-suffix shape.
///
/// **Accepted, named gap, matching `amk_store::api_keys::MINT_ATTEMPTS`'s own precedent:** the
/// `!exists` branch below — a genuine second collision on a *suggested* candidate — is not pinned
/// by a test. Forcing one through the real `words::suggestion_candidate` random draw (4 random
/// hex digits over a ~10,000-value keyspace per candidate) would need an injectable RNG seam this
/// crate does not have; a black-box probe would be flaky by construction (~0.03% per draw against
/// one pre-seeded collision, astronomically lower against the three this function tries to find).
/// `collision_is_already_exists_403_with_three_suggestions_none_colliding` pins the *shape* (three
/// unique, non-colliding suggestions) every real run produces; the `!exists` skip itself is not
/// separately exercised.
const SUGGESTION_ATTEMPTS: usize = 10;

async fn collision_suggestions(
    pool: &sqlx::PgPool,
    organization_id: &OrganizationId,
    username: &str,
    domain: &str,
) -> Result<Vec<String>, AppError> {
    let mut found: Vec<String> = Vec::new();
    for _ in 0..SUGGESTION_ATTEMPTS {
        if found.len() >= SUGGESTION_COUNT {
            break;
        }
        let candidate_username = words::suggestion_candidate(username);
        if found.contains(&candidate_username) {
            continue;
        }
        let candidate_id = InboxId::new(format!("{candidate_username}@{domain}"));
        let exists = inboxes::get(pool, organization_id, None, &candidate_id)
            .await?
            .is_some();
        if !exists {
            found.push(candidate_username);
        }
    }
    Ok(found)
}

async fn create_inbox(
    state: &AppState,
    organization_id: OrganizationId,
    pod_id: PodId,
    req: CreateInboxRequest,
) -> Result<Json<Inbox>, AppError> {
    let username = req.username.unwrap_or_else(words::generate_username);
    // Both defaults fail closed rather than guess (fixture 23's Q3): AgentMail's own defaults
    // (`agentmail.to`, `"AgentMail"`) name their deployment, not this one.
    let domain = match req.domain {
        Some(d) => d,
        None => state.config.primary_domain.clone().ok_or_else(|| {
            AppError::internal("inbox creation with no domain requires a configured primary_domain")
        })?,
    };
    let display_name = match req.display_name {
        Some(dn) => Some(dn),
        None => Some(state.config.product_name.clone().ok_or_else(|| {
            AppError::internal(
                "inbox creation with no display_name requires a configured product_name",
            )
        })?),
    };

    let inbox_id = InboxId::new(format!("{username}@{domain}"));
    let result = inboxes::create(
        &state.pool,
        NewInbox {
            inbox_id,
            organization_id: organization_id.clone(),
            pod_id,
            client_id: req.client_id,
            display_name,
            metadata: req.metadata,
        },
    )
    .await;

    match result {
        Ok(inbox) => Ok(Json(inbox)),
        Err(StoreError::InboxAlreadyExists) => {
            let suggestions =
                collision_suggestions(&state.pool, &organization_id, &username, &domain).await?;
            Err(ErrorEnvelope::new(ErrorCode::AlreadyExists, "Inbox already exists")
                .with_suggestions(suggestions)
                .into())
        }
        Err(e) => Err(e.into()),
    }
}

// ---- get / update / delete, at either mount ------------------------------------------------

pub async fn get_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
) -> Result<Json<Inbox>, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = organization_window(&ctx.scope);
    get_inbox(&state, &ctx, &window, &target).await
}

pub async fn get_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdString(pod_id, raw_inbox_id): PathPodIdString,
) -> Result<Json<Inbox>, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = window_for_pod_own_resource(&ctx.scope, pod_id)?;
    get_inbox(&state, &ctx, &window, &target).await
}

async fn get_inbox(
    state: &AppState,
    ctx: &AuthContext,
    window: &ScopeFilter,
    target: &InboxId,
) -> Result<Json<Inbox>, AppError> {
    // Permission decided BEFORE scope — before `bound_inbox_matches`, before any store call. This
    // used to run last; it moved to the top for the same reasoning as `handlers::pods::get`'s
    // identical comment, with a sharper stake here: `inbox_id` **is** the email address, directly
    // guessable, unlike a pod's UUID. Scope-first let a credential lacking `inbox_read` get 403
    // for an inbox that exists and 404 for one that does not — an enumeration oracle over real
    // addresses on a public multi-tenant API. Permission-first answers the identical 403 in both
    // cases, before any lookup. `[INFERRED]`: no fixture observes which error the reference API
    // returns for this combination — fail-closed reading, not an observation.
    permissions::require(&ctx.grants, "inbox_read")?;
    if !bound_inbox_matches(window, target) {
        return Err(window.not_found(ResourceKind::Inbox).into());
    }
    let row = inboxes::get(&state.pool, window.organization_id(), window.pod_id().copied(), target)
        .await?;
    // `window.check(i)` is defense-in-depth, not the live guard, at this call site: the query
    // above already pins `window.pod_id()`, and `bound_inbox_matches` above already pins the
    // exact inbox — a mutation-testing pass confirmed no reachable HTTP scope shape produces a
    // row here that `check` would reject and those two did not already exclude. Left in place on
    // purpose (belt-and-suspenders on a security boundary), not because it is exercised.
    let inbox = match row {
        Some(i) => window.check(i)?,
        None => return Err(window.not_found(ResourceKind::Inbox).into()),
    };
    Ok(Json(inbox))
}

pub async fn update_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
    JsonBody(req): JsonBody<UpdateInboxRequest>,
) -> Result<Json<Inbox>, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = organization_window(&ctx.scope);
    update_inbox(&state, &ctx, &window, &target, req).await
}

pub async fn update_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdString(pod_id, raw_inbox_id): PathPodIdString,
    JsonBody(req): JsonBody<UpdateInboxRequest>,
) -> Result<Json<Inbox>, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = window_for_pod_own_resource(&ctx.scope, pod_id)?;
    update_inbox(&state, &ctx, &window, &target, req).await
}

/// `[SPEC:openapi] type_inboxes:UpdateInboxRequest`: "Sending an empty object is rejected... Each
/// update must include at least one of `display_name` or `metadata`." These two rules are
/// amk-http's to own — `amk-store`'s own `inboxes::update` doc says so explicitly, and treats an
/// empty merge as a no-op rather than an error.
fn validate_update(req: &UpdateInboxRequest) -> Result<(), AppError> {
    if req.display_name.is_none() && req.metadata.is_unchanged() {
        return Err(AppError::new(
            ErrorCode::ValidationError,
            "At least one of display_name or metadata must be provided.",
        ));
    }
    if let MetadataUpdate::Merge(m) = &req.metadata {
        if m.is_empty() {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                "metadata must not be an empty object; send null to clear all metadata.",
            ));
        }
    }
    Ok(())
}

async fn update_inbox(
    state: &AppState,
    ctx: &AuthContext,
    window: &ScopeFilter,
    target: &InboxId,
    req: UpdateInboxRequest,
) -> Result<Json<Inbox>, AppError> {
    if !bound_inbox_matches(window, target) {
        return Err(window.not_found(ResourceKind::Inbox).into());
    }
    validate_update(&req)?;
    permissions::require(&ctx.grants, "inbox_update")?;
    let row = inboxes::update(
        &state.pool,
        window.organization_id(),
        window.pod_id().copied(),
        target,
        req,
    )
    .await?;
    // See `get_inbox`'s identical comment: defense-in-depth, not the live guard — the update
    // call above already pins `window.pod_id()`, and `bound_inbox_matches` above already pins
    // the exact inbox.
    let inbox = match row {
        Some(i) => window.check(i)?,
        None => return Err(window.not_found(ResourceKind::Inbox).into()),
    };
    Ok(Json(inbox))
}

pub async fn delete_org(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = organization_window(&ctx.scope);
    delete_inbox(&state, &ctx, &window, &target).await
}

pub async fn delete_pod(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdString(pod_id, raw_inbox_id): PathPodIdString,
) -> Result<StatusCode, AppError> {
    let target = inbox_id_from_path(&raw_inbox_id)?;
    let window = window_for_pod_own_resource(&ctx.scope, pod_id)?;
    delete_inbox(&state, &ctx, &window, &target).await
}

async fn delete_inbox(
    state: &AppState,
    ctx: &AuthContext,
    window: &ScopeFilter,
    target: &InboxId,
) -> Result<StatusCode, AppError> {
    if !bound_inbox_matches(window, target) {
        return Err(window.not_found(ResourceKind::Inbox).into());
    }
    permissions::require(&ctx.grants, "inbox_delete")?;
    let deleted =
        inboxes::delete(&state.pool, window.organization_id(), window.pod_id().copied(), target)
            .await?;
    if deleted {
        // 202: deletion is accepted-then-processed (fixture 22), not the 200 openapi documents.
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(window.not_found(ResourceKind::Inbox).into())
    }
}
