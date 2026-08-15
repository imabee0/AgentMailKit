//! API keys: minting, argon2id verification, and the three-mount (organization/pod/inbox)
//! repository surface.
//!
//! # Secret format `[ASSUMED]`
//!
//! No fixture shows a real AgentMail API key — `amk_types::api_key::ApiKey`'s own doc comment
//! records the whole resource as `[SPEC:openapi]` only, and no live capture exists. The one piece
//! of evidence this project has ever seen is `reference/fixtures/05-error-catalog.http:6`, where
//! `am_us_00000000000000000000000000000000` (`am_us_` + 32 characters) drew the bare 403 reserved
//! for a *well-formed but unknown* key rather than the malformed-credential response — so the
//! reference API's keys carry a region segment, and this crate reproduces that observed shape
//! exactly: **`am_us_` followed by 32 characters of URL-safe (alphanumeric) CSPRNG output**,
//! `[PREFIX_TAG]` + `[SECRET_LEN]` below. A minted key never begins `am_eu_` — trivially true of a
//! constant `am_us_` prefix, and asserted anyway (`a_minted_key_never_begins_am_eu`) so that
//! changing the tag later cannot silently break it; the `am_eu_`/EU-routing note in the dispatch
//! contract is downgraded to `[UNVERIFIED]` there (its source is not vendored under
//! `reference/`), but never minting it is fail-closed and costs nothing regardless.
//!
//! `prefix` (the wire field, and this table's O(1) lookup column) is the region tag plus the
//! first [`VISIBLE_LEN`] characters of that random portion — `[ASSUMED]` split, chosen because the
//! alternative, a constant `"am_us_"` for every key, cannot be `UNIQUE` (the dispatch contract's
//! own requirement for the lookup index) and storing the *entire* 32 characters in clear would
//! leave nothing secret. Splitting off a short slice for O(1) lookup while hashing the full
//! presented string is the standard shape for this kind of credential (Stripe, GitHub PATs); the
//! exact split point has no evidence behind it, so 8 characters is a plain engineering choice
//! (48 bits of visible entropy is a comfortable margin below any lookup-index concern, and 24
//! characters — 144 bits — stay behind the argon2id hash).
//!
//! # Timing (`authenticate`)
//!
//! [`authenticate`] must not let a caller distinguish "no key starts with this prefix" from
//! "a key starts with this prefix but the secret is wrong" — `reference/fixtures/05-error-catalog.http`
//! shows both cases behind the identical bare `403 {"message":"Forbidden"}`, and a timing gap
//! would reopen that distinction at the network layer. There is **one** call to `verify_secret` in
//! the whole function, unconditional, on a hash a pure fallback selects: the row's own hash on a
//! hit, a fixed dummy hash (computed once at first use) on every kind of miss — unknown prefix,
//! malformed presented value, or a resolved row whose secret turns out to be wrong. Structuring it
//! as one call site rather than a branch that decides whether to call it at all means "skip the
//! verify on a miss" cannot be written without deleting the only place `verify_secret` is called,
//! which `authenticate_with_the_right_secret_resolves_the_key` already kills — the timing
//! invariant does not depend on a reviewer noticing a second call site was left out.
//!
//! # What this module does not do
//!
//! [`authenticate`] never writes — no `used_at` update on the read path, which is a different
//! design (an auth hot path that writes on every request) from the one the dispatch contract
//! chose. [`touch_used_at`] is separate and it is the caller's (amk-http's) decision when to call
//! it. The hash never leaves this module: [`ApiKey`] (the read/list wire type) and
//! [`AuthenticatedKey`] (this crate's own internal read model for the auth path) both omit it.

use std::sync::OnceLock;

use amk_types::api_key::{ApiKey, ApiKeyPermissions, CreateApiKeyResponse};
use amk_types::ids::{ApiKeyId, InboxId, OrganizationId, PodId};
use amk_types::Timestamp;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand::distributions::Alphanumeric;
use rand::Rng;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::StoreError;

