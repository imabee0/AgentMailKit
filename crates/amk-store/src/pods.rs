//! Pods: the mid-tier scope under an organization.

use amk_types::ids::{has_forbidden_byte, OrganizationId, PodId};
use amk_types::pod::Pod;
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::error::StoreError;
use crate::pagination::{Page, PodCursor, SortDirection};

/// The settable subset of [`Pod`] at creation. `client_id` is the idempotency key —
/// `amk_types::pod::CreatePodRequest`'s own doc: replaying it must return the original row.
pub struct NewPod {
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub client_id: Option<String>,
    pub name: String,
}

fn row_to_pod(row: &PgRow) -> Result<Pod, StoreError> {
    Ok(Pod {
        organization_id: Some(OrganizationId::new(row.try_get::<String, _>("organization_id")?)),
        pod_id: PodId::from(row.try_get::<uuid::Uuid, _>("pod_id")?),
        client_id: row.try_get("client_id")?,
        name: row.try_get("name")?,
        updated_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("updated_at")?),
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

/// Idempotent by `(organization_id, client_id)`: a real `INSERT ... ON CONFLICT` — never a
/// check-then-insert — so replaying the same pair returns the original row rather than a
/// duplicate, even under concurrent replay.
pub async fn create(pool: &PgPool, new: NewPod) -> Result<Pod, StoreError> {
    // Sibling of the identical guard in `inboxes::create`: `client_id` is a free-form
    // caller-supplied idempotency key bound straight into the `INSERT`, and a NUL byte in it
    // would otherwise fail at parameter encoding (SQLSTATE 22021) as a raw `StoreError::Database`
    // rather than this clear, typed rejection.
    if new.client_id.as_deref().is_some_and(has_forbidden_byte) {
        return Err(StoreError::InvalidValue("client_id"));
    }
    // `name` is free-form control-plane text with no P2 owner (the id-safety dispatch guarded
    // only id-typed fields), bound straight into this `INSERT` — a NUL byte would otherwise fail
    // at parameter encoding (SQLSTATE 22021) rather than reject cleanly.
    if has_forbidden_byte(&new.name) {
        return Err(StoreError::InvalidValue("name"));
    }
    let row = sqlx::query(
        "INSERT INTO pods (pod_id, organization_id, client_id, name) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (organization_id, client_id) WHERE client_id IS NOT NULL DO NOTHING \
         RETURNING pod_id, organization_id, client_id, name, created_at, updated_at",
    )
    .bind(new.pod_id.0)
    .bind(new.organization_id.as_str())
    .bind(&new.client_id)
    .bind(&new.name)
    .fetch_optional(pool)
    .await?;

    let row = match row {
        Some(row) => row,
        None => {
            let client_id = new
                .client_id
                .as_deref()
                .expect("invariant: ON CONFLICT only fires when client_id is Some");
            sqlx::query(
                "SELECT pod_id, organization_id, client_id, name, created_at, updated_at \
                 FROM pods WHERE organization_id = $1 AND client_id = $2",
            )
            .bind(new.organization_id.as_str())
            .bind(client_id)
            .fetch_one(pool)
            .await?
        }
    };
    row_to_pod(&row)
}

pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: PodId,
) -> Result<Option<Pod>, StoreError> {
    let row = sqlx::query(
        "SELECT pod_id, organization_id, client_id, name, created_at, updated_at \
         FROM pods WHERE organization_id = $1 AND pod_id = $2",
    )
    .bind(organization_id.as_str())
    .bind(pod_id.0)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_pod).transpose()
}

/// One list request, already resolved to a concrete direction and a decoded cursor — same role as
/// [`crate::messages::ListMessagesQuery`].
pub struct ListPodsQuery {
    pub limit: u64,
    pub direction: SortDirection,
    pub cursor: Option<PodCursor>,
}

const LIST_ASC_SQL: &str =
    "SELECT pod_id, organization_id, client_id, name, created_at, updated_at \
     FROM pods \
     WHERE organization_id = $1 \
       AND ($2::timestamptz IS NULL OR (created_at, pod_id) > ($2, $3)) \
     ORDER BY created_at ASC, pod_id ASC \
     LIMIT $4";

const LIST_DESC_SQL: &str =
    "SELECT pod_id, organization_id, client_id, name, created_at, updated_at \
     FROM pods \
     WHERE organization_id = $1 \
       AND ($2::timestamptz IS NULL OR (created_at, pod_id) < ($2, $3)) \
     ORDER BY created_at DESC, pod_id DESC \
     LIMIT $4";

