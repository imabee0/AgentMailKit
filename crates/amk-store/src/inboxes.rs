//! Inboxes. `inbox_id` is stored and compared in its normalized (ASCII-lowercased) form —
//! `reference/fixtures/18-inbox-case-normalization.txt` — and that normalized value is the
//! primary key, so two case-variant usernames collide at the database's own unique constraint,
//! never at an application-level check-then-insert.

use amk_types::ids::{has_forbidden_byte, InboxId, OrganizationId, PodId};
use amk_types::inbox::{Inbox, Metadata, MetadataUpdate, MetadataValue, UpdateInboxRequest};
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;

use crate::error::{PageTokenError, StoreError};
use crate::pagination::{InboxCursor, Page, SortDirection};

/// Metadata is exposed through both its keys and its values — measured against the dev database,
/// `'{"a\0b":"v"}'::jsonb` and `'{"k":"a\0b"}'::jsonb` both raise `54000 null character not
/// permitted`, unguarded, as a raw `StoreError::Database` rather than a typed rejection. Checking
/// only the value (or only the key) leaves the other half of this reachable.
fn metadata_value_has_forbidden_byte(v: &MetadataValue) -> bool {
    matches!(v, MetadataValue::String(s) if has_forbidden_byte(s))
}

/// [`Metadata`] — the state carried by [`NewInbox::metadata`] and `Inbox.metadata` itself: every
/// value present, no per-key deletion. Checks both key and value, at the only nesting level
/// [`MetadataValue`] permits (it is a flat scalar enum — no arrays or nested objects).
fn metadata_has_forbidden_byte(m: &Metadata) -> bool {
    m.iter()
        .any(|(k, v)| has_forbidden_byte(k) || metadata_value_has_forbidden_byte(v))
}

/// The `MetadataUpdate::Merge` map: each value is `Option<MetadataValue>` (`None` deletes that
/// key), so this checks the key always and the value only when present.
fn merge_map_has_forbidden_byte(m: &BTreeMap<String, Option<MetadataValue>>) -> bool {
    m.iter().any(|(k, v)| {
        has_forbidden_byte(k) || v.as_ref().is_some_and(metadata_value_has_forbidden_byte)
    })
}

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
    // `inbox_id` (the username) and `client_id` both arrive in the request body — neither travels
    // through `from_path_segment`, so neither is covered by the path-segment door — and both are
    // bound straight into the `INSERT` below. A NUL byte in either would otherwise fail at
    // parameter encoding (SQLSTATE 22021): an ungraceful `StoreError::Database`, not the masking
    // defect the lookups target (this is an insert, not a lookup with a not-found to hide behind),
    // but caller-controlled input that 500s unnecessarily is worth a clear, typed rejection now
    // that the check is one line. Named per field, not one shared `InvalidValue("id")`: a caller
    // that cannot tell which of the two it got wrong will retry with the same bad payload.
    if has_forbidden_byte(new.inbox_id.as_str()) {
        return Err(StoreError::InvalidValue("inbox_id"));
    }
    if new.client_id.as_deref().is_some_and(has_forbidden_byte) {
        return Err(StoreError::InvalidValue("client_id"));
    }
    // `display_name` and `metadata` are the same kind of free-form control-plane text, with no P2
    // owner (that's mail *content*, guarded — or not — inside amk-ingest): a caller-body field
    // bound straight into this `INSERT`, so an unguarded NUL byte in either would 500 rather than
    // reject cleanly.
    if new.display_name.as_deref().is_some_and(has_forbidden_byte) {
        return Err(StoreError::InvalidValue("display_name"));
    }
    if new
        .metadata
        .as_ref()
        .is_some_and(metadata_has_forbidden_byte)
    {
        return Err(StoreError::InvalidValue("metadata"));
    }
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

