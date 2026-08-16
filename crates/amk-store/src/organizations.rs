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
//!
//! # The eight columns migration 0009 added
//!
//! `daily_send_limit`/`five_minute_send_limit`/`first_day_recipient_limit`/
//! `first_week_recipient_limit`/`tracking_allowed`/`authentication_id`/`authentication_type` are
//! **not** in [`NewOrganization`]: no endpoint sets them (the dispatch contract's own words —
//! "operator configuration, reachable today only by a direct `UPDATE`"), so there is no create-
//! time value to accept. `name` is the one exception — `amk init` sets it from
//! `AMK_PRODUCT_NAME` — and is the only field [`NewOrganization`] gained.

use amk_types::ids::OrganizationId;
use amk_types::pod::Organization;
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::error::StoreError;

/// The settable subset of [`Organization`] at creation. See the module doc for why the eight
/// send/receive-limit columns are not here.
pub struct NewOrganization {
    pub organization_id: OrganizationId,
    /// `amk init` sets this from `AMK_PRODUCT_NAME` when set; every other caller in this repo
    /// (every `tests/support::seed_org*` helper) passes `None`.
    pub name: Option<String>,
    pub inbox_limit: Option<u64>,
    pub domain_limit: Option<u64>,
}

/// Every column [`hydrate_row`] reads beyond the three original ones, bundled into one struct
/// purely to keep `hydrate`'s own parameter list from growing to thirteen positional arguments —
/// not a wire shape, never constructed outside this module.
struct NewColumns {
    name: Option<String>,
    daily_send_limit: Option<i64>,
    five_minute_send_limit: Option<i64>,
    first_day_recipient_limit: Option<i64>,
    first_week_recipient_limit: Option<i64>,
    tracking_allowed: Option<bool>,
    authentication_id: Option<String>,
    authentication_type: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn hydrate(
    pool: &PgPool,
    organization_id: String,
    inbox_limit: Option<i64>,
    domain_limit: Option<i64>,
    new_columns: NewColumns,
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
        name: new_columns.name,
        daily_send_limit: new_columns.daily_send_limit.map(|v| v as u64),
        five_minute_send_limit: new_columns.five_minute_send_limit.map(|v| v as u64),
        first_day_recipient_limit: new_columns.first_day_recipient_limit.map(|v| v as u64),
        first_week_recipient_limit: new_columns.first_week_recipient_limit.map(|v| v as u64),
        tracking_allowed: new_columns.tracking_allowed,
        authentication_id: new_columns.authentication_id,
        authentication_type: new_columns.authentication_type,
        // No billing surface, by decision (see the module doc): these three never get a column,
        // so there is nothing to read and nothing that could ever make them anything but `None`.
        billing_id: None,
        billing_type: None,
        billing_subscription_id: None,
        updated_at: Timestamp::from(updated_at),
        created_at: Timestamp::from(created_at),
    })
}

// One literal per query, matching `api_keys.rs`'s idiom — sqlx 0.9's `SqlSafeStr` bound accepts
// only `&'static str`, so the column list is duplicated across `INSERT_SQL`/`GET_SQL` rather than
// built with `format!`.
const INSERT_SQL: &str = "INSERT INTO organizations \
    (organization_id, inbox_limit, domain_limit, name) \
     VALUES ($1, $2, $3, $4) \
     RETURNING organization_id, inbox_limit, domain_limit, name, daily_send_limit, \
        five_minute_send_limit, first_day_recipient_limit, first_week_recipient_limit, \
        tracking_allowed, authentication_id, authentication_type, created_at, updated_at";

const GET_SQL: &str = "SELECT organization_id, inbox_limit, domain_limit, name, \
        daily_send_limit, five_minute_send_limit, first_day_recipient_limit, \
        first_week_recipient_limit, tracking_allowed, authentication_id, authentication_type, \
        created_at, updated_at \
     FROM organizations WHERE organization_id = $1";

async fn hydrate_row(pool: &PgPool, row: PgRow) -> Result<Organization, StoreError> {
    let organization_id: String = row.try_get("organization_id")?;
    let inbox_limit: Option<i64> = row.try_get("inbox_limit")?;
    let domain_limit: Option<i64> = row.try_get("domain_limit")?;
    let new_columns = NewColumns {
        name: row.try_get("name")?,
        daily_send_limit: row.try_get("daily_send_limit")?,
        five_minute_send_limit: row.try_get("five_minute_send_limit")?,
        first_day_recipient_limit: row.try_get("first_day_recipient_limit")?,
        first_week_recipient_limit: row.try_get("first_week_recipient_limit")?,
        tracking_allowed: row.try_get("tracking_allowed")?,
        authentication_id: row.try_get("authentication_id")?,
        authentication_type: row.try_get("authentication_type")?,
    };
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
    hydrate(
        pool,
        organization_id,
        inbox_limit,
        domain_limit,
        new_columns,
        created_at,
        updated_at,
    )
    .await
}

pub async fn create(pool: &PgPool, new: NewOrganization) -> Result<Organization, StoreError> {
    let row = sqlx::query(INSERT_SQL)
        .bind(new.organization_id.as_str())
        .bind(new.inbox_limit.map(|v| v as i64))
        .bind(new.domain_limit.map(|v| v as i64))
        .bind(&new.name)
        .fetch_one(pool)
        .await?;
    hydrate_row(pool, row).await
}

pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
) -> Result<Option<Organization>, StoreError> {
    let row = sqlx::query(GET_SQL)
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
