//! `/v0/inboxes/{inbox_id}/messages` — the message READ surface.
//!
//! `[SPEC:.claude/contracts/amk-http-message-thread-reads.md]`. Two operations, because
//! `amk-store::messages` offers exactly two: `list` and `get`. `PATCH`, `DELETE`, `search`, the
//! batch pair, `raw` and the attachment download are all named out of scope in the contract, each
//! with the reason it is deferred rather than forgotten — `PATCH`/`DELETE` because the store has no
//! update or delete, which would invert the amk-store -> amk-http write order.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use amk_core::labels::{excluded_labels, system_label_violations, LabelAccess};
use amk_core::permissions;
use amk_core::scope::ResourceKind;
use amk_outbound::{
    mailbox_addr, reply_all_recipients, reply_subject, sign_and_deliver, OutboundError,
    SendContext, SignedMessage,
};
use amk_store::inboxes;
use amk_store::messages::{self, ListMessagesQuery, NewMessage};
use amk_store::pagination::MessageCursor;
use amk_store::threads::{self, NewThread, ThreadMember};
use amk_store::StoreError;
use amk_types::ids::{InboxId, MessageId, ThreadId};
use amk_types::message::{
    Addresses, ListMessagesResponse, ReplyAllMessageRequest, ReplyToMessageRequest,
    SendMessageRequest, SendMessageResponse, UpdateMessageRequest, UpdateMessageResponse,
};
use amk_types::{ErrorCode, Message, Timestamp, ValidationIssue};

use crate::auth::AuthContext;
use crate::body::{validation_error, validation_error_many, JsonBody, QueryParams};
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

// ---- send / reply / reply-all / forward -------------------------------------------------------

enum ThreadPlan {
    New,
    Join(ThreadId),
}