/// `pod_id: None` means the caller is mounted at the organization (spans every pod in it, like
/// [`list`]); `Some(p)` pins to that one pod — see the module-level note on why `get`/`delete`
/// used to pin `organization_id` only.
pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: Option<PodId>,
    inbox_id: &InboxId,
) -> Result<Option<Inbox>, StoreError> {
    // A NUL-bearing `inbox_id` can never name a real row (Postgres `text` cannot hold one), so
    // this is not-found by definition — never the `StoreError::Database` a bound `%00` would
    // otherwise raise at parameter encoding (SQLSTATE 22021). Checked ahead of any query, per
    // this crate's rule that denial and absence must be indistinguishable.
    if has_forbidden_byte(inbox_id.as_str()) {
        return Ok(None);
    }
    let normalized = inbox_id.normalized();
    let row = sqlx::query(
        "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
         FROM inboxes WHERE organization_id = $1 AND inbox_id = $2 AND ($3::uuid IS NULL OR pod_id = $3)",
    )
    .bind(organization_id.as_str())
    .bind(normalized.as_str())
    .bind(pod_id.map(|p| p.0))
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_inbox).transpose()
}

/// Lookup by the `inbox_id` primary key alone. RCPT has only the address;
/// `inbox_id` is the PK (`0003_inboxes.sql`). Same row as [`get`].
pub async fn get_by_inbox_id(
    pool: &PgPool,
    inbox_id: &InboxId,
) -> Result<Option<Inbox>, StoreError> {
    if has_forbidden_byte(inbox_id.as_str()) {
        return Ok(None);
    }
    let normalized = inbox_id.normalized();
    let row = sqlx::query(
        "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
         FROM inboxes WHERE inbox_id = $1",
    )
    .bind(normalized.as_str())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_inbox).transpose()
}

/// One list request, already resolved to a concrete direction and a decoded (and scope-validated)
/// cursor — same role as [`crate::messages::ListMessagesQuery`].
pub struct ListInboxesQuery {
    pub limit: u64,
    pub direction: SortDirection,
    pub cursor: Option<InboxCursor>,
}

const LIST_ASC_SQL: &str =
    "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
     FROM inboxes \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::timestamptz IS NULL OR (created_at, inbox_id) > ($3, $4)) \
     ORDER BY created_at ASC, inbox_id ASC \
     LIMIT $5";

const LIST_DESC_SQL: &str =
    "SELECT inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at \
     FROM inboxes \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::timestamptz IS NULL OR (created_at, inbox_id) < ($3, $4)) \
     ORDER BY created_at DESC, inbox_id DESC \
     LIMIT $5";

/// List inboxes in an organization, optionally narrowed to one pod, paginated. `pod_id` is this
/// query's own scope pin — see [`get`]'s doc for its meaning — and is what
/// [`InboxCursor::decode`]'s `pinned` argument must have been checked against before the cursor
/// reached here.
pub async fn list(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: Option<PodId>,
    query: ListInboxesQuery,
) -> Result<Page<Inbox>, StoreError> {
    // See the identical guard in `messages::list`/`threads::list`: a zero-row page has no row to
    // anchor a cursor on, so return it directly rather than run a query at all.
    if query.limit == 0 {
        return Ok(Page { items: Vec::new(), next: None });
    }
    // Sibling of the identical guard in `messages::list`/`threads::list`: `InboxCursor`'s fields
    // are `pub`, so nothing at the type level guarantees a cursor reaching this function went
    // through `InboxCursor::decode` first — defense in depth for the one free-text field it
    // carries, ahead of any query.
    if let Some(c) = &query.cursor {
        if has_forbidden_byte(c.inbox_id.as_str()) {
            return Err(StoreError::InvalidPageToken(PageTokenError::ForbiddenByte(
                "cursor.inbox_id",
            )));
        }
    }
    let sql = match query.direction {
        SortDirection::Ascending => LIST_ASC_SQL,
        SortDirection::Descending => LIST_DESC_SQL,
    };
    let (cursor_ts, cursor_id) = match &query.cursor {
        Some(c) => (Some(c.created_at), Some(c.inbox_id.as_str().to_owned())),
        None => (None, None),
    };
    // See the identical comment in `messages::list`: `query.limit` is an unclamped `u64`, so
    // `limit: u64::MAX` or `limit: i64::MAX as u64` must not overflow or wrap `fetch_limit`.
    let fetch_limit = query.limit.saturating_add(1).min(i64::MAX as u64) as i64;

    let rows = sqlx::query(sql)
        .bind(organization_id.as_str())
        .bind(pod_id.map(|p| p.0))
        .bind(cursor_ts)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await?;

    let has_more = rows.len() as u64 > query.limit;
    let items: Vec<Inbox> = rows
        .iter()
        .take(query.limit as usize)
        .map(row_to_inbox)
        .collect::<Result<_, _>>()?;

    let next = if has_more {
        let last = items
            .last()
            .expect("has_more implies at least one item when limit > 0");
        Some(
            InboxCursor {
                created_at: last.created_at.into_inner(),
                inbox_id: last.inbox_id.clone(),
                pod_id: last.pod_id,
            }
            .encode(),
        )
    } else {
        None
    };

    Ok(Page { items, next })
}

