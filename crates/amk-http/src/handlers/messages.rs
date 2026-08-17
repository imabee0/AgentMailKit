//! `/v0/inboxes/{inbox_id}/messages` — the message READ surface.
//!
//! `[SPEC:.claude/contracts/amk-http-message-thread-reads.md]`. Two operations, because
//! `amk-store::messages` offers exactly two: `list` and `get`. `PATCH`, `DELETE`, `search`, the
//! batch pair, `raw` and the attachment download are all named out of scope in the contract, each
//! with the reason it is deferred rather than forgotten — `PATCH`/`DELETE` because the store has no
//! update or delete, which would invert the amk-store -> amk-http write order.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use amk_core::labels::{excluded_labels, system_label_violations, LabelAccess};
use amk_core::permissions;
use amk_core::scope::ResourceKind;
use amk_store::messages::{self, ListMessagesQuery};
use amk_store::pagination::MessageCursor;
use amk_store::StoreError;
use amk_types::ids::{InboxId, MessageId};
use amk_types::message::{
    Addresses, ListMessagesResponse, UpdateMessageRequest, UpdateMessageResponse,
};
use amk_types::{Message, ValidationIssue};

use crate::auth::AuthContext;
use crate::body::{validation_error_many, JsonBody, QueryParams};
use crate::error::AppError;
use crate::ids::decode_segment;
use crate::pagination::ListMailQuery;
use crate::scope_ext::settle_inbox_mount;
use crate::AppState;

/// A NUL-bearing id can never name a real row, so it masks as not-found rather than surfacing a
/// different failure shape — the rule `handlers::inboxes::inbox_id_from_path` already applies to
/// `inbox_id`, applied here to both ids this module takes from a path.
///
/// `message_id` **is** an RFC 5322 angle-bracket Message-ID
/// (`[SPEC:reference/fixtures/03-id-formats.http]`), so its `<`, `>` and `@` arrive
/// percent-encoded. `decode_segment` is the existing decoder; there is deliberately not a second.
fn ids_from_path(raw_inbox: &str, raw_message: &str) -> Result<(InboxId, MessageId), AppError> {
    let inbox = decode_segment(raw_inbox)
        .map_err(|_| amk_core::scope::ScopeDenial::new(ResourceKind::Message))?;
    let message = decode_segment(raw_message)
        .map_err(|_| amk_core::scope::ScopeDenial::new(ResourceKind::Message))?;
    Ok((InboxId::new(inbox), MessageId::new(message)))
}

pub async fn list(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
    QueryParams(q): QueryParams<ListMailQuery>,
) -> Result<Json<ListMessagesResponse>, AppError> {
    let inbox_id = match decode_segment(&raw_inbox_id) {
        Ok(s) => InboxId::new(s),
        Err(_) => return Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    };
    // The inbox mount is settled before anything is read: a credential whose window does not admit
    // this inbox gets the same not-found a genuinely absent inbox gets, and the settled filter is
    // what pins the query.
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "message_read")?;

    let resolved = q.resolve()?;
    // `[SPEC:reference/fixtures/20-search-and-label-precedence.txt]` + register B3: this is a LIST
    // path, so both the `label_*_read` permission and the matching `include_*` flag are required.
    // The excluded set is pushed into the SQL, never applied to a fetched page — post-filtering
    // leaks the hidden rows' count and cursors (`?limit=1` walked across the cursor returns
    // `count:0` with a `next_page_token` on exactly the hidden rows).
    let access = LabelAccess::list(&ctx.grants, q.include_flags());
    let excluded = excluded_labels(&access);

    let cursor = match &resolved.page_token {
        Some(t) => Some(
            MessageCursor::decode(t, filter.inbox_id())
                .map_err(|e| AppError::from(StoreError::InvalidPageToken(e)))?,
        ),
        None => None,
    };
    let page = messages::list(
        &state.pool,
        &filter,
        &excluded,
        ListMessagesQuery { limit: resolved.limit, direction: resolved.direction, cursor },
    )
    .await?;
    Ok(Json(ListMessagesResponse::new(page.items, resolved.echo_limit, page.next)))
}

pub async fn get(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
) -> Result<Json<Message>, AppError> {
    let (inbox_id, message_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "message_read")?;

    // Get-by-id has no `include_*` parameter, so the permission alone decides and restricted mail
    // IS returned — fixture 09b observed exactly this asymmetry with the list path above.
    let access = LabelAccess::by_id(&ctx.grants);
    let excluded = excluded_labels(&access);

    match messages::get(&state.pool, &filter, &inbox_id, &message_id, &excluded).await? {
        Some(m) => Ok(Json(m)),
        None => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    }
}

/// The system-label gate. `[SPEC:reference/fixtures/19-message-label-patch-gate.txt]`.
///
/// It lives here, at the request boundary, and not in `amk-store`: the ingest pipeline owns
/// `sent`/`received`/`bounced`/`scheduled` and applies them through `apply_mutation` directly, so a
/// gate inside the mutation would lock the pipeline out of its own labels.
///
/// Three things the fixture settles that the spec text gets wrong by omission:
/// - the gate applies to **messages** as well as threads (the OpenAPI description mentions it only
///   on `UpdateThreadRequest`, and that omission misled two reviewers and this project's own
///   dispatch);
/// - **restricted is not system** — a client MAY set `spam`/`trash`/`blocked`/`unauthenticated`.
///   Restricted governs who may SEE a label; system governs who may SET one;
/// - one bad label rejects the **whole** mutation, not the valid part, so this runs before any
///   store call and nothing is written when it fires.
pub(crate) fn reject_system_labels(add: &[String], remove: &[String]) -> Result<(), AppError> {
    let violations = system_label_violations(add, remove);
    if violations.is_empty() {
        return Ok(());
    }
    // `path` is `["add_labels", 0]` — field name THEN array index, a mixed string/integer JSON
    // path, verbatim from the fixture's captured body. `code` is `custom`.
    let issues = violations
        .into_iter()
        .map(|v| {
            let mut issue = ValidationIssue::custom(v.message());
            issue.path = vec![
                serde_json::Value::String(v.field.as_field_name().to_owned()),
                serde_json::Value::from(v.index),
            ];
            issue
        })
        .collect();
    Err(validation_error_many(issues))
}

pub async fn update(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
    JsonBody(req): JsonBody<UpdateMessageRequest>,
) -> Result<Json<UpdateMessageResponse>, AppError> {
    let (inbox_id, message_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "message_update")?;

    // `add_labels`/`remove_labels` are `Addresses` — the untagged "one string OR a list" shape the
    // spec uses for every label field ("Label or labels to add to message"). Flatten once, here.
    let add = req.add_labels.map(Addresses::into_vec).unwrap_or_default();
    let remove = req
        .remove_labels
        .map(Addresses::into_vec)
        .unwrap_or_default();
    reject_system_labels(&add, &remove)?;

    match messages::update(&state.pool, &filter, &inbox_id, &message_id, &add, &remove).await? {
        Some(labels) => Ok(Json(UpdateMessageResponse { message_id, labels })),
        None => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let (inbox_id, message_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    permissions::require(&ctx.grants, "message_delete")?;

    match messages::delete(&state.pool, &filter, &inbox_id, &message_id).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    }
}