pub async fn send(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(raw_inbox_id): Path<String>,
    JsonBody(req): JsonBody<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, AppError> {
    let inbox_id = match decode_segment(&raw_inbox_id) {
        Ok(s) => InboxId::new(s),
        Err(_) => return Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    };
    send_prepared(&state, &ctx, &inbox_id, req, SendContext::default(), ThreadPlan::New).await
}

pub async fn reply(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
    JsonBody(req): JsonBody<ReplyToMessageRequest>,
) -> Result<Json<SendMessageResponse>, AppError> {
    let (inbox_id, parent_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let parent = load_parent(&state, &ctx, &inbox_id, &parent_id).await?;
    if req.reply_all_conflicts_with_recipients() {
        return Err(validation_error(ValidationIssue::custom(
            "reply_all is mutually exclusive with to, cc, and bcc",
        )));
    }
    let sending = inbox_id.as_str();
    let (to, cc, bcc) = if req.reply_all == Some(true) {
        let recips = reply_all_recipients(
            &parent.item.from,
            &parent.item.to,
            parent.item.cc.as_deref().unwrap_or(&[]),
            sending,
        );
        (Some(Addresses::Many(recips)), None, None)
    } else if [&req.to, &req.cc, &req.bcc]
        .into_iter()
        .flatten()
        .any(|a| !a.is_empty())
    {
        (req.to, req.cc, req.bcc)
    } else {
        (Some(Addresses::One(mailbox_addr(&parent.item.from).to_owned())), None, None)
    };
    let send_req = SendMessageRequest {
        to,
        cc,
        bcc,
        reply_to: req.reply_to,
        subject: Some(reply_subject(parent.item.subject.as_deref())),
        text: req.text,
        html: req.html,
        labels: req.labels,
        attachments: req.attachments,
        headers: req.headers,
    };
    let send_ctx = reply_context(&parent);
    send_prepared(
        &state,
        &ctx,
        &inbox_id,
        send_req,
        send_ctx,
        ThreadPlan::Join(parent.item.thread_id),
    )
    .await
}

pub async fn reply_all(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
    JsonBody(req): JsonBody<ReplyAllMessageRequest>,
) -> Result<Json<SendMessageResponse>, AppError> {
    let (inbox_id, parent_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let parent = load_parent(&state, &ctx, &inbox_id, &parent_id).await?;
    let recips = reply_all_recipients(
        &parent.item.from,
        &parent.item.to,
        parent.item.cc.as_deref().unwrap_or(&[]),
        inbox_id.as_str(),
    );
    let send_req = SendMessageRequest {
        to: Some(Addresses::Many(recips)),
        cc: None,
        bcc: None,
        reply_to: req.reply_to,
        subject: Some(reply_subject(parent.item.subject.as_deref())),
        text: req.text,
        html: req.html,
        labels: req.labels,
        attachments: req.attachments,
        headers: req.headers,
    };
    let send_ctx = reply_context(&parent);
    send_prepared(
        &state,
        &ctx,
        &inbox_id,
        send_req,
        send_ctx,
        ThreadPlan::Join(parent.item.thread_id),
    )
    .await
}

pub async fn forward(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((raw_inbox_id, raw_message_id)): Path<(String, String)>,
    JsonBody(req): JsonBody<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, AppError> {
    let (inbox_id, parent_id) = ids_from_path(&raw_inbox_id, &raw_message_id)?;
    let _parent = load_parent(&state, &ctx, &inbox_id, &parent_id).await?;
    send_prepared(&state, &ctx, &inbox_id, req, SendContext::default(), ThreadPlan::New).await
}

fn reply_context(parent: &Message) -> SendContext {
    let parent_id = MessageId::bracketed(parent.item.message_id.as_str());
    let mut references: Vec<String> = parent
        .item
        .references
        .as_ref()
        .map(|v| {
            v.iter()
                .map(|m| MessageId::bracketed(m.as_str()).as_str().to_owned())
                .collect()
        })
        .unwrap_or_default();
    if !references.iter().any(|r| r == parent_id.as_str()) {
        references.push(parent_id.as_str().to_owned());
    }
    SendContext {
        from: String::new(),
        in_reply_to: Some(parent_id.as_str().to_owned()),
        references,
    }
}

async fn load_parent(
    state: &AppState,
    ctx: &AuthContext,
    inbox_id: &InboxId,
    message_id: &MessageId,
) -> Result<Message, AppError> {
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, inbox_id).await?;
    permissions::require(&ctx.grants, "message_read")?;
    let access = LabelAccess::by_id(&ctx.grants);
    let excluded = excluded_labels(&access);
    match messages::get(&state.pool, &filter, inbox_id, message_id, &excluded).await? {
        Some(m) => Ok(m),
        None => Err(amk_core::scope::ScopeDenial::new(ResourceKind::Message).into()),
    }
}

async fn send_prepared(
    state: &AppState,
    ctx: &AuthContext,
    inbox_id: &InboxId,
    req: SendMessageRequest,
    mut send_ctx: SendContext,
    thread: ThreadPlan,
) -> Result<Json<SendMessageResponse>, AppError> {
    let filter = settle_inbox_mount(&state.pool, &ctx.scope, inbox_id).await?;
    permissions::require(&ctx.grants, "message_send")?;
    reject_system_labels(&req.labels, &[])?;

    let inbox =
        inboxes::get(&state.pool, filter.organization_id(), filter.pod_id().copied(), inbox_id)
            .await?
            .ok_or_else(|| amk_core::scope::ScopeDenial::new(ResourceKind::Message))?;
    send_ctx.from = inbox.inbox_id.as_str().to_owned();

    let message_id = mint_message_id(&send_ctx.from);
    let signed = sign_and_deliver(&req, &send_ctx, &state.keyring, &message_id, &state.transport)
        .await
        .map_err(outbound_to_app)?;

    let thread_id = persist_sent(state, &inbox, &req, &send_ctx, &signed, thread).await?;
    Ok(Json(SendMessageResponse {
        message_id: MessageId::new(signed.message_id.clone()),
        thread_id,
    }))
}

fn mint_message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("localhost");
    format!("<{}@{}>", uuid::Uuid::new_v4(), domain)
}

fn outbound_to_app(err: OutboundError) -> AppError {
    match err {
        OutboundError::NoSigningKey(d) | OutboundError::UnusableSigningKey(d) => AppError::new(
            ErrorCode::MessageRejected,
            format!("no DKIM key configured for domain {d}"),
        ),
        OutboundError::ForbiddenHeader(name) => {
            let mut issue = ValidationIssue::custom(format!("header {name} is not permitted"));
            issue.path = vec![serde_json::Value::String(name)];
            validation_error(issue)
        }
        OutboundError::Assembly(msg) => {
            let mut issue = ValidationIssue::custom(msg);
            if issue.message.contains("attachment") {
                issue.path = vec![serde_json::Value::String("attachments".into())];
            }
            validation_error(issue)
        }
        OutboundError::Delivery(msg) => {
            AppError::new(ErrorCode::MessageRejected, format!("delivery failed: {msg}"))
        }
    }
}

fn headers_from_raw(raw: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = String::from_utf8_lossy(raw);
    let header_block = text.split("\r\n\r\n").next().unwrap_or("");
    let mut map = BTreeMap::new();
    let mut current_name: Option<String> = None;
    let mut current_val = String::new();
    for line in header_block.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            current_val.push(' ');
            current_val.push_str(line.trim());
            continue;
        }
        if let Some(name) = current_name.take() {
            map.insert(name, std::mem::take(&mut current_val));
        }
        if let Some((n, v)) = line.split_once(':') {
            current_name = Some(n.to_owned());
            current_val = v.trim().to_owned();
        }
    }
    if let Some(name) = current_name {
        map.insert(name, current_val);
    }
    (!map.is_empty()).then_some(map)
}

