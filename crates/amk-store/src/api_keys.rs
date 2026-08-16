//! API keys: minting, argon2id verification, and the three-mount (organization/pod/inbox)
//! repository surface.
//!
//! # Secret format
//!
//! `reference/fixtures/23-inbox-defaults-and-key-shape.txt` captured a real minted key: `am_us_`
//! followed by **64 lowercase-hex characters**, whose `prefix` (the wire field, and this table's
//! O(1) lookup column) is `am_us_` plus the first **6** of them. This supersedes the earlier
//! `[ASSUMED]` 32-character URL-safe-alphanumeric shape, whose only evidence was
//! `reference/fixtures/05-error-catalog.http:6` — a *rejected*, synthetic
//! `am_us_00000000000000000000000000000000`, which showed only what the gateway accepts as
//! well-formed, never what it mints. [`PREFIX_TAG`] + [`SECRET_LEN`] below now match fixture 23
//! exactly. A minted key never begins `am_eu_` — trivially true of a constant `am_us_` prefix,
//! and asserted anyway (`a_minted_key_never_begins_am_eu`) so that changing the tag later cannot
//! silently break it. That assertion rests on two claims of different strength, and the
//! distinction is worth keeping: the EU host **exists** — `reference/openapi.json`'s `servers`
//! array carries `{"url":"https://api.agentmail.eu","description":"eu-prod"}`, vendored and
//! hash-pinned, so `[SPEC:openapi]`. That an `am_eu_`-prefixed key **routes** a client there is
//! `[UNVERIFIED]`: the plan cites the node SDK (`environments.ts`, `Client.ts:80`), read from a
//! clone that was never vendored under `reference/`, so nothing in this repository can re-check
//! it. Never minting the prefix is fail-closed under either reading and costs nothing, which is
//! why the gap is recorded rather than closed.
//!
//! The random portion is 32 bytes of `rand::rngs::OsRng` output, hex-encoded lowercase by hand
//! (`write!(s, "{b:02x}")`) rather than through a dependency — 256 bits exactly, with no
//! modulo-bias question to answer for a fixed byte-to-two-hex-digits mapping. [`VISIBLE_LEN`]
//! (6, per fixture 23) is short enough that a mint can now collide in `api_keys_prefix_idx`
//! (`UNIQUE`) at realistic key counts — [`MINT_ATTEMPTS`] below redraws on that one constraint.
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

use std::fmt::Write as _;
use std::sync::OnceLock;

use amk_types::api_key::{ApiKey, ApiKeyPermissions, CreateApiKeyResponse};
use amk_types::ids::{has_forbidden_byte, ApiKeyId, InboxId, OrganizationId, PodId};
use amk_types::Timestamp;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand::Rng;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::{PageTokenError, StoreError};
use crate::pagination::{ApiKeyCursor, Page, SortDirection};

/// The region segment. `[ASSUMED]` — see the module doc.
const PREFIX_TAG: &str = "am_us_";
/// Total length of the random portion of a minted secret (after [`PREFIX_TAG`]): 64 lowercase-hex
/// characters, the hex encoding of 32 CSPRNG bytes — observed, fixture 23.
const SECRET_LEN: usize = 64;
/// How many characters of the random portion are echoed into the stored, displayable `prefix` —
/// observed, fixture 23.
const VISIBLE_LEN: usize = 6;
/// How many times [`create`] redraws a secret after a `prefix` collision on `api_keys_prefix_idx`
/// before giving up and surfacing the underlying database error unmapped — see the module doc.
///
/// **Accepted, named gap: this constant's exact value is not pinned by a test.** Lowering it to
/// `1` passes the whole suite; only `0` is caught, and incidentally (the loop never runs, so the
/// `expect` below fires on essentially every `create`). Forcing a real collision through `create`
/// would need an injectable RNG seam, and the failure mode that seam would guard is "redraws once
/// instead of four times" at a per-mint collision probability with thirteen zeros after the
/// decimal point. Recorded here, beside the constant, rather than fixed — a reader changing this
/// number is the person who needs to know no test will stop them.
const MINT_ATTEMPTS: u32 = 4;

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

