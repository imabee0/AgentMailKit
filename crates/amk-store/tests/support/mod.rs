//! Shared integration-test scaffolding.
//!
//! Every DB-touching test in this crate goes through [`pool`], which returns `None` when the dev
//! database is unreachable — the dispatch contract requires `cargo test` to pass on a machine
//! without one, so a test that needs a database must skip cleanly, never fail.
//!
//! Tests share one Postgres instance and the `public` schema (no per-test schema isolation), so
//! every seed helper mints a fresh random id — this is what lets tests run concurrently (the
//! default for `cargo test`) without colliding, and it is *also* how the "two simultaneous
//! creates of the same username" test gets a real collision: it is the one test that deliberately
//! reuses one id from two tasks instead of minting two.

use amk_store::inboxes::{self, NewInbox};
use amk_store::organizations::{self, NewOrganization};
use amk_store::pods::{self, NewPod};
use amk_types::ids::{InboxId, OrganizationId, PodId};
use sqlx::PgPool;
use uuid::Uuid;

const DATABASE_URL: &str = "postgres://amk:amk-dev-local@127.0.0.1:55432/amk";

/// Connect (and migrate) fresh for each test.
///
/// Deliberately **not** shared behind a `once_cell`/static across tests: `#[tokio::test]` gives
/// each test its own Tokio runtime, and a `sqlx::PgPool`'s background connection-management tasks
/// are spawned on (and die with) the runtime that created it — a pool built by one test and
/// reused by another hangs the moment it needs to grow past whatever idle connections happened to
/// survive. Connecting fresh (~25-90ms, confirmed against the dev database) costs far less than
/// debugging that. Returns `None` when the dev database is unreachable — every DB-touching test
/// must skip cleanly rather than fail, per the dispatch contract, so `cargo test` on a machine
/// without Postgres still passes.
///
/// That skip is exactly what makes this gate silent in the direction that matters: a test suite
/// that reports `ok` whether or not it touched a database cannot tell "22 integration tests
/// passed" from "22 integration tests did nothing", and a gate that can silently verify nothing is
/// worse than one that fails. `AMK_REQUIRE_DB=1` closes that hole for a caller that already knows
/// a database is supposed to be there (`scripts/check.sh` sets it when the dev database answers):
/// with it set, an unreachable database is a **panic**, not a skip, so a developer whose local
/// Postgres breaks gets a loud failure instead of a quiet, meaningless pass. Any other value, or
/// the variable unset, keeps today's skip behaviour.
///
/// `amk_store::connect` does two distinct things behind one `Result`: it opens the connection,
/// then runs the embedded migrations against it — so a failure here is not always "unreachable".
/// This was found live: a shared dev database that already had migration 0007 applied made every
/// suite on a checkout with only 0001–0006 fail `connect` with *"migration 7 was previously
/// applied but is missing in the resolved migrations"* — `sqlx::Error::Migrate(_)` — which the
/// single "unreachable" message below rendered indistinguishably from Postgres genuinely being
/// down, sending the next reader to restart a database that was never the problem. The panic (and
/// the skip message) branch on the error variant so a migration mismatch reads as one.
pub async fn pool() -> Option<PgPool> {
    match amk_store::connect(DATABASE_URL).await {
        Ok(p) => Some(p),
        Err(e @ sqlx::Error::Migrate(_)) => {
            let msg = format!(
                "the dev database is reachable but its migration history disagrees with this \
                 checkout's migrations/ directory ({e}). This is not a connectivity problem — do \
                 not restart the database. Likely cause: this checkout and another checkout (or \
                 branch) sharing the same dev database disagree about which migrations exist. \
                 Reconcile the schema (or point AMK_DATABASE_URL/DATABASE_URL at a fresh \
                 database) before re-running."
            );
            if std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1") {
                panic!("{msg}");
            }
            eprintln!("skipping: {msg}");
            None
        }
        Err(e) => {
            if std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1") {
                panic!(
                    "AMK_REQUIRE_DB=1 but the dev database is unreachable ({e}). \
                     Run `./scripts/dev-db.sh up`, or unset AMK_REQUIRE_DB to allow this suite \
                     to skip its database-backed tests."
                );
            }
            eprintln!("skipping: dev database unreachable ({e})");
            None
        }
    }
}

pub fn unique_suffix() -> String {
    Uuid::new_v4().simple().to_string()
}

pub async fn seed_org(pool: &PgPool) -> OrganizationId {
    let id = OrganizationId::new(format!("org-{}", unique_suffix()));
    organizations::create(
        pool,
        NewOrganization {
            organization_id: id.clone(),
            name: None,
            inbox_limit: None,
            domain_limit: None,
        },
    )
    .await
    .expect("seed organization");
    id
}

pub async fn seed_pod(pool: &PgPool, org: &OrganizationId) -> PodId {
    let pod_id = PodId::new_random();
    pods::create(
        pool,
        NewPod { organization_id: org.clone(), pod_id, client_id: None, name: "test-pod".into() },
    )
    .await
    .expect("seed pod");
    pod_id
}

/// `local_part` is combined with a fresh random suffix and `@example.test` — callers that need an
/// exact address (e.g. to test case folding) should call [`inboxes::create`] directly instead.
pub async fn seed_inbox(
    pool: &PgPool,
    org: &OrganizationId,
    pod: PodId,
    local_part: &str,
) -> InboxId {
    let inbox_id = InboxId::new(format!("{local_part}-{}@example.test", unique_suffix()));
    let inbox = inboxes::create(
        pool,
        NewInbox {
            inbox_id,
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .expect("seed inbox");
    inbox.inbox_id
}

/// Convenience: a fresh org, one pod in it, and one inbox in that pod.
pub async fn seed_org_pod_inbox(pool: &PgPool) -> (OrganizationId, PodId, InboxId) {
    let org = seed_org(pool).await;
    let pod = seed_pod(pool, &org).await;
    let inbox = seed_inbox(pool, &org, pod, "inbox").await;
    (org, pod, inbox)
}
