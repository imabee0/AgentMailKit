//! `amk init` — mint the default organization, its default pod, and an org-scoped root API key
//! granting everything. See the dispatch contract's own "`amk init` — and it is what makes the
//! org mount work" section; this module is a direct implementation of its five numbered steps.
//!
//! # The id equality `amk-http` depends on
//!
//! Step 1 mints exactly one fresh UUID and uses it as **both** the organization's id and its
//! default pod's id — `amk_http`'s org-mount inbox creation resolves the default pod by
//! `pod_id == organization_id` (fixture 22). Any other arrangement (two independently minted
//! ids, or reusing an id from somewhere else) makes `POST /v0/inboxes` at the organization mount
//! an internal error in every deployment this binary sets up.
//!
//! # Why re-running `init` must refuse, and why that is not automatic
//!
//! `organizations::create` is a plain `INSERT`. Because step 1 mints a **fresh** UUID on every
//! call, a second run's row collides with nothing the first run wrote, and the `INSERT`
//! **succeeds** — silently minting a second organization, a second default pod, and a second
//! root key with every permission. There is no unique-violation to catch this by accident. The
//! guard has to be explicit and it has to run first: [`amk_store::organizations::exists`],
//! checked **before minting anything** — not the UUID, not the key.

use amk_store::api_keys::{self, NewApiKey};
use amk_store::organizations::{self, NewOrganization};
use amk_store::pods::{self, NewPod};
use amk_types::api_key::CreateApiKeyResponse;
use amk_types::ids::{OrganizationId, PodId};
use sqlx::PgPool;
use uuid::Uuid;

use crate::redact::{describe_connect_failure, describe_store_failure};

/// `[ASSUMED]` — no fixture names what a minted root key is called; `NewApiKey::name` is a
/// required free-text field regardless of scope. Matches `pods::create`'s own `"Default Pod"`
/// naming decision (also `[ASSUMED]`, same contract section) in spirit: an obvious, human-legible
/// label rather than an invented wire concept.
const ROOT_KEY_NAME: &str = "Root Key";

/// `[ASSUMED]` — see [`ROOT_KEY_NAME`]'s own note; this is the pod's name, not the key's.
const DEFAULT_POD_NAME: &str = "Default Pod";

/// Why `amk init` refused to run, or failed partway through.
#[derive(Debug)]
pub enum InitError {
    /// `AMK_DATABASE_URL` could not be used to connect (and migrate). Carries an already-redacted
    /// message — see `crate::redact` — never the underlying `sqlx::Error` itself.
    Connect(String),
    /// [`organizations::exists`] returned `true`: this deployment already has a root
    /// organization. Nothing was minted — no UUID, no organization row, no pod, no key.
    AlreadyInitialized,
    /// A store call failed after the `exists` guard passed. Carries an already-redacted message.
    Store(String),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Connect(msg) => write!(f, "{msg}"),
            InitError::AlreadyInitialized => write!(
                f,
                "this deployment is already initialised -- amk init only ever mints one root \
                 key. Run `amk doctor` to inspect the existing deployment."
            ),
            InitError::Store(msg) => write!(f, "amk init failed: {msg}"),
        }
    }
}

/// What a successful `amk init` produces: the ids `amk-http` will resolve the org mount by, and
/// the one and only copy of the root key's plaintext secret.
///
/// `Debug` is safe to derive: `CreateApiKeyResponse`'s own hand-written `Debug` already redacts
/// `api_key` to `<redacted>` (`amk_types::api_key`, precisely so a stray `{:?}` anywhere in this
/// codebase cannot leak it) — this struct only wraps that guarantee, never bypasses it.
#[derive(Debug)]
pub struct InitOutcome {
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub root_key: CreateApiKeyResponse,
}

/// Connect (and migrate) using `AMK_DATABASE_URL`'s value, then run [`run_with_pool`].
pub async fn run(database_url: &str) -> Result<InitOutcome, InitError> {
    let pool = amk_store::connect(database_url).await.map_err(|e| {
        InitError::Connect(format!(
            "could not connect using AMK_DATABASE_URL: {}",
            describe_connect_failure(&e)
        ))
    })?;
    run_with_pool(&pool).await
}

/// The five steps, against an already-connected pool — split out from [`run`] so tests can drive
/// it against a database they already hold a pool for, without reconnecting.
pub async fn run_with_pool(pool: &PgPool) -> Result<InitOutcome, InitError> {
    // Step 0 (the guard `organizations::create`'s own `INSERT` cannot give us — see the module
    // doc): refuse before minting anything at all if this deployment already has a root
    // organization.
    let already = organizations::exists(pool)
        .await
        .map_err(|e| InitError::Store(describe_store_failure(&e)))?;
    if already {
        return Err(InitError::AlreadyInitialized);
    }

    // Step 1: one fresh UUID, used as both the organization's id and its default pod's id.
    let shared_id = Uuid::new_v4();
    let organization_id = OrganizationId::new(shared_id.to_string());
    let pod_id = PodId::from(shared_id);

    // Step 2.
    organizations::create(
        pool,
        NewOrganization {
            organization_id: organization_id.clone(),
            inbox_limit: None,
            domain_limit: None,
        },
    )
    .await
    .map_err(|e| InitError::Store(describe_store_failure(&e)))?;

    // Step 3.
    pods::create(
        pool,
        NewPod {
            organization_id: organization_id.clone(),
            pod_id,
            client_id: None,
            name: DEFAULT_POD_NAME.to_owned(),
        },
    )
    .await
    .map_err(|e| InitError::Store(describe_store_failure(&e)))?;

    // Step 4: organization-scoped (`pod_id`/`inbox_id` both `None`), `permissions: None` — grants
    // everything (the NULL-vs-`{}` distinction `amk-store` owns; see `NewApiKey::permissions`'s
    // own doc comment). This is the root key.
    let root_key = api_keys::create(
        pool,
        NewApiKey {
            organization_id: organization_id.clone(),
            pod_id: None,
            inbox_id: None,
            name: ROOT_KEY_NAME.to_owned(),
            permissions: None,
        },
    )
    .await
    .map_err(|e| InitError::Store(describe_store_failure(&e)))?;

    // Step 5 (printing the three values exactly once) is the caller's job — `src/bin/amk.rs` —
    // this function only returns them.
    Ok(InitOutcome { organization_id, pod_id, root_key })
}