/// See [`get`]'s doc for `pod_id`'s meaning.
///
/// Unconditional, per fixture 22 (`DELETE /v0/inboxes/{inbox_id}` returned 202 with no emptiness
/// precondition of any kind): migration 0008 cascades every FK referencing `inboxes`
/// (`threads`/`messages`/`api_keys`, plus `messages_thread_id_fkey`), so deleting an inbox that
/// owns threads, messages and an inbox-scoped api key removes all of them in one statement.
/// Contrast [`crate::pods::delete`], which refuses instead — see that function's own doc for why
/// the two are deliberately opposite answers.
pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: Option<PodId>,
    inbox_id: &InboxId,
) -> Result<bool, StoreError> {
    // Sibling of the same check in `get`, written independently here rather than assumed to
    // follow from it: the previous dispatch's fifth review round found exactly this asymmetry —
    // `get` guarded, `delete` not — surviving a mutation with the suite green.
    if has_forbidden_byte(inbox_id.as_str()) {
        return Ok(false);
    }
    let normalized = inbox_id.normalized();
    let result = sqlx::query(
        "DELETE FROM inboxes WHERE organization_id = $1 AND inbox_id = $2 \
         AND ($3::uuid IS NULL OR pod_id = $3)",
    )
    .bind(organization_id.as_str())
    .bind(normalized.as_str())
    .bind(pod_id.map(|p| p.0))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

// One atomic `UPDATE` — see `.claude/contracts/amk-store-inbox-update.md`'s "merge trap" section
// for why `||` alone, and the naive `(COALESCE(metadata,'{}') || $adds) - $dels::text[]` form,
// are both wrong. The two nested `CASE`s below are the guarded expression from that contract,
// verified against the dev database:
//   NULL          + {}       - {}     => NULL     (no-op stays NULL, never {})
//   NULL          + {}       - {x}    => NULL     (deleting from nothing is nothing)
//   NULL          + {"a":1}  - {}     => {"a": 1}
//   {"a":1}       + {}       - {}     => {"a": 1}  (untouched)
//   {"a":1,"b":2} + {"c":3}  - {a}    => {"b": 2, "c": 3}
const UPDATE_SQL: &str = "UPDATE inboxes SET \
    display_name = CASE WHEN $4 THEN $5 ELSE display_name END, \
    metadata = CASE \
        WHEN $6 THEN NULL \
        WHEN $7 THEN \
            CASE WHEN metadata IS NULL AND $8 = '{}'::jsonb THEN NULL \
                 ELSE (COALESCE(metadata, '{}'::jsonb) || $8) - $9::text[] \
            END \
        ELSE metadata \
    END, \
    updated_at = CASE WHEN $10 THEN now() ELSE updated_at END \
    WHERE organization_id = $1 AND inbox_id = $2 AND ($3::uuid IS NULL OR pod_id = $3) \
    RETURNING inbox_id, organization_id, pod_id, client_id, display_name, metadata, created_at, updated_at";

/// Merge [`UpdateInboxRequest::metadata`] into the stored value. `[SPEC:openapi]
/// type_inboxes:UpdateInboxRequest`, verbatim: keys included are added or overwritten, keys
/// omitted are left unchanged, a key mapped to `null` is removed, and `metadata: null` clears
/// everything.
///
/// This is a **lookup**, exactly like [`get`]/[`delete`]: a NUL-bearing `inbox_id` and a scope
/// miss (wrong organization or wrong pod) both mask as `Ok(None)`, never an error. `display_name`
/// and `metadata` are different — this is an update, not a lookup, so there is no not-found to
/// mask into, and a NUL byte in either is rejected with a typed [`StoreError::InvalidValue`]
/// rather than silently stripped or left to fail at parameter encoding.
///
/// Whether "sending an empty object is rejected" or "at least one of `display_name`/`metadata`
/// must be present" holds is **not this function's job** — those are wire-validation rules that
/// produce `amk-http`'s `validation_error` envelope, and this crate has no business constructing
/// one. Here, `Merge(empty)` is a no-op on metadata, and a fully-empty request is a no-op that
/// still returns the current row.
pub async fn update(
    pool: &PgPool,
    organization_id: &OrganizationId,
    pod_id: Option<PodId>,
    inbox_id: &InboxId,
    req: UpdateInboxRequest,
) -> Result<Option<Inbox>, StoreError> {
    // See `get`: a NUL-bearing lookup id can never name a real row, so this masks as not-found —
    // never `StoreError::Database`, and never a side channel between "malformed" and "absent".
    if has_forbidden_byte(inbox_id.as_str()) {
        return Ok(None);
    }
    if req.display_name.as_deref().is_some_and(has_forbidden_byte) {
        return Err(StoreError::InvalidValue("display_name"));
    }

    // Split `Merge`'s map into `adds` (keys with a value: concatenated in) and `dels` (keys
    // mapped to null: removed) — the wire's per-key `null` means delete, which plain `||` does
    // not implement (it would store a JSON null instead of removing the key).
    let (is_clear, is_merge, adds, dels): (bool, bool, Metadata, Vec<String>) = match &req.metadata
    {
        MetadataUpdate::Unchanged => (false, false, Metadata::new(), Vec::new()),
        MetadataUpdate::Clear => (true, false, Metadata::new(), Vec::new()),
        MetadataUpdate::Merge(m) => {
            if merge_map_has_forbidden_byte(m) {
                return Err(StoreError::InvalidValue("metadata"));
            }
            let mut adds = Metadata::new();
            let mut dels = Vec::new();
            for (k, v) in m {
                match v {
                    Some(value) => {
                        adds.insert(k.clone(), value.clone());
                    }
                    None => dels.push(k.clone()),
                }
            }
            (false, true, adds, dels)
        }
    };

    // "Changed" means a field was *present*, not that its value differs from what is stored — a
    // resent, byte-identical `display_name` still bumps `updated_at`. The one exception is a
    // `Merge` whose map is entirely empty (no adds, no dels): that nets to nothing, and a no-op
    // update must not bump `updated_at` — it is on the wire and a client polling it would see a
    // phantom change. A `Clear` always bumps: it is an explicit action, present on the wire,
    // exactly like `display_name`.
    let has_display_name = req.display_name.is_some();
    let merge_nets_to_nothing = is_merge && adds.is_empty() && dels.is_empty();
    let bump = has_display_name || is_clear || (is_merge && !merge_nets_to_nothing);

    let normalized = inbox_id.normalized();
    let row = sqlx::query(UPDATE_SQL)
        .bind(organization_id.as_str())
        .bind(normalized.as_str())
        .bind(pod_id.map(|p| p.0))
        .bind(has_display_name)
        .bind(&req.display_name)
        .bind(is_clear)
        .bind(is_merge)
        .bind(Json(&adds))
        .bind(&dels)
        .bind(bump)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_inbox).transpose()
}