fn flatten_opt(a: Option<&Addresses>) -> Vec<String> {
    a.cloned().map(Addresses::into_vec).unwrap_or_default()
}

fn preview_of(text: Option<&str>) -> Option<String> {
    let t = text?.trim();
    if t.is_empty() {
        return None;
    }
    let end: usize = t.chars().take(120).map(|c| c.len_utf8()).sum();
    Some(t[..end.min(t.len())].to_owned())
}

fn sent_labels(user: &[String]) -> Vec<String> {
    let mut labels = vec![amk_types::message::labels::SENT.to_owned()];
    for l in user {
        if !labels.iter().any(|e| e == l) {
            labels.push(l.clone());
        }
    }
    labels
}

async fn persist_sent(
    state: &AppState,
    inbox: &amk_types::Inbox,
    req: &SendMessageRequest,
    send_ctx: &SendContext,
    signed: &SignedMessage,
    thread: ThreadPlan,
) -> Result<ThreadId, AppError> {
    let now = Timestamp::now();
    let message_id = MessageId::new(signed.message_id.clone());
    let to = flatten_opt(req.to.as_ref());
    let cc = flatten_opt(req.cc.as_ref());
    let bcc = flatten_opt(req.bcc.as_ref());
    let labels = sent_labels(&req.labels);
    let preview = preview_of(req.text.as_deref());
    let size = signed.raw.len() as u64;
    let in_reply_to = send_ctx.in_reply_to.as_ref().map(MessageId::bracketed);
    let references = if send_ctx.references.is_empty() {
        None
    } else {
        Some(
            send_ctx
                .references
                .iter()
                .map(MessageId::bracketed)
                .collect(),
        )
    };
    let headers = headers_from_raw(&signed.raw);
    let joining = matches!(thread, ThreadPlan::Join(_));

    let thread_id = match thread {
        ThreadPlan::New => {
            let thread_id = ThreadId::new_random();
            threads::insert(
                &state.pool,
                NewThread {
                    thread_id,
                    organization_id: inbox.organization_id.clone().expect("inbox always has org"),
                    pod_id: inbox.pod_id,
                    inbox_id: inbox.inbox_id.clone(),
                    labels: labels.clone(),
                    timestamp: now,
                    received_timestamp: None,
                    sent_timestamp: Some(now),
                    senders: vec![send_ctx.from.clone()],
                    recipients: {
                        let mut r = to.clone();
                        r.extend(cc.clone());
                        r
                    },
                    subject: req.subject.clone(),
                    preview: preview.clone(),
                    last_message_id: message_id.clone(),
                    message_count: 1,
                    size,
                },
            )
            .await?;
            thread_id
        }
        ThreadPlan::Join(thread_id) => thread_id,
    };

    messages::insert(
        &state.pool,
        NewMessage {
            inbox_id: inbox.inbox_id.clone(),
            message_id: message_id.clone(),
            organization_id: inbox.organization_id.clone().expect("inbox always has org"),
            pod_id: inbox.pod_id,
            thread_id,
            labels,
            timestamp: now,
            from: send_ctx.from.clone(),
            to,
            cc: (!cc.is_empty()).then_some(cc.clone()),
            bcc: (!bcc.is_empty()).then_some(bcc),
            subject: req.subject.clone(),
            preview: preview.clone(),
            attachments: None,
            in_reply_to,
            references,
            headers,
            smtp_id: None,
            size,
            reply_to: {
                let v = flatten_opt(req.reply_to.as_ref());
                (!v.is_empty()).then_some(v)
            },
            text: req.text.clone(),
            html: req.html.clone(),
            extracted_text: None,
            extracted_html: None,
        },
    )
    .await?;

    if joining {
        let recorded = threads::record_member(
            &state.pool,
            thread_id,
            ThreadMember {
                last_message_id: message_id,
                timestamp: now,
                sent_timestamp: Some(now),
                sender: send_ctx.from.clone(),
                recipients: {
                    let mut r = flatten_opt(req.to.as_ref());
                    r.extend(flatten_opt(req.cc.as_ref()));
                    r
                },
                preview,
                size,
            },
        )
        .await?;
        if !recorded {
            return Err(amk_core::scope::ScopeDenial::new(ResourceKind::Thread).into());
        }
    }
    Ok(thread_id)
}
