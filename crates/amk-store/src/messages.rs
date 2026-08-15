//! Message storage: an insert path good enough to seed tests, and the two read paths that carry
//! the crate's two security rules.
//!
//! Every query here — [`get`] and [`list`] alike — pins every coordinate the caller's
//! [`ScopeFilter`] carries (organization always; pod/inbox when pinned) directly in its `WHERE`
//! clause, and [`list`] additionally excludes restricted labels there too, via the predicate
//! `amk-core` hands back from [`amk_core::labels::excluded_labels`]. Neither rule is applied by
//! filtering a fetched row in Rust — see the crate root docs for why that leaks.

use amk_core::scope::ScopeFilter;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::message::{Attachment, Message, MessageItem};
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;

use crate::error::StoreError;
use crate::pagination::{MessageCursor, Page, SortDirection};

/// Every settable field of [`Message`]/[`MessageItem`], for seeding a row. This is a storage
/// insert struct, not a second wire shape: every field name and type is copied from the frozen
/// type it mirrors, and amk-store never re-derives what belongs on the wire.
pub struct NewMessage {
    /// Not yet normalized — folded inside [`insert`], matching [`crate::inboxes`].
    pub inbox_id: InboxId,
    pub message_id: MessageId,
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub thread_id: ThreadId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub attachments: Option<Vec<Attachment>>,
    pub in_reply_to: Option<MessageId>,
    pub references: Option<Vec<MessageId>>,
    pub headers: Option<BTreeMap<String, String>>,
    pub smtp_id: Option<String>,
    pub size: u64,
    pub reply_to: Option<Vec<String>>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub extracted_text: Option<String>,
    pub extracted_html: Option<String>,
}