/// The region segment. `[ASSUMED]` — see the module doc.
const PREFIX_TAG: &str = "am_us_";
/// Total length of the random portion of a minted secret (after [`PREFIX_TAG`]).
const SECRET_LEN: usize = 32;
/// How many characters of the random portion are echoed into the stored, displayable `prefix`.
/// `[ASSUMED]` — see the module doc.
const VISIBLE_LEN: usize = 8;

/// Which of the three mounts (`/v0/api-keys`, `/v0/pods/{pod_id}/api-keys`,
/// `/v0/inboxes/{inbox_id}/api-keys`) a `get`/`list`/`delete` call is scoped to.
///
/// Not `amk_core::scope::Mount`: that type names the mount an *incoming request* arrived on,
/// which is a different concept from "which stored scope column a key query pins", and reusing it
/// here would borrow a shape for a purpose its own doc comment does not describe. This is amk-store's
/// own read-path parameter, in the same category as [`crate::messages::ListMessagesQuery`] — not
/// a wire shape.
///
/// It exists because `pod_id` and `inbox_id` are mutually exclusive on one row (the migration's
/// `CHECK`), unlike `messages`/`threads` where both are always populated together. A single
/// "pin whichever of two optional columns is `Some`" `WHERE` clause — the pattern
/// [`crate::inboxes::list`] uses — cannot express "match a key scoped to this inbox", because an
/// inbox-scoped row's own `pod_id` column is always `NULL`: pinning `pod_id = $inbox's_pod` would
/// exclude every inbox-scoped row outright. [`KeyScope::Inbox`] pins `inbox_id` alone and leaves
/// `pod_id` unpinned, sidestepping the mismatch entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyScope {
    /// The organization mount. Lists/reads/deletes any key in the organization, whatever its own
    /// scope — the same "no filter beyond the tenant pin" behaviour
    /// [`crate::inboxes::list`] has when its own `pod_id` parameter is `None`.
    Organization,
    /// The pod mount: only keys whose own `pod_id` column names this pod.
    Pod(PodId),
    /// The inbox mount: only keys whose own `inbox_id` column names this inbox (case-folded).
    Inbox(InboxId),
}

fn scope_params(scope: &KeyScope) -> (Option<Uuid>, Option<String>) {
    match scope {
        KeyScope::Organization => (None, None),
        KeyScope::Pod(p) => (Some(p.0), None),
        KeyScope::Inbox(i) => (None, Some(i.normalized().as_str().to_owned())),
    }
}

/// The settable subset of a new key, mirroring [`crate::pods::NewPod`]/[`crate::inboxes::NewInbox`]'s
/// role for their own resources.
///
/// No `api_key_id` field: unlike `CreatePodRequest`/`CreateInboxRequest`,
/// `amk_types::api_key::CreateApiKeyRequest` carries no `client_id` — API-key creation is not
/// idempotent on the wire — so there is no externally significant id a caller must mint upfront.
/// [`create`] mints the id itself, in the same place it already mints the secret, the `prefix`
/// and the hash.
pub struct NewApiKey {
    pub organization_id: OrganizationId,
    /// Exactly one of `pod_id`/`inbox_id` may be `Some`, or both `None` for an organization-scoped
    /// key — enforced by the migration's `CHECK`, not merely here.
    pub pod_id: Option<PodId>,
    pub inbox_id: Option<InboxId>,
    pub name: String,
    /// `None` grants everything; `Some(ApiKeyPermissions::default())` grants nothing — the
    /// NULL-vs-`{}` distinction `amk_types::api_key::KeyGrants::from_wire` owns. Passed straight
    /// through; this module never restates that semantics.
    pub permissions: Option<ApiKeyPermissions>,
}