/// 32 CSPRNG bytes, hex-encoded lowercase by hand (`SECRET_LEN` hex characters). `rand::rngs::OsRng`
/// draws directly from the OS's own randomness source on every call — not a general-purpose
/// (non-cryptographic) RNG, and not a PRNG merely *seeded* from one. Written with `write!`, not a
/// hex-encoding dependency: four characters of format string is not worth a new crate.
fn random_secret_chars() -> String {
    let bytes: [u8; 32] = rand::rngs::OsRng.gen();
    let mut s = String::with_capacity(SECRET_LEN);
    for b in bytes {
        write!(s, "{b:02x}").expect("invariant: writing to a String never fails");
    }
    s
}

/// Mint one new secret. Returns `(full secret to show the caller exactly once, prefix to store)`.
fn mint() -> (String, String) {
    let random = random_secret_chars();
    let secret = format!("{PREFIX_TAG}{random}");
    let visible = random
        .get(..VISIBLE_LEN)
        .expect("invariant: SECRET_LEN (64) is always >= VISIBLE_LEN (6)");
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
        organization_id: Some(OrganizationId::new(row.try_get::<String, _>("organization_id")?)),
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
// a caller-supplied id first has to be checked against the column's real type somehow.
//
// Three earlier versions of [`get`]/[`delete`]/[`touch_used_at`] got this wrong, each fixing the
// previous one's defect while introducing its own:
//
// * The first parsed in Rust with a `let Some(id) = ... else { return Ok(None) }` early return. A
//   review lens found that structure mutable into a live bug: replacing the `None` arm with
//   `.unwrap_or_else(Uuid::nil)` (continue with a placeholder instead of returning early) made a
//   malformed id resolve any row seeded at the nil UUID. Nothing seeds that value today, but
//   "nothing currently seeds it" is not "this cannot happen" — the vulnerability was the
//   *existence* of a Rust-side branch a mutation could rewrite to skip the early return, not the
//   specific fallback value.
// * The second removed the branch by not parsing at all: bind the raw string and compare
//   `api_key_id::text = lower($n)` in SQL, so a malformed id is simply text that cannot equal any
//   row's canonical UUID rendering. That closed the *comparison* but not the *encoding*: Postgres
//   `text` cannot carry an embedded NUL byte at all, and `ApiKeyId::from_path_segment`
//   (`crates/amk-types/src/ids.rs`) rejects only invalid UTF-8 — `%00` percent-decodes to a
//   perfectly valid UTF-8 string that Postgres then refuses to encode as a bind parameter
//   (`22021 invalid byte sequence for encoding "UTF8"`), surfacing as `StoreError::Database`
//   instead of the uniform "not found" every other malformed id gets. Moving a check to the
//   database boundary only closes it uniformly if that boundary is itself uniformly closed; `text`
//   parameter *encoding* is not, so the check has to happen before a value ever becomes a bind
//   parameter, not at the database.
// * The third bound a parsed `Uuid` — `Uuid::parse_str(api_key_id.as_str()).ok()` as `Option<Uuid>`
//   — which fixes the encoding problem (parsing happens in Rust, so a NUL byte never reaches a
//   query parameter; failure is the *value* `None`, which binds as SQL `NULL` and matches zero
//   rows without erroring, not a branch a mutation can redirect) and restores the primary key's
//   native index. But `Uuid::parse_str` treats five renderings of one value as equal — canonical,
//   uppercase-hyphenated, simple-32, braced, `urn:uuid:` — and [`ApiKeyId`] is `string_id!`
//   (`amk_types::ids`), not `uuid_id!`: opaque, byte-exact `PartialEq`, no `normalized()`. Unlike
//   [`PodId`]/[`ThreadId`] (genuinely `uuid_id!`) or [`InboxId`] (case-folds only because fixture
//   18 proved AgentMail does), nothing says `ApiKeyId` accepts an alternate rendering of a UUID it
//   issued — `reference/types_dump.txt:29` types it as a bare `str`. Binding the parsed value alone
//   collapsed those five renderings into one equivalence class this crate invented, which is a
//   *wider* unevidenced surface than the `lower()` case-folding the second version was fixed to
//   remove, not a narrower one.
//
// The fix keeps the `Uuid` bind (for the index and the total NUL-safe parse) and adds one filter:
// require the value's own canonical rendering to equal what was presented, so parsing is used only
// to validate-and-normalize-for-comparison, never to accept an alternate rendering as equivalent.
// `Uuid`'s `Display` is canonical lowercase-hyphenated, and every `ApiKeyId` this crate ever hands
// a caller is built from that same rendering — [`create`] binds the raw `Uuid` into the `uuid`
// column natively (never as text), and only calls `Uuid::to_string()` once, separately, to build
// the `api_key_id` field of the `CreateApiKeyResponse` it returns; [`row_to_api_key`] and
// [`row_to_authenticated`] do the same on every later read. So the id this crate issued always
// still resolves; every other rendering — differently cased, unhyphenated, braced,
// `urn:`-prefixed — becomes `None` just as a NUL byte or any other unparseable string does.
// `.filter(..)` keeps this total: there is still no branch, and deleting it at any one of the
// three call sites (`get`, `delete`, `touch_used_at`) is independently caught by
// `tests/api_keys.rs`'s `only_the_canonical_rendering_of_an_api_key_id_resolves_it_everywhere`.

/// Parse `id` to a `Uuid` for binding into `api_key_id = $n`, but only when the value's own
/// canonical (lowercase-hyphenated) rendering is byte-identical to what was presented — see the
/// doc comment above this section for why. Covers every other case as one uniform `None`:
/// unparseable text (including a NUL byte, which this way never reaches a query parameter at
/// all), and parseable-but-differently-rendered text (uppercase, unhyphenated, braced, `urn:`).
///
/// Shared by all three call sites that resolve a caller-presented id — [`get`], [`delete`] and
/// [`touch_used_at`] — precisely so the guarantee is one property in one function rather than
/// three copies that could drift independently. Each call site's own filter must be verified
/// independently anyway (a mutation at one site is invisible to a test that only exercises
/// another), which is what `only_the_canonical_rendering_of_an_api_key_id_resolves_it_everywhere`
/// does.
pub(crate) fn exact_api_key_uuid(id: &ApiKeyId) -> Option<Uuid> {
    let s = id.as_str();
    Uuid::parse_str(s).ok().filter(|u| u.to_string() == s)
}

// One literal per query, matching `messages.rs`'s idiom — sqlx 0.9's `SqlSafeStr` bound accepts
// only `&'static str`, so the column list is duplicated across these rather than built with
// `format!` (which would have to audit the composed string for injection on every call for no
// reason: nothing here is ever runtime-composed from unpinned data).
const INSERT_SQL: &str = "INSERT INTO api_keys \
    (api_key_id, organization_id, pod_id, inbox_id, name, prefix, hash, permissions) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
     RETURNING api_key_id, organization_id, prefix, name, pod_id, inbox_id, permissions, used_at, created_at";

const GET_SQL: &str =
    "SELECT api_key_id, organization_id, prefix, name, pod_id, inbox_id, permissions, used_at, \
        created_at \
     FROM api_keys \
     WHERE organization_id = $1 AND api_key_id = $2 \
       AND ($3::uuid IS NULL OR pod_id = $3) \
       AND ($4::text IS NULL OR inbox_id = $4)";

const LIST_ASC_SQL: &str =
    "SELECT api_key_id, organization_id, prefix, name, pod_id, inbox_id, permissions, used_at, created_at \
     FROM api_keys \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND ($4::timestamptz IS NULL OR (created_at, api_key_id) > ($4, $5)) \
     ORDER BY created_at ASC, api_key_id ASC \
     LIMIT $6";

const LIST_DESC_SQL: &str =
    "SELECT api_key_id, organization_id, prefix, name, pod_id, inbox_id, permissions, used_at, created_at \
     FROM api_keys \
     WHERE organization_id = $1 \
       AND ($2::uuid IS NULL OR pod_id = $2) \
       AND ($3::text IS NULL OR inbox_id = $3) \
       AND ($4::timestamptz IS NULL OR (created_at, api_key_id) < ($4, $5)) \
     ORDER BY created_at DESC, api_key_id DESC \
     LIMIT $6";

const DELETE_SQL: &str = "DELETE FROM api_keys \
     WHERE organization_id = $1 AND api_key_id = $2 \
       AND ($3::uuid IS NULL OR pod_id = $3) \
       AND ($4::text IS NULL OR inbox_id = $4)";

const AUTHENTICATE_SQL: &str = "SELECT api_key_id, organization_id, pod_id, inbox_id, \
        permissions, hash \
     FROM api_keys WHERE prefix = $1";

fn is_prefix_collision(db_err: &dyn sqlx::error::DatabaseError) -> bool {
    db_err.is_unique_violation() && db_err.constraint() == Some("api_keys_prefix_idx")
}

/// Mint a new key and store it. Returns the one response that carries the plaintext secret —
/// [`CreateApiKeyResponse`] — which the caller must hand back to its own caller and never store.
///
/// Retries up to [`MINT_ATTEMPTS`] times on a `prefix` collision (`api_keys_prefix_idx`, matched
/// by constraint name, never a bare `is_unique_violation()` — see the module doc for why 6 visible
/// hex characters makes this reachable at realistic key counts, and `.claude/contracts`'s sibling
/// note on `pods::delete` for why constraint-name matching, not SQLSTATE alone, is this crate's
/// rule for turning a database error into a specific outcome). Any other database error, or a
/// collision on the `MINT_ATTEMPTS`th attempt, propagates unmapped as [`StoreError::Database`].
pub async fn create(pool: &PgPool, new: NewApiKey) -> Result<CreateApiKeyResponse, StoreError> {
    if new
        .inbox_id
        .as_ref()
        .is_some_and(|i| has_forbidden_byte(i.as_str()))
    {
        return Err(StoreError::InvalidValue("inbox_id"));
    }
    // `name` is free-form control-plane text with no P2 owner, bound straight into this
    // `INSERT` — a NUL byte would otherwise fail at parameter encoding (SQLSTATE 22021) rather
    // than reject cleanly. Sibling of the identical guard in `inboxes::create`/`pods::create`.
    if has_forbidden_byte(&new.name) {
        return Err(StoreError::InvalidValue("name"));
    }

    let inbox_id = new.inbox_id.as_ref().map(|i| i.normalized());

    let mut last_collision: Option<sqlx::Error> = None;
    for _ in 0..MINT_ATTEMPTS {
        let api_key_id = Uuid::new_v4();
        let (secret, prefix) = mint();
        let hash = hash_secret(&secret);

        let attempt = sqlx::query(INSERT_SQL)
            .bind(api_key_id)
            .bind(new.organization_id.as_str())
            .bind(new.pod_id.map(|p| p.0))
            .bind(inbox_id.as_ref().map(InboxId::as_str))
            .bind(&new.name)
            .bind(&prefix)
            .bind(&hash)
            .bind(new.permissions.as_ref().map(Json))
            .fetch_one(pool)
            .await;

        let row = match attempt {
            Ok(row) => row,
            Err(sqlx::Error::Database(db_err)) if is_prefix_collision(db_err.as_ref()) => {
                last_collision = Some(sqlx::Error::Database(db_err));
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        return Ok(CreateApiKeyResponse {
            organization_id: Some(OrganizationId::new(
                row.try_get::<String, _>("organization_id")?,
            )),
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
        });
    }
    Err(last_collision
        .expect(
            "invariant: MINT_ATTEMPTS > 0, so the loop only exits without returning after \
                 recording at least one collision",
        )
        .into())
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
    // The guard lives here, before `scope_params`, and deliberately not inside it. `scope_params`
    // turns `KeyScope::Inbox` into a *pin*: `Some(id)` narrows the query to that one inbox, but
    // `None` does not fail closed — it becomes `($3::text IS NULL OR inbox_id = $3)`, which is
    // *unpinned* and matches every inbox in the organization. If a NUL byte were handled by
    // returning `(None, None)` from `scope_params`, a hostile inbox-scoped request would silently
    // widen into an organization-wide one instead of failing — a cross-tenant read, strictly worse
    // than the 500 this dispatch fixes. Rejecting here, before that helper ever runs, is what keeps
    // the failure mode a miss instead of a leak.
    if let KeyScope::Inbox(i) = scope {
        if has_forbidden_byte(i.as_str()) {
            return Ok(None);
        }
    }
    let (pod_param, inbox_param) = scope_params(scope);
    let row = sqlx::query(GET_SQL)
        .bind(organization_id.as_str())
        .bind(exact_api_key_uuid(api_key_id))
        .bind(pod_param)
        .bind(inbox_param)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_api_key).transpose()
}

/// One list request, already resolved to a concrete direction and a decoded (and scope-validated)
/// cursor — same role as [`crate::messages::ListMessagesQuery`]/[`crate::threads::ListThreadsQuery`].
pub struct ListApiKeysQuery {
    pub limit: u64,
    pub direction: SortDirection,
    pub cursor: Option<ApiKeyCursor>,
}

/// List keys visible at `scope`, paginated. See [`KeyScope`]'s own doc for what each mount
/// returns.
pub async fn list(
    pool: &PgPool,
    organization_id: &OrganizationId,
    scope: &KeyScope,
    query: ListApiKeysQuery,
) -> Result<Page<ApiKey>, StoreError> {
    // See the identical guard in `messages::list`/`threads::list`: a zero-row page has no row to
    // anchor a cursor on, so return it directly — and before any query, including the scope guard
    // below, runs.
    if query.limit == 0 {
        return Ok(Page { items: Vec::new(), next: None });
    }
    // See the identical comment in `get`: this must run before `scope_params`, not inside it, or a
    // hostile NUL-bearing inbox scope silently widens to every key in the organization instead of
    // returning none.
    if let KeyScope::Inbox(i) = scope {
        if has_forbidden_byte(i.as_str()) {
            return Ok(Page { items: Vec::new(), next: None });
        }
    }
    // Sibling of the identical guard in `messages::list`/`threads::list`: `ApiKeyCursor`'s fields
    // are `pub` and nothing at the type level guarantees a cursor reaching this function went
    // through `ApiKeyCursor::decode` first. `api_key_id` needs no matching check here — a
    // non-canonical or NUL-bearing value fails `exact_api_key_uuid` below and binds SQL `NULL`
    // rather than reaching parameter encoding as `text`, exactly as `pod_id`'s `Uuid` binding does.
    if let Some(c) = &query.cursor {
        if let Some(inbox) = &c.inbox_id {
            if has_forbidden_byte(inbox.as_str()) {
                return Err(StoreError::InvalidPageToken(PageTokenError::ForbiddenByte(
                    "cursor.inbox_id",
                )));
            }
        }
    }
    let (pod_param, inbox_param) = scope_params(scope);
    let sql = match query.direction {
        SortDirection::Ascending => LIST_ASC_SQL,
        SortDirection::Descending => LIST_DESC_SQL,
    };
    let (cursor_ts, cursor_id) = match &query.cursor {
        Some(c) => (Some(c.created_at), exact_api_key_uuid(&c.api_key_id)),
        None => (None, None),
    };
    // See the identical comment in `messages::list`: `query.limit` is an unclamped `u64`, so
    // `limit: u64::MAX` or `limit: i64::MAX as u64` must not overflow or wrap `fetch_limit`.
    let fetch_limit = query.limit.saturating_add(1).min(i64::MAX as u64) as i64;

    let rows = sqlx::query(sql)
        .bind(organization_id.as_str())
        .bind(pod_param)
        .bind(inbox_param)
        .bind(cursor_ts)
        .bind(cursor_id)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await?;

    let has_more = rows.len() as u64 > query.limit;
    let items: Vec<ApiKey> = rows
        .iter()
        .take(query.limit as usize)
        .map(row_to_api_key)
        .collect::<Result<_, _>>()?;

    let next = if has_more {
        let last = items
            .last()
            .expect("has_more implies at least one item when limit > 0");
        Some(
            ApiKeyCursor {
                created_at: last.created_at.into_inner(),
                api_key_id: last.api_key_id.clone(),
                pod_id: last.pod_id,
                inbox_id: last.inbox_id.clone(),
            }
            .encode(),
        )
    } else {
        None
    };

    Ok(Page { items, next })
}

/// Delete one key, pinned to the mount's own scope exactly as [`get`] is.
pub async fn delete(
    pool: &PgPool,
    organization_id: &OrganizationId,
    scope: &KeyScope,
    api_key_id: &ApiKeyId,
) -> Result<bool, StoreError> {
    // See the identical comment in `get`: this must run before `scope_params`, not inside it, or a
    // hostile NUL-bearing inbox scope silently widens to every key in the organization instead of
    // deleting nothing.
    if let KeyScope::Inbox(i) = scope {
        if has_forbidden_byte(i.as_str()) {
            return Ok(false);
        }
    }
    let (pod_param, inbox_param) = scope_params(scope);
    let result = sqlx::query(DELETE_SQL)
        .bind(organization_id.as_str())
        .bind(exact_api_key_uuid(api_key_id))
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
    // A NUL-bearing `presented` value is treated as the same kind of miss as `candidate_prefix`
    // returning `None` — folded into this match, not a new statement ahead of it, so it shares the
    // unconditional `verify_secret` call below rather than short-circuiting past it. An early
    // `return Ok(None)` here would skip the argon2 verify entirely and resolve measurably faster
    // than a real miss, reopening the exact timing distinction the module doc (and five prior
    // review rounds) require `authenticate` not to have. Postgres `text` cannot encode the byte
    // either way, so the query must not run — but "must not query" and "must not verify" are not
    // the same requirement, and only the first one is true here.
    let row = match candidate_prefix(presented) {
        Some(prefix) if !has_forbidden_byte(&prefix) => {
            sqlx::query(AUTHENTICATE_SQL)
                .bind(prefix)
                .fetch_optional(pool)
                .await?
        }
        _ => None,
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
    let result = sqlx::query("UPDATE api_keys SET used_at = now() WHERE api_key_id = $1")
        .bind(exact_api_key_uuid(api_key_id))
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_key_matches_the_observed_shape() {
        // reference/fixtures/23-inbox-defaults-and-key-shape.txt: `am_us_` + 64 lowercase-hex
        // characters, `prefix` = `am_us_` + the first 6 of them. Checking `[0-9a-f]` specifically
        // — not merely `is_ascii_alphanumeric()` — is the point: a test that only checked lengths
        // would still pass on the old URL-safe (alphanumeric) alphabet this dispatch replaces.
        let (secret, prefix) = mint();
        assert!(secret.starts_with(PREFIX_TAG));
        assert_eq!(secret.len(), PREFIX_TAG.len() + SECRET_LEN);
        assert!(
            secret[PREFIX_TAG.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "the random portion must be lowercase hex only: {secret:?}"
        );
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
            "am_us_12345",              // one short of VISIBLE_LEN (6)
        ] {
            let _ = candidate_prefix(input);
        }
        // VISIBLE_LEN is 6 now (fixture 23) — the previous version of this test hardcoded 8, which
        // would silently assert something false under the new constant rather than fail loudly
        // (`.claude/contracts/amk-store-http-prereqs.md`'s own warning about this site).
        assert_eq!(candidate_prefix("am_us_12345"), None, "too short to have a prefix at all");
        assert_eq!(
            candidate_prefix("am_us_12345678rest-of-the-secret"),
            Some("am_us_123456".to_owned())
        );
    }

    // ---- prefix-collision predicate, against a real error --------------------------------
    //
    // `is_prefix_collision` is a private helper: nothing under `tests/` (a separate crate) can
    // reach it, so this lives here, as a DB-touching unit test, rather than in
    // `tests/api_keys.rs`. Skips cleanly when the dev database is unreachable, matching every
    // other DB-touching test in this crate.

    /// Connect directly, bypassing `tests/support::pool()` (which this module cannot see — it is
    /// compiled into a separate integration-test crate). Mirrors its skip behaviour exactly:
    /// `Ok(None)` on any connection failure, never a panic, so `cargo test -p amk-store` still
    /// passes on a machine with no Postgres.
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

    #[tokio::test]
    async fn prefix_collision_is_distinguished_from_the_inboxes_pkey_violation() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };

        let org_id = format!("org-prefix-collision-{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO organizations (organization_id) VALUES ($1)")
            .bind(&org_id)
            .execute(&pool)
            .await
            .unwrap();
        let pod_id = Uuid::new_v4();
        sqlx::query("INSERT INTO pods (pod_id, organization_id, name) VALUES ($1, $2, 'p')")
            .bind(pod_id)
            .bind(&org_id)
            .execute(&pool)
            .await
            .unwrap();

        // The real error this predicate exists to recognize: two `api_keys` rows sharing one
        // `prefix`, raised by `api_keys_prefix_idx` (`UNIQUE`).
        let prefix = format!("am_us_{}", &Uuid::new_v4().simple().to_string()[..6]);
        let insert_key = || {
            sqlx::query(
                "INSERT INTO api_keys (api_key_id, organization_id, name, prefix, hash) \
                 VALUES ($1, $2, 'collision-probe', $3, 'irrelevant-hash')",
            )
            .bind(Uuid::new_v4())
            .bind(&org_id)
            .bind(&prefix)
        };
        insert_key().execute(&pool).await.unwrap();
        let prefix_err = insert_key()
            .execute(&pool)
            .await
            .expect_err("the second insert must collide on the UNIQUE prefix index");
        let sqlx::Error::Database(prefix_db_err) = prefix_err else {
            panic!("expected a database error, got something else");
        };
        assert_eq!(prefix_db_err.constraint(), Some("api_keys_prefix_idx"));
        assert!(
            is_prefix_collision(prefix_db_err.as_ref()),
            "a genuine prefix collision must be recognized"
        );

        // The negative case the dispatch contract names explicitly: a *different* unique
        // violation — `inboxes_pkey` — must not be misclassified as a prefix collision. Matching
        // by constraint name, not merely `is_unique_violation()`, is the whole point of this
        // predicate (see its own doc and `pods::is_pod_reference_violation`'s identical rule for
        // foreign-key violations).
        let inbox_id = format!("prefix-collision-probe-{}@example.test", Uuid::new_v4().simple());
        let insert_inbox = || {
            sqlx::query(
                "INSERT INTO inboxes (inbox_id, organization_id, pod_id) VALUES ($1, $2, $3)",
            )
            .bind(&inbox_id)
            .bind(&org_id)
            .bind(pod_id)
        };
        insert_inbox().execute(&pool).await.unwrap();
        let inbox_err = insert_inbox()
            .execute(&pool)
            .await
            .expect_err("the second insert must collide on inboxes_pkey");
        let sqlx::Error::Database(inbox_db_err) = inbox_err else {
            panic!("expected a database error, got something else");
        };
        assert_eq!(inbox_db_err.constraint(), Some("inboxes_pkey"));
        assert!(
            !is_prefix_collision(inbox_db_err.as_ref()),
            "a differently-named unique violation must not be misclassified as a prefix collision"
        );
    }
}
