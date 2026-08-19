//! `GET .../attachments/{attachment_id}` — the four non-draft mounts of `get-attachment`.
//!
//! `[SPEC:openapi type_attachments:AttachmentResponse]`: metadata plus a time-limited signed
//! `download_url`, mirrored from the raw-message endpoint because the reference serves both the
//! same way — fixture 06 measured the ~1h TTL and the flat 403 after it on the same CDN scheme.
//! The three draft-scoped mounts wait for drafts themselves; the exclusion is recorded where the
//! router mounts these, not silently.
//!
//! # One resolver, four mounts
//!
//! All four handlers converge on [`respond`] with a [`messages::AttachmentScope`] built from
//! whatever the route matched — (inbox, message), (inbox, thread), (pod, thread) or (org, thread).
//! The store query applies the caller's `ScopeFilter` and the excluded-label predicate in SQL,
//! exactly like `raw_blob`: an attachment path that resolved through a laxer query than the read
//! path would hand out the bytes of mail the caller cannot list.
//!
//! # Everything masks as the same 404
//!
//! No such message, out of scope, no such attachment, and metadata-without-a-body are one answer.
//! `attachment_id` is a UUID this server mints, so it is more enumerable than a Message-ID; an
//! endpoint that distinguished "exists but not yours" from "does not exist" would be an existence
//! oracle over every attachment in the deployment.

use axum::extract::{Path, State};
use axum::Json;
use chrono::{Duration as ChronoDuration, Utc};

use amk_core::download;
use amk_core::labels::{excluded_labels, LabelAccess};
use amk_core::permissions;
use amk_core::scope::{ResourceKind, ScopeDenial, ScopeFilter};
use amk_store::messages::{self, AttachmentScope};
use amk_types::message::AttachmentResponse;
use amk_types::Timestamp;

use crate::auth::AuthContext;
use crate::error::AppError;
use crate::handlers::messages::ids_from_path;
use crate::handlers::threads::{inbox_thread_ids_from_path, thread_from_path};
use crate::ids::{decode_segment, PathPodIdStringString};
use crate::scope_ext::{organization_window, settle_inbox_mount};
use crate::AppState;

/// Attachments ride the message-read grant. There is no `attachment_read` among the 38 flags
/// (`amk-types::api_key` is the owner; a flag not there does not exist), and the bytes are part
/// of the message the flag already guards — inventing a finer flag would be a second permission
/// vocabulary for one resource.
const ATTACHMENT_READ: &str = "message_read";

/// `GET /v0/inboxes/{inbox_id}/messages/{message_id}/attachments/{attachment_id}`
pub async fn get_message(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox, raw_message, raw_attachment)): Path<(String, String, String)>,
) -> Result<Json<AttachmentResponse>, AppError> {
    let (inbox_id, message_id) = ids_from_path(&raw_inbox, &raw_message)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    let scope = AttachmentScope {
        inbox_id: Some(&inbox_id),
        message_id: Some(&message_id),
        thread_id: None,
    };
    respond(&state, &ctx, &filter, scope, &raw_attachment).await
}

/// `GET /v0/inboxes/{inbox_id}/threads/{thread_id}/attachments/{attachment_id}`
pub async fn get_inbox_thread(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox, raw_thread, raw_attachment)): Path<(String, String, String)>,
) -> Result<Json<AttachmentResponse>, AppError> {
    let (inbox_id, thread_id) = inbox_thread_ids_from_path(&raw_inbox, &raw_thread)?;
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, &inbox_id).await?;
    let scope = AttachmentScope {
        inbox_id: Some(&inbox_id),
        message_id: None,
        thread_id: Some(&thread_id),
    };
    respond(&state, &ctx, &filter, scope, &raw_attachment).await
}

/// `GET /v0/threads/{thread_id}/attachments/{attachment_id}`
pub async fn get_org_thread(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_thread, raw_attachment)): Path<(String, String)>,
) -> Result<Json<AttachmentResponse>, AppError> {
    let thread_id = thread_from_path(&raw_thread)?;
    let filter = organization_window(&ctx.scope);
    let scope = AttachmentScope { inbox_id: None, message_id: None, thread_id: Some(&thread_id) };
    respond(&state, &ctx, &filter, scope, &raw_attachment).await
}

/// `GET /v0/pods/{pod_id}/threads/{thread_id}/attachments/{attachment_id}`
pub async fn get_pod_thread(
    State(state): State<AppState>,
    ctx: AuthContext,
    PathPodIdStringString(pod_id, raw_thread, raw_attachment): PathPodIdStringString,
) -> Result<Json<AttachmentResponse>, AppError> {
    let thread_id = thread_from_path(&raw_thread)?;
    // A pod-mounted resource fetched by id needs no probe — the lookup below is itself the proof.
    let filter = crate::handlers::inboxes::window_for_pod_own_resource(&ctx.scope, pod_id)?;
    let scope = AttachmentScope { inbox_id: None, message_id: None, thread_id: Some(&thread_id) };
    respond(&state, &ctx, &filter, scope, &raw_attachment).await
}

async fn respond(
    state: &AppState,
    ctx: &AuthContext,
    filter: &ScopeFilter,
    scope: AttachmentScope<'_>,
    raw_attachment: &str,
) -> Result<Json<AttachmentResponse>, AppError> {
    permissions::require(&ctx.grants, ATTACHMENT_READ)?;
    // The id is matched exactly, as stored. It is a UUID this server minted, so a segment that
    // decodes to anything else simply names no row; parsing it first would add a second judgement
    // of validity that has to stay in sync with the minting side for no gain.
    let attachment_id =
        decode_segment(raw_attachment).map_err(|_| ScopeDenial::new(ResourceKind::Attachment))?;

    // By-id access, the same rule as get-by-id and raw: restricted mail IS reachable by id
    // (fixture 09b), so its attachments must be too — one visibility rule, not two.
    let access = LabelAccess::by_id(&ctx.grants);
    let excluded: Vec<String> = excluded_labels(&access)
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let Some((blob_id, attachment)) =
        messages::attachment_blob(&state.pool, filter, scope, attachment_id, &excluded).await?
    else {
        return Err(ScopeDenial::new(ResourceKind::Attachment).into());
    };

    // Configured together or not at all — `amkd` refuses to start with a blob root and no key, so
    // a row carrying a blob id with no key to sign for it is a deployment rollback, not a request
    // error. Same rule and same wording as the raw endpoint.
    let Some(key) = state.config.master_key.as_ref() else {
        return Err(AppError::internal("master key absent while a blob exists"));
    };

    let expires_at = Utc::now() + ChronoDuration::seconds(download::DEFAULT_TTL_SECS as i64);
    let token = download::mint(key, &blob_id, expires_at.timestamp().max(0) as u64);
    let download_url = format!(
        "{}/v0/blobs/{}?token={}",
        state.config.public_base_url.trim_end_matches('/'),
        blob_id,
        token
    );

    Ok(Json(AttachmentResponse {
        attachment,
        download_url,
        expires_at: Timestamp::from(expires_at),
    }))
}