/// List pods in an organization, paginated. `GET /v0/pods` is this function's only mount — see
/// [`PodCursor`]'s own doc for why it needs no scope pin.
pub async fn list(
    pool: &PgPool,
    organization_id: &OrganizationId,
    query: ListPodsQuery,
) -> Result<Page<Pod>, StoreError> {
    // See the identical guard in `messages::list`/`threads::list`: a zero-row page has no row to
    // anchor a cursor on, so return it directly rather than run a query at all.
    if query.limit == 0 {
        return Ok(Page { items: Vec::new(), next: None });
    }
    let sql = match query.direction {
        SortDirection::Ascending => LIST_ASC_SQL,
        SortDirection::Descending => LIST_DESC_SQL,
    };
    let (cursor_ts, cursor_id) = match &query.cursor {
        Some(c) => (Some(c.created_at), Some(c.pod_id.0)),
        None => (None, None),
    };
    // See the identical comment in `messages::list`: `query.limit` is an unclamped `u64`, so
    // `limit: u64::MAX` or `limit: i64::MAX as u64` must not overflow or wrap `fetch_limit`.
    let fetch_limit = query.limit.saturating_add(1).min(i64::MAX as u64) as i64;

    let rows = sqlx::query(sql)
        .bind(organization_id.as_str())
        .bind(cursor_ts)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await?;

    let has_more = rows.len() as u64 > query.limit;
    let items: Vec<Pod> = rows
        .iter()
        .take(query.limit as usize)
        .map(row_to_pod)
        .collect::<Result<_, _>>()?;

    let next = if has_more {
        let last = items
            .last()
            .expect("has_more implies at least one item when limit > 0");
        Some(PodCursor { created_at: last.created_at.into_inner(), pod_id: last.pod_id }.encode())
    } else {
        None
    };

    Ok(Page { items, next })
}

/// The four foreign keys referencing `pods` (section 3 of the dispatch derivation) that can make
/// this `DELETE` fail `23503` — every one of them, matched by constraint name, never a bare
/// `is_foreign_key_violation()`. A future constraint that also happens to raise `23503` on this
/// statement would otherwise be silently renamed [`StoreError::PodNotEmpty`] and handed
/// `amk-http`'s `409 cannot_delete` for a violation that means something else entirely.
fn is_pod_reference_violation(db_err: &dyn sqlx::error::DatabaseError) -> bool {
    db_err.is_foreign_key_violation()
        && matches!(
            db_err.constraint(),
            Some(
                "inboxes_pod_id_fkey"
                    | "threads_pod_id_fkey"
                    | "messages_pod_id_fkey"
                    | "api_keys_pod_id_fkey"
            )
        )
}

/// Delete a pod. Fixture 22: a pod that still owns an inbox refuses with `cannot_delete` / HTTP
/// 409, and the refusal is **total** — every foreign key referencing `pods` is left at its
/// database default (`NO ACTION`, migration 0008's own comment), so the `DELETE` itself fails
/// outright rather than orphaning or cascading through a referencing row. Contrast
/// [`crate::inboxes::delete`], which cascades — see that decision's own reasoning in the dispatch
/// contract for why the two are deliberately opposite answers.
pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: PodId,
) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM pods WHERE organization_id = $1 AND pod_id = $2")
        .bind(organization_id.as_str())
        .bind(pod_id.0)
        .execute(pool)
        .await;
    match result {
        Ok(result) => Ok(result.rows_affected() > 0),
        Err(sqlx::Error::Database(db_err)) if is_pod_reference_violation(db_err.as_ref()) => {
            Err(StoreError::PodNotEmpty)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_pod_reference_violation` is a private helper: nothing under `tests/` (a separate
    /// crate) can reach it directly, so — like `api_keys::is_prefix_collision`'s own test — this
    /// lives here, as a DB-touching unit test, and skips cleanly when the dev database is
    /// unreachable.
    async fn pool_or_skip() -> Option<PgPool> {
        const DATABASE_URL: &str = "postgres://amk:amk-dev-local@127.0.0.1:55432/amk";
        match crate::connect(DATABASE_URL).await {
            Ok(pool) => Some(pool),
            Err(e) => {
                eprintln!("skipping: dev database unreachable ({e})");
                None
            }
        }
    }

    /// The negative case: every test that reaches `is_pod_reference_violation` through
    /// `pods::delete` triggers one of the four names it matches, by construction — so none of
    /// them can distinguish this predicate from a bare `is_foreign_key_violation()` (the widened
    /// guard `matches!(c, Some("a"|"b"|"c"|"d"))` -> `c.is_some()` would collapse to). This test
    /// manufactures a *different* foreign-key violation — `pods_organization_id_fkey`, the
    /// outgoing FK from `pods` to `organizations`, raised by an `INSERT` rather than the `DELETE`
    /// this predicate actually guards — and asserts the predicate rejects it. Matching by
    /// constraint name, not merely `is_foreign_key_violation()`, is the whole point: a future
    /// constraint that also happens to raise `23503` on the `DELETE` must not be silently
    /// misclassified as `PodNotEmpty`.
    #[tokio::test]
    async fn is_pod_reference_violation_rejects_a_differently_named_fk_violation() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };

        let err = sqlx::query(
            "INSERT INTO pods (pod_id, organization_id, name) VALUES ($1, $2, 'orphan')",
        )
        .bind(uuid::Uuid::new_v4())
        .bind("org-that-does-not-exist")
        .execute(&pool)
        .await
        .expect_err("a pod naming a nonexistent organization must violate a foreign key");
        let sqlx::Error::Database(db_err) = err else {
            panic!("expected a database error, got something else");
        };
        assert_eq!(db_err.constraint(), Some("pods_organization_id_fkey"));
        assert!(db_err.is_foreign_key_violation());
        assert!(
            !is_pod_reference_violation(db_err.as_ref()),
            "a foreign-key violation on a different constraint must not be mistaken for \
             PodNotEmpty"
        );
    }
}
