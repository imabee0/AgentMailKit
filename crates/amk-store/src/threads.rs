//! Thread storage.
//!
//! [`list`] is a paginated collection and follows the same rule as [`crate::messages::list`]:
//! the restricted-label exclusion is in the `WHERE` clause, never a post-filter.
//!
//! [`get_with_messages`] is the one sanctioned exception the crate root docs call out: a single
//! thread's membership is not paginated, so it is fetched whole (scope-pinned, but *not*
//! label-excluded) and then reduced in memory by `amk_core::labels::redact_thread`, which strips
//! hidden members and recomputes every aggregate that counted them. Pushing the label exclusion
//! into the messages sub-query here would be wrong in a different way: `redact_thread` needs to
//! see a hidden member in order to recompute `message_count`/`size`/`senders`/… without it, not
//! merely to already not have it.
//!
//! `ThreadItem.attachments` is always returned `None` from every query in this module: it is a
//! derived aggregate over member attachments, and the blob/attachment system is out of this
//! dispatch's scope (see the crate root docs). Recorded here rather than guessed at.

use amk_core::labels::{redact_thread, LabelAccess, ThreadRedaction};
use amk_core::scope::ScopeFilter;
use amk_types::ids::{InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::thread::{Thread, ThreadItem};
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::error::StoreError;
use crate::messages::row_to_message;
use crate::pagination::{Page, SortDirection, ThreadCursor};

/// Every settable field of [`ThreadItem`], for seeding a row — mirrors [`crate::messages::NewMessage`].
pub struct NewThread {
    pub thread_id: ThreadId,
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    /// Not yet normalized — folded inside [`insert`].
    pub inbox_id: InboxId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    pub received_timestamp: Option<Timestamp>,
    pub sent_timestamp: Option<Timestamp>,
    pub senders: Vec<String>,
    pub recipients: Vec<String>,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub last_message_id: MessageId,
    pub message_count: u64,
    pub size: u64,
}

pub async fn insert(pool: &PgPool, t: NewThread) -> Result<(), StoreError> {
    let inbox_id = t.inbox_id.normalized();
    sqlx::query(
        "INSERT INTO threads ( \
            thread_id, organization_id, pod_id, inbox_id, labels, \"timestamp\", \
            received_timestamp, sent_timestamp, senders, recipients, subject, preview, \
            last_message_id, message_count, size \
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(t.thread_id.0)
    .bind(t.organization_id.as_str())
    .bind(t.pod_id.0)
    .bind(inbox_id.as_str())
    .bind(&t.labels)
    .bind(t.timestamp.into_inner())
    .bind(t.received_timestamp.map(Timestamp::into_inner))
    .bind(t.sent_timestamp.map(Timestamp::into_inner))
    .bind(&t.senders)
    .bind(&t.recipients)
    .bind(&t.subject)
    .bind(&t.preview)
    .bind(t.last_message_id.as_str())
    .bind(t.message_count as i64)
    .bind(t.size as i64)
    .execute(pool)
    .await?;
    Ok(())
}

fn row_to_thread_item(row: &PgRow) -> Result<ThreadItem, StoreError> {
    Ok(ThreadItem {
        organization_id: Some(OrganizationId::new(row.try_get::<String, _>("organization_id")?)),
        pod_id: Some(PodId::from(row.try_get::<uuid::Uuid, _>("pod_id")?)),
        inbox_id: InboxId::new(row.try_get::<String, _>("inbox_id")?),
        thread_id: ThreadId::from(row.try_get::<uuid::Uuid, _>("thread_id")?),
        labels: row.try_get("labels")?,
        timestamp: Timestamp::from(row.try_get::<DateTime<Utc>, _>("timestamp")?),
        received_timestamp: row
            .try_get::<Option<DateTime<Utc>>, _>("received_timestamp")?
            .map(Timestamp::from),
        sent_timestamp: row
            .try_get::<Option<DateTime<Utc>>, _>("sent_timestamp")?
            .map(Timestamp::from),
        senders: row.try_get("senders")?,
        recipients: row.try_get("recipients")?,
        subject: row.try_get("subject")?,
        preview: row.try_get("preview")?,
        // See the module docs: attachment aggregation is out of this dispatch's scope.
        attachments: None,
        last_message_id: MessageId::new(row.try_get::<String, _>("last_message_id")?),
        message_count: row.try_get::<i64, _>("message_count")? as u64,
        size: row.try_get::<i64, _>("size")? as u64,
        updated_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("updated_at")?),
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

const GET_ITEM_SQL: &str =
    "SELECT thread_id, organization_id, pod_id, inbox_id, labels, \"timestamp\", \
        received_timestamp, sent_timestamp, senders, recipients, subject, preview, \
        last_message_id, message_count, size, created_at, updated_at \
     FROM threads \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND thread_id = $4";

const THREAD_MESSAGES_SQL: &str =
    "SELECT inbox_id, message_id, organization_id, pod_id, thread_id, labels, \"timestamp\", \
        from_address, to_addresses, cc_addresses, bcc_addresses, subject, preview, attachments, \
        in_reply_to, message_references, headers, smtp_id, size, reply_to, body_text, body_html, \
        extracted_text, extracted_html, created_at, updated_at \
     FROM messages \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND thread_id = $3 \
     ORDER BY \"timestamp\" ASC, message_id ASC";

/// Fetch one thread with its messages, ascending by timestamp (`amk_types::thread::Thread`'s own
/// contract), and apply the label-redaction rule to it.
///
/// Membership is fetched whole and *not* label-excluded in SQL — see the module docs for why —
/// then reduced by [`redact_thread`]. A thread every member of which is hidden is indistinguishable
/// from an absent one: this returns `Ok(None)`, and the caller renders that as `not_found`,
/// exactly as a single hidden message does (`amk_core::labels` module docs).
pub async fn get_with_messages(
    pool: &PgPool,
    filter: &ScopeFilter,
    thread_id: ThreadId,
    access: &LabelAccess<'_>,
) -> Result<Option<Thread>, StoreError> {
    let Some(item_row) = sqlx::query(GET_ITEM_SQL)
        .bind(filter.organization_id().as_str())
        .bind(filter.pod_id().map(|p| p.0))
        .bind(filter.inbox_id().map(InboxId::as_str))
        .bind(thread_id.0)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };
    let item = row_to_thread_item(&item_row)?;

    let message_rows = sqlx::query(THREAD_MESSAGES_SQL)
        .bind(filter.organization_id().as_str())
        .bind(filter.pod_id().map(|p| p.0))
        .bind(thread_id.0)
        .fetch_all(pool)
        .await?;
    let messages = message_rows
        .iter()
        .map(row_to_message)
        .collect::<Result<Vec<_>, _>>()?;

    let mut thread = Thread { item, messages };
    match redact_thread(&mut thread, access) {
        ThreadRedaction::Withheld => Ok(None),
        ThreadRedaction::Unchanged | ThreadRedaction::Redacted => Ok(Some(thread)),
    }
}

pub struct ListThreadsQuery {
    pub limit: u64,
    pub direction: SortDirection,
    pub cursor: Option<ThreadCursor>,
}

const LIST_ASC_SQL: &str =
    "SELECT thread_id, organization_id, pod_id, inbox_id, labels, \"timestamp\", \
        received_timestamp, sent_timestamp, senders, recipients, subject, preview, \
        last_message_id, message_count, size, created_at, updated_at \
     FROM threads \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND NOT (labels && $4) \
       AND ($5::timestamptz IS NULL OR (\"timestamp\", thread_id) > ($5, $6)) \
     ORDER BY \"timestamp\" ASC, thread_id ASC \
     LIMIT $7";

const LIST_DESC_SQL: &str =
    "SELECT thread_id, organization_id, pod_id, inbox_id, labels, \"timestamp\", \
        received_timestamp, sent_timestamp, senders, recipients, subject, preview, \
        last_message_id, message_count, size, created_at, updated_at \
     FROM threads \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND NOT (labels && $4) \
       AND ($5::timestamptz IS NULL OR (\"timestamp\", thread_id) < ($5, $6)) \
     ORDER BY \"timestamp\" DESC, thread_id DESC \
     LIMIT $7";

/// List threads in a scope, excluding restricted labels this credential may not see. Same shape
/// and same guarantee as [`crate::messages::list`].
pub async fn list(
    pool: &PgPool,
    filter: &ScopeFilter,
    excluded_labels: &[&str],
    query: ListThreadsQuery,
) -> Result<Page<ThreadItem>, StoreError> {
    let sql = match query.direction {
        SortDirection::Ascending => LIST_ASC_SQL,
        SortDirection::Descending => LIST_DESC_SQL,
    };
    let excluded: Vec<&str> = excluded_labels.to_vec();
    let (cursor_ts, cursor_id) = match &query.cursor {
        Some(c) => (Some(c.timestamp), Some(c.thread_id.0)),
        None => (None, None),
    };
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
    let items: Vec<ThreadItem> = rows
        .iter()
        .take(query.limit as usize)
        .map(row_to_thread_item)
        .collect::<Result<_, _>>()?;

    let next = if has_more {
        let last = items
            .last()
            .expect("has_more implies at least one item when limit > 0");
        Some(
            ThreadCursor {
                thread_id: last.thread_id,
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