/// What [`authenticate`] resolves a presented secret to.
///
/// Not [`ApiKey`]: that wire type deliberately omits `organization_id` (no artifact shows it on
/// the public resource — see its own doc comment), but the auth layer needs it to build
/// `Identity` (`organization_id` is required there per `openapi.json` `type_auth:Identity`). This
/// is amk-store's own internal read model for that one call, in the same category as [`NewApiKey`]
/// on the write side — never serialized to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedKey {
    pub api_key_id: ApiKeyId,
    pub organization_id: OrganizationId,
    pub pod_id: Option<PodId>,
    pub inbox_id: Option<InboxId>,
    pub permissions: Option<ApiKeyPermissions>,
}

// ---- minting -------------------------------------------------------------------------------

/// `SECRET_LEN` alphanumeric characters from a CSPRNG. `rand::rngs::OsRng` draws directly from
/// the OS's own randomness source on every call — not a general-purpose (non-cryptographic) RNG,
/// and not a PRNG merely *seeded* from one.
fn random_secret_chars() -> String {
    rand::rngs::OsRng
        .sample_iter(Alphanumeric)
        .take(SECRET_LEN)
        .map(char::from)
        .collect()
}

/// Mint one new secret. Returns `(full secret to show the caller exactly once, prefix to store)`.
fn mint() -> (String, String) {
    let random = random_secret_chars();
    let secret = format!("{PREFIX_TAG}{random}");
    let visible = random
        .get(..VISIBLE_LEN)
        .expect("invariant: SECRET_LEN (32) is always >= VISIBLE_LEN (8)");
    let prefix = format!("{PREFIX_TAG}{visible}");
    (secret, prefix)
}

/// The stored `prefix` a presented value would look up under, or `None` when the value is too
/// short or does not carry [`PREFIX_TAG`] at all — a caller-controlled string, so this must never
/// panic, including on a value that is not valid UTF-8 at the slice boundary it wants
/// (`str::get`, not indexing, is what makes that safe).
fn candidate_prefix(presented: &str) -> Option<String> {
    let rest = presented.strip_prefix(PREFIX_TAG)?;
    let visible = rest.get(..VISIBLE_LEN)?;
    Some(format!("{PREFIX_TAG}{visible}"))
}

// ---- hashing ---------------------------------------------------------------------------------

fn hash_secret(secret: &str) -> String {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .expect(
            "invariant: hashing a freshly generated salt with argon2's default params cannot \
             fail — the only failure modes are a salt/params the library itself rejects, and \
             SaltString::generate always produces one it accepts",
        )
        .to_string()
}

fn verify_secret(secret: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A fixed hash to verify against when no row was found, so a prefix miss costs the same one
/// argon2id computation as a hit. Computed once per process (its own cost must not vary between
/// calls either) against a constant, never-secret plaintext — nothing verifies successfully
/// against it, and nothing needs to: its result is always discarded.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_secret("amk-store-timing-parity-dummy-never-a-real-secret"))
}

// ---- row <-> type ------------------------------------------------------------------------------

fn row_permissions(row: &PgRow) -> Result<Option<ApiKeyPermissions>, StoreError> {
    let permissions: Option<Json<ApiKeyPermissions>> = row.try_get("permissions")?;
    Ok(permissions.map(|Json(p)| p))
}

