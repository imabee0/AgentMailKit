//! Pods: the mid-tier scope under an organization.

use amk_types::ids::{OrganizationId, PodId};
use amk_types::pod::Pod;
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::error::StoreError;

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

pub async fn list(pool: &PgPool, organization_id: &OrganizationId) -> Result<Vec<Pod>, StoreError> {
    let rows = sqlx::query(
        "SELECT pod_id, organization_id, client_id, name, created_at, updated_at \
         FROM pods WHERE organization_id = $1 ORDER BY created_at ASC, pod_id ASC",
    )
    .bind(organization_id.as_str())
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_pod).collect()
}

pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: PodId,
) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM pods WHERE organization_id = $1 AND pod_id = $2")
        .bind(organization_id.as_str())
        .bind(pod_id.0)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
