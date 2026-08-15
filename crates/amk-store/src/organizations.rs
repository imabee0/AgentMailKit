//! Organizations: the top-level tenant.
//!
//! `amk_types::pod::Organization` has no create endpoint on the wire — `reference/openapi.json`
//! exposes only `GET /v0/organizations` — so [`NewOrganization`] is this crate's own
//! insert-parameter struct, built from exactly the settable subset of `Organization`'s own
//! fields. It is not a second wire shape: `billing_*` is never populated (AgentMailKit ships no
//! billing surface — see `amk_types::pod::Organization`'s own doc comment), and
//! `inbox_count`/`domain_count` are computed at read time from the `inboxes` table rather than
//! tracked as columns, so a stored counter can never drift from the rows it counts.
//! `domain_count` is always `0`: there is no `domains` table yet (out of this dispatch's scope).

use amk_types::ids::OrganizationId;
use amk_types::pod::Organization;
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::error::StoreError;

/// The settable subset of [`Organization`] at creation.
pub struct NewOrganization {
    pub organization_id: OrganizationId,
    pub inbox_limit: Option<u64>,
    pub domain_limit: Option<u64>,
}

async fn hydrate(
    pool: &PgPool,
    organization_id: String,
    inbox_limit: Option<i64>,
    domain_limit: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Result<Organization, StoreError> {
    let inbox_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inboxes WHERE organization_id = $1")
            .bind(&organization_id)
            .fetch_one(pool)
            .await?;
    Ok(Organization {
        organization_id: OrganizationId::new(organization_id),
        inbox_count: inbox_count as u64,
        domain_count: 0,
        inbox_limit: inbox_limit.map(|v| v as u64),
        domain_limit: domain_limit.map(|v| v as u64),
        billing_id: None,
        billing_type: None,
        billing_subscription_id: None,
        updated_at: Timestamp::from(updated_at),
        created_at: Timestamp::from(created_at),
    })
}

async fn hydrate_row(pool: &PgPool, row: PgRow) -> Result<Organization, StoreError> {
    let organization_id: String = row.try_get("organization_id")?;
    let inbox_limit: Option<i64> = row.try_get("inbox_limit")?;
    let domain_limit: Option<i64> = row.try_get("domain_limit")?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
    hydrate(pool, organization_id, inbox_limit, domain_limit, created_at, updated_at).await
}

pub async fn create(pool: &PgPool, new: NewOrganization) -> Result<Organization, StoreError> {
    let row = sqlx::query(
        r#"
        INSERT INTO organizations (organization_id, inbox_limit, domain_limit)
        VALUES ($1, $2, $3)
        RETURNING organization_id, inbox_limit, domain_limit, created_at, updated_at
        "#,
    )
    .bind(new.organization_id.as_str())
    .bind(new.inbox_limit.map(|v| v as i64))
    .bind(new.domain_limit.map(|v| v as i64))
    .fetch_one(pool)
    .await?;
    hydrate_row(pool, row).await
}

pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
) -> Result<Option<Organization>, StoreError> {
    let row = sqlx::query(
        "SELECT organization_id, inbox_limit, domain_limit, created_at, updated_at \
         FROM organizations WHERE organization_id = $1",
    )
    .bind(organization_id.as_str())
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Some(hydrate_row(pool, row).await?)),
        None => Ok(None),
    }
}

pub async fn list(pool: &PgPool) -> Result<Vec<Organization>, StoreError> {
    let rows = sqlx::query(
        "SELECT organization_id, inbox_limit, domain_limit, created_at, updated_at \
         FROM organizations ORDER BY created_at ASC, organization_id ASC",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(hydrate_row(pool, row).await?);
    }
    Ok(out)
}

pub async fn delete(pool: &PgPool, organization_id: &OrganizationId) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM organizations WHERE organization_id = $1")
        .bind(organization_id.as_str())
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