pub async fn insert(pool: &PgPool, msg: NewMessage) -> Result<(), StoreError> {
    let inbox_id = msg.inbox_id.normalized();
    let references: Option<Vec<String>> = msg
        .references
        .as_ref()
        .map(|v| v.iter().map(|m| m.as_str().to_owned()).collect());

    sqlx::query(
        "INSERT INTO messages ( \
            inbox_id, message_id, organization_id, pod_id, thread_id, labels, \"timestamp\", \
            from_address, to_addresses, cc_addresses, bcc_addresses, subject, preview, \
            attachments, in_reply_to, message_references, headers, smtp_id, size, reply_to, \
            text, html, extracted_text, extracted_html \
         ) VALUES ( \
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, \
            $19, $20, $21, $22, $23, $24 \
         )",
    )
    .bind(inbox_id.as_str())
    .bind(msg.message_id.as_str())
    .bind(msg.organization_id.as_str())
    .bind(msg.pod_id.0)
    .bind(msg.thread_id.0)
    .bind(&msg.labels)
    .bind(msg.timestamp.into_inner())
    .bind(&msg.from)
    .bind(&msg.to)
    .bind(&msg.cc)
    .bind(&msg.bcc)
    .bind(&msg.subject)
    .bind(&msg.preview)
    .bind(msg.attachments.as_ref().map(Json))
    .bind(msg.in_reply_to.as_ref().map(MessageId::as_str))
    .bind(&references)
    .bind(msg.headers.as_ref().map(Json))
    .bind(&msg.smtp_id)
    .bind(msg.size as i64)
    .bind(&msg.reply_to)
    .bind(&msg.text)
    .bind(&msg.html)
    .bind(&msg.extracted_text)
    .bind(&msg.extracted_html)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) fn row_to_message_item(row: &PgRow) -> Result<MessageItem, StoreError> {
    let references: Option<Vec<String>> = row.try_get("message_references")?;
    let attachments: Option<Json<Vec<Attachment>>> = row.try_get("attachments")?;
    let headers: Option<Json<BTreeMap<String, String>>> = row.try_get("headers")?;
    Ok(MessageItem {
        organization_id: Some(OrganizationId::new(row.try_get::<String, _>("organization_id")?)),
        pod_id: Some(PodId::from(row.try_get::<uuid::Uuid, _>("pod_id")?)),
        inbox_id: InboxId::new(row.try_get::<String, _>("inbox_id")?),
        thread_id: ThreadId::from(row.try_get::<uuid::Uuid, _>("thread_id")?),
        message_id: MessageId::new(row.try_get::<String, _>("message_id")?),
        labels: row.try_get("labels")?,
        timestamp: Timestamp::from(row.try_get::<DateTime<Utc>, _>("timestamp")?),
        from: row.try_get("from_address")?,
        to: row.try_get("to_addresses")?,
        cc: row.try_get("cc_addresses")?,
        bcc: row.try_get("bcc_addresses")?,
        subject: row.try_get("subject")?,
        preview: row.try_get("preview")?,
        attachments: attachments.map(|Json(a)| a),
        in_reply_to: row
            .try_get::<Option<String>, _>("in_reply_to")?
            .map(MessageId::new),
        references: references.map(|v| v.into_iter().map(MessageId::new).collect()),
        headers: headers.map(|Json(h)| h),
        smtp_id: row.try_get("smtp_id")?,
        size: row.try_get::<i64, _>("size")? as u64,
        updated_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("updated_at")?),
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

pub(crate) fn row_to_message(row: &PgRow) -> Result<Message, StoreError> {
    let item = row_to_message_item(row)?;
    Ok(Message {
        item,
        reply_to: row.try_get("reply_to")?,
        text: row.try_get("text")?,
        html: row.try_get("html")?,
        extracted_text: row.try_get("extracted_text")?,
        extracted_html: row.try_get("extracted_html")?,
    })
}

const GET_SQL: &str =
    "SELECT inbox_id, message_id, organization_id, pod_id, thread_id, labels, \"timestamp\", \
        from_address, to_addresses, cc_addresses, bcc_addresses, subject, preview, attachments, \
        in_reply_to, message_references, headers, smtp_id, size, reply_to, text, html, \
        extracted_text, extracted_html, created_at, updated_at \
     FROM messages \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND inbox_id = $3 \
       AND message_id = $4 \
       AND NOT (labels && $5)";

/// Fetch one message by id. The scope pin and the restricted-label exclusion are both in the
/// `WHERE` clause: a row this credential may not see is never fetched, so there is nothing to
/// post-filter and nothing to leak through a denial's shape.
pub async fn get(
    pool: &PgPool,
    filter: &ScopeFilter,
    inbox_id: &InboxId,
    message_id: &MessageId,
    excluded_labels: &[&str],
) -> Result<Option<Message>, StoreError> {
    let normalized_inbox = inbox_id.normalized();
    let excluded: Vec<&str> = excluded_labels.to_vec();
    let row = sqlx::query(GET_SQL)
        .bind(filter.organization_id().as_str())
        .bind(filter.pod_id().map(|p| p.0))
        .bind(normalized_inbox.as_str())
        .bind(message_id.as_str())
        .bind(&excluded)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_message).transpose()
}

/// One list request, already resolved to a concrete direction and a decoded (and scope-validated)
/// cursor — resolving `ListParams`' `Option<bool>`/`Option<String>` into these is the caller's
/// job, not this crate's.
pub struct ListMessagesQuery {
    pub limit: u64,
    pub direction: SortDirection,
    pub cursor: Option<MessageCursor>,
}

const LIST_ASC_SQL: &str =
    "SELECT inbox_id, message_id, organization_id, pod_id, thread_id, labels, \"timestamp\", \
        from_address, to_addresses, cc_addresses, bcc_addresses, subject, preview, attachments, \
        in_reply_to, message_references, headers, smtp_id, size, reply_to, text, html, \
        extracted_text, extracted_html, created_at, updated_at \
     FROM messages \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND NOT (labels && $4) \
       AND ($5::timestamptz IS NULL OR (\"timestamp\", message_id) > ($5, $6)) \
     ORDER BY \"timestamp\" ASC, message_id ASC \
     LIMIT $7";

const LIST_DESC_SQL: &str =
    "SELECT inbox_id, message_id, organization_id, pod_id, thread_id, labels, \"timestamp\", \
        from_address, to_addresses, cc_addresses, bcc_addresses, subject, preview, attachments, \
        in_reply_to, message_references, headers, smtp_id, size, reply_to, text, html, \
        extracted_text, extracted_html, created_at, updated_at \
     FROM messages \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND NOT (labels && $4) \
       AND ($5::timestamptz IS NULL OR (\"timestamp\", message_id) < ($5, $6)) \
     ORDER BY \"timestamp\" DESC, message_id DESC \
     LIMIT $7";

/// List messages in a scope, excluding restricted labels this credential may not see.
///
/// The exclusion, the scope pins and the keyset comparison are all in one `WHERE` clause (one of
/// exactly two fixed literals, chosen by [`SortDirection`] — the direction is never formatted
/// into the query text). A row this call excludes is never fetched, so it cannot be counted,
/// cannot consume a page slot, and cannot appear in the returned cursor — the regression this
/// crate exists to prevent (`reference/fixtures/09b-unauthenticated-variant.txt`).
pub async fn list(
    pool: &PgPool,
    filter: &ScopeFilter,
    excluded_labels: &[&str],
    query: ListMessagesQuery,
) -> Result<Page<MessageItem>, StoreError> {
    let sql = match query.direction {
        SortDirection::Ascending => LIST_ASC_SQL,
        SortDirection::Descending => LIST_DESC_SQL,
    };
    let excluded: Vec<&str> = excluded_labels.to_vec();
    let (cursor_ts, cursor_id) = match &query.cursor {
        Some(c) => (Some(c.timestamp), Some(c.message_id.as_str().to_owned())),
        None => (None, None),
    };
    // Fetch one extra row to know whether a next page exists, without a second round trip.
    let fetch_limit = query.limit as i64 + 1;

    let rows = sqlx::query(sql)
        .bind(filter.organization_id().as_str())
        .bind(filter.pod_id().map(|p| p.0))
        .bind(filter.inbox_id().map(InboxId::as_str))
        .bind(&excluded)
        .bind(cursor_ts)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await?;

    let has_more = rows.len() as u64 > query.limit;
    let items: Vec<MessageItem> = rows
        .iter()
        .take(query.limit as usize)
        .map(row_to_message_item)
        .collect::<Result<_, _>>()?;

    let next = if has_more {
        let last = items
            .last()
            .expect("has_more implies at least one item when limit > 0");
        Some(
            MessageCursor {
                message_id: last.message_id.clone(),
                inbox_id: last.inbox_id.clone(),
                timestamp: last.timestamp.into_inner(),
            }
            .encode(),
        )
    } else {
        None
    };

    Ok(Page { items, next })
}