fn row_to_api_key(row: &PgRow) -> Result<ApiKey, StoreError> {
    Ok(ApiKey {
        api_key_id: ApiKeyId::new(row.try_get::<Uuid, _>("api_key_id")?.to_string()),
        prefix: row.try_get("prefix")?,
        name: row.try_get("name")?,
        pod_id: row.try_get::<Option<Uuid>, _>("pod_id")?.map(PodId::from),
        inbox_id: row
            .try_get::<Option<String>, _>("inbox_id")?
            .map(InboxId::new),
        used_at: row
            .try_get::<Option<DateTime<Utc>>, _>("used_at")?
            .map(Timestamp::from),
        permissions: row_permissions(row)?,
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

fn row_to_authenticated(row: &PgRow) -> Result<AuthenticatedKey, StoreError> {
    Ok(AuthenticatedKey {
        api_key_id: ApiKeyId::new(row.try_get::<Uuid, _>("api_key_id")?.to_string()),
        organization_id: OrganizationId::new(row.try_get::<String, _>("organization_id")?),
        pod_id: row.try_get::<Option<Uuid>, _>("pod_id")?.map(PodId::from),
        inbox_id: row
            .try_get::<Option<String>, _>("inbox_id")?
            .map(InboxId::new),
        permissions: row_permissions(row)?,
    })
}

// ---- repository ------------------------------------------------------------------------------
//
// `api_key_id` is stored as a `uuid` column (the migration's own decision) while [`ApiKeyId`] is
// an opaque string newtype (`amk_types::ids`, frozen — not every id type there is UUID-typed), so
// a caller-supplied id first has to be checked against the column's real type somehow. An earlier
// version of this module did that in Rust — `Uuid::parse_str(id.as_str()).ok()`, with a `None`
// arm returning "not found" before any query ran — and a review lens found that structure
// mutable into a live bug: replacing the `None` arm with `.unwrap_or_else(Uuid::nil)` (continue
// with a placeholder instead of returning early) made a malformed id resolve any row a
// `Uuid::nil()` had been seeded into. Nothing seeds that value today, but "nothing currently
// seeds it" is not the same as "this cannot happen" — the vulnerability was the existence of a
// Rust-side branch that a mutation could rewrite to skip the early return, not the specific
// value it happened to fall back to.
//
// [`get`], [`delete`] and [`touch_used_at`] below have no such branch at all: `api_key_id` is
// bound as the raw presented `&str` and compared with `api_key_id::text = lower($n)` in SQL.
// There is no Rust code path between "a caller-supplied string" and "a query parameter" for that
// mutation to rewrite — a malformed id is simply a string that cannot equal any row's canonical
// lowercase UUID text, which Postgres itself answers with zero rows, never an error. `lower()`
// on the parameter (not the column, which `::text` already renders in canonical lowercase)
// preserves case-insensitive matching for a validly-cased-but-differently-cased id, exactly as a
// parse-then-compare-by-value would have. The one cost is that `api_key_id::text = $n` cannot use
// the primary key's native `uuid` index for a true index-only scan the way `api_key_id = $n::uuid`
// would; accepted here because `get`/`delete`/`touch_used_at` are single-row, administrative-rate
// calls, never the request-per-second `authenticate` path (which looks up by `prefix`, unaffected).

// One literal per query, matching `messages.rs`'s idiom — sqlx 0.9's `SqlSafeStr` bound accepts
// only `&'static str`, so the column list is duplicated across these rather than built with
// `format!` (which would have to audit the composed string for injection on every call for no
// reason: nothing here is ever runtime-composed from unpinned data).
const INSERT_SQL: &str = "INSERT INTO api_keys \
    (api_key_id, organization_id, pod_id, inbox_id, name, prefix, hash, permissions) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
     RETURNING api_key_id, prefix, name, pod_id, inbox_id, permissions, used_at, created_at";

const GET_SQL: &str = "SELECT api_key_id, prefix, name, pod_id, inbox_id, permissions, used_at, \
        created_at \
     FROM api_keys \
     WHERE organization_id = $1 AND api_key_id::text = lower($2) \
       AND ($3::uuid IS NULL OR pod_id = $3) \
       AND ($4::text IS NULL OR inbox_id = $4)";

const LIST_SQL: &str = "SELECT api_key_id, prefix, name, pod_id, inbox_id, permissions, used_at, \
        created_at \
     FROM api_keys \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
     ORDER BY created_at ASC, api_key_id ASC";

const DELETE_SQL: &str = "DELETE FROM api_keys \
     WHERE organization_id = $1 AND api_key_id::text = lower($2) \
       AND ($3::uuid IS NULL OR pod_id = $3) \
       AND ($4::text IS NULL OR inbox_id = $4)";

const AUTHENTICATE_SQL: &str = "SELECT api_key_id, organization_id, pod_id, inbox_id, \
        permissions, hash \
     FROM api_keys WHERE prefix = $1";

/// Mint a new key and store it. Returns the one response that carries the plaintext secret —
/// [`CreateApiKeyResponse`] — which the caller must hand back to its own caller and never store.
pub async fn create(pool: &PgPool, new: NewApiKey) -> Result<CreateApiKeyResponse, StoreError> {
    let api_key_id = Uuid::new_v4();
    let (secret, prefix) = mint();
    let hash = hash_secret(&secret);
    let inbox_id = new.inbox_id.as_ref().map(|i| i.normalized());

    let row = sqlx::query(INSERT_SQL)
        .bind(api_key_id)
        .bind(new.organization_id.as_str())
        .bind(new.pod_id.map(|p| p.0))
        .bind(inbox_id.as_ref().map(InboxId::as_str))
        .bind(&new.name)
        .bind(&prefix)
        .bind(&hash)
        .bind(new.permissions.as_ref().map(Json))
        .fetch_one(pool)
        .await?;

    Ok(CreateApiKeyResponse {
        api_key_id: ApiKeyId::new(row.try_get::<Uuid, _>("api_key_id")?.to_string()),
        api_key: secret,
        prefix: row.try_get("prefix")?,
        name: row.try_get("name")?,
        pod_id: row.try_get::<Option<Uuid>, _>("pod_id")?.map(PodId::from),
        inbox_id: row
            .try_get::<Option<String>, _>("inbox_id")?
            .map(InboxId::new),
        permissions: row_permissions(&row)?,
        created_at: Timestamp::from(row.try_get::<DateTime<Utc>, _>("created_at")?),
    })
}

/// Fetch one key by id, pinned to the mount's own scope. A key that exists but is scoped
/// differently from `scope` (e.g. a pod-scoped key looked up through the inbox mount) is
/// indistinguishable from an absent one — `None` either way, for the caller to mask uniformly.
pub async fn get(
    pool: &PgPool,
    organization_id: &OrganizationId,
    scope: &KeyScope,
    api_key_id: &ApiKeyId,
) -> Result<Option<ApiKey>, StoreError> {
    let (pod_param, inbox_param) = scope_params(scope);
    let row = sqlx::query(GET_SQL)
        .bind(organization_id.as_str())
        .bind(api_key_id.as_str())
        .bind(pod_param)
        .bind(inbox_param)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_api_key).transpose()
}

/// List keys visible at `scope`. See [`KeyScope`]'s own doc for what each mount returns.
pub async fn list(
    pool: &PgPool,
    organization_id: &OrganizationId,
    scope: &KeyScope,
) -> Result<Vec<ApiKey>, StoreError> {
    let (pod_param, inbox_param) = scope_params(scope);
    let rows = sqlx::query(LIST_SQL)
        .bind(organization_id.as_str())
        .bind(pod_param)
        .bind(inbox_param)
        .fetch_all(pool)
        .await?;
    rows.iter().map(row_to_api_key).collect()
}

/// Delete one key, pinned to the mount's own scope exactly as [`get`] is.
pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    scope: &KeyScope,
    api_key_id: &ApiKeyId,
) -> Result<bool, StoreError> {
    let (pod_param, inbox_param) = scope_params(scope);
    let result = sqlx::query(DELETE_SQL)
        .bind(organization_id.as_str())
        .bind(api_key_id.as_str())
        .bind(pod_param)
        .bind(inbox_param)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Resolve a presented secret to the key it belongs to, or `None` for every kind of miss
/// (unknown prefix, wrong secret, or a malformed presented value) — see the module doc for why
/// those must cost the same. Never writes; see [`touch_used_at`].
pub async fn authenticate(
    pool: &PgPool,
    presented: &str,
) -> Result<Option<AuthenticatedKey>, StoreError> {
    let row = match candidate_prefix(presented) {
        Some(prefix) => {
            sqlx::query(AUTHENTICATE_SQL)
                .bind(prefix)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };

    // Exactly one argon2id verify, on every path, against a hash chosen by a pure fallback:
    // the row's own hash on a hit, the fixed dummy hash on every kind of miss. This is
    // deliberately the ONLY call to `verify_secret` in this function — there is no second call
    // site to skip and no branch that can bypass this one, so "verify only on a hit" is not
    // merely untested, it is unwritable without deleting this line, which
    // `authenticate_with_the_right_secret_resolves_the_key` already kills.
    let stored_hash: Option<String> = row.as_ref().map(|r| r.try_get("hash")).transpose()?;
    let hash_to_check = stored_hash.as_deref().unwrap_or(dummy_hash());
    let matched = verify_secret(presented, hash_to_check);

    match (row, matched) {
        (Some(row), true) => Ok(Some(row_to_authenticated(&row)?)),
        _ => Ok(None),
    }
}

/// Record that a key was used, independent of [`authenticate`] — see the module doc for why the
/// two are separate calls. Returns whether a row was found and updated.
pub async fn touch_used_at(pool: &PgPool, api_key_id: &ApiKeyId) -> Result<bool, StoreError> {
    let result =
        sqlx::query("UPDATE api_keys SET used_at = now() WHERE api_key_id::text = lower($1)")
            .bind(api_key_id.as_str())
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_key_matches_the_observed_shape() {
        let (secret, prefix) = mint();
        assert!(secret.starts_with(PREFIX_TAG));
        assert_eq!(secret.len(), PREFIX_TAG.len() + SECRET_LEN);
        assert!(secret[PREFIX_TAG.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric()));
        assert!(prefix.starts_with(PREFIX_TAG));
        assert_eq!(prefix.len(), PREFIX_TAG.len() + VISIBLE_LEN);
        assert!(
            secret.starts_with(&prefix),
            "prefix must be a true leading segment of the secret"
        );
    }

    #[test]
    fn a_minted_key_never_begins_am_eu() {
        // [UNVERIFIED] in the dispatch contract that the SDK routes am_eu_ to the EU host — not
        // checkable from this repository — but never minting it is fail-closed regardless, and
        // cheap to assert so a later change to PREFIX_TAG cannot silently reintroduce it.
        for _ in 0..50 {
            let (secret, _) = mint();
            assert!(!secret.starts_with("am_eu_"));
        }
    }

    #[test]
    fn two_successive_mints_differ() {
        let (a, _) = mint();
        let (b, _) = mint();
        assert_ne!(a, b, "two mints must not collide");
    }

    #[test]
    fn hash_and_verify_round_trip() {
        let hash = hash_secret("am_us_the-real-secret");
        assert!(verify_secret("am_us_the-real-secret", &hash));
        assert!(!verify_secret("am_us_the-wrong-secret", &hash));
        // The stored hash never contains the secret itself, in any form.
        assert!(!hash.contains("the-real-secret"));
    }

    #[test]
    fn candidate_prefix_never_panics_on_hostile_input() {
        // Every one of these must return without panicking, whatever it returns.
        for input in [
            "",
            "am_us_",
            "am_us_ab",
            "hello",
            "am_eu_00000000000000000000000000000000",
            "am_us_\u{1F600}\u{1F600}", // multi-byte characters straddling the VISIBLE_LEN cut
            "am_us_1234567",            // one short of VISIBLE_LEN
        ] {
            let _ = candidate_prefix(input);
        }
        assert_eq!(candidate_prefix("am_us_1234567"), None, "too short to have a prefix at all");
        assert_eq!(
            candidate_prefix("am_us_12345678rest-of-the-secret"),
            Some("am_us_12345678".to_owned())
        );
    }
}
