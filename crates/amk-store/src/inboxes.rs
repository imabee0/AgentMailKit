//! Inboxes. `inbox_id` is stored and compared in its normalized (ASCII-lowercased) form —
//! `reference/fixtures/18-inbox-case-normalization.txt` — and that normalized value is the
//! primary key, so two case-variant usernames collide at the database's own unique constraint,
//! never at an application-level check-then-insert.

use amk_types::ids::{InboxId, OrganizationId, PodId};
use amk_types::inbox::{Inbox, Metadata};
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgPool, Row};

use crate::error::StoreError;

/// The settable subset of [`Inbox`] at creation.
pub struct NewInbox {
    /// Not yet normalized — folding happens inside [`create`], per fixture 18: normalization is
    /// the store's job, not the caller's.
    pub inbox_id: InboxId,
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub client_id: Option<String>,
    pub display_name: Option<String>,
    pub metadata: Option<Metadata>,
}

fn row_to_inbox(row: &PgRow) -> Result<Inbox, StoreError> {
    let inbox_id: String = row.try_get("inbox_id")?;
    let metadata: Option<Json<Metadata>> = row.try_get("metadata")?;
    Ok(Inbox {
        organization_id: Some(OrganizationId::new(row.try_get::<String, _>("organization_id")?)),
        pod_id: PodId::from(row.try_get::<uuid::Uuid, _>("pod_id")?),
        email: inbox_id.clone(),
        inbox_id: InboxId::new(inbox_id),
        client_id: row.try_get("client_id")?,
        display_name: row.try_get("display_name")?,
        metadata: metadata.map(|Json(m)| m),
        updated_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("updated_at")?),
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

/// Create an inbox.
///
/// Two independent races, both resolved by the database rather than a check-then-insert:
/// * the same `(organization_id, client_id)` replayed → the `ON CONFLICT` target fires and the
///   *original* row is returned — an idempotent replay, not a duplicate;
/// * the same normalized `inbox_id` (a username collision — fixture 18, and the concurrent-create
///   edge case) → the primary key itself raises a unique violation, mapped here to
///   [`StoreError::InboxAlreadyExists`] rather than propagated as a raw database error.
pub async fn create(pool: &PgPool, new: NewInbox) -> Result<Inbox, StoreError> {
    let normalized = new.inbox_id.normalized();

    let attempt = sqlx::query(
        "INSERT INTO inboxes (inbox_id, organization_id, pod_id, client_id, display_name, metadata) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (organization_id, client_id) WHERE client_id IS NOT NULL DO NOTHING \
         RETURNING inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at",
    )
    .bind(normalized.as_str())
    .bind(new.organization_id.as_str())
    .bind(new.pod_id.0)
    .bind(&new.client_id)
    .bind(&new.display_name)
    .bind(new.metadata.as_ref().map(Json))
    .fetch_optional(pool)
    .await;

    let row = match attempt {
        Ok(Some(row)) => row,
        Ok(None) => {
            // Conflicted on (organization_id, client_id): idempotent replay.
            let client_id = new
                .client_id
                .as_deref()
                .expect("invariant: ON CONFLICT only fires when client_id is Some");
            sqlx::query(
                "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
                 FROM inboxes WHERE organization_id = $1 AND client_id = $2",
            )
            .bind(new.organization_id.as_str())
            .bind(client_id)
            .fetch_one(pool)
            .await?
        }
        Err(sqlx::Error::Database(db_err)) if is_inbox_pkey_violation(db_err.as_ref()) => {
            return Err(StoreError::InboxAlreadyExists);
        }
        Err(e) => return Err(e.into()),
    };

    row_to_inbox(&row)
}

fn is_inbox_pkey_violation(db_err: &dyn sqlx::error::DatabaseError) -> bool {
    db_err.is_unique_violation() && db_err.constraint() == Some("inboxes_pkey")
}

pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
    inbox_id: &InboxId,
) -> Result<Option<Inbox>, StoreError> {
    let normalized = inbox_id.normalized();
    let row = sqlx::query(
        "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
         FROM inboxes WHERE organization_id = $1 AND inbox_id = $2",
    )
    .bind(organization_id.as_str())
    .bind(normalized.as_str())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_inbox).transpose()
}

/// List inboxes in an organization, optionally narrowed to one pod.
pub async fn list(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: Option<PodId>,
) -> Result<Vec<Inbox>, StoreError> {
    let rows = sqlx::query(
        "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
         FROM inboxes \
         WHERE organization_id = $1 AND ($2::uuid IS NULL OR pod_id = $2) \
         ORDER BY created_at ASC, inbox_id ASC",
    )
    .bind(organization_id.as_str())
    .bind(pod_id.map(|p| p.0))
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_inbox).collect()
}

pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    inbox_id: &InboxId,
) -> Result<bool, StoreError> {
    let normalized = inbox_id.normalized();
    let result = sqlx::query("DELETE FROM inboxes WHERE organization_id = $1 AND inbox_id = $2")
        .bind(organization_id.as_str())
        .bind(normalized.as_str())
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
