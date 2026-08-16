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
        // The P1 gate (fixture 25) found these emitted live and absent here. `amk-types` now
        // carries them; giving them a column and a value is the next dispatch's work, so they are
        // explicitly `None` — omitted on the wire — rather than silently defaulted to a number
        // this deployment never configured.
        name: None,
        daily_send_limit: None,
        five_minute_send_limit: None,
        first_day_recipient_limit: None,
        first_week_recipient_limit: None,
        tracking_allowed: None,
        authentication_id: None,
        authentication_type: None,
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

/// Whether this deployment has been initialised at all — a bare boolean, no ids, no rows.
///
/// Exists for exactly one caller: `amk init`, which must refuse to run twice. It cannot use
/// [`get`], because a second `amk init` invocation has no id in hand — it mints a fresh UUID, so
/// nothing it holds could collide with the first run's row, and `create`'s plain `INSERT` would
/// **succeed**, silently minting a second organization, a second default pod and a second root
/// key. An untracked credential with every permission is the worst possible outcome of a typo'd
/// re-run, and "it happens to fail on a unique violation" was never true here.
///
/// Deliberately not a resurrection of `list`, which was deleted for taking no credential and
/// returning every organization in the deployment: this discloses one bit — *some* organization
/// exists — and no identifier, no count, no row. That is the whole difference, and it is why this
/// is safe to expose where `list` was not.
pub async fn exists(pool: &PgPool) -> Result<bool, StoreError> {
    let row: (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM organizations)")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

pub async fn delete(pool: &PgPool, organization_id: &OrganizationId) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM organizations WHERE organization_id = $1")
        .bind(organization_id.as_str())
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
