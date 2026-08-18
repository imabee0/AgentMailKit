//! Crate-local inbox lookup. RCPT uses this trait plus the local-domain allow-list.
//! Comparison is [`InboxId::eq_normalized`] only.

use amk_types::{InboxId, OrganizationId, PodId};
use sqlx::PgPool;
use std::future::Future;

/// Maps an RCPT address to the inbox that should receive it.
///
/// Not an `amk-types` item — this crate owns the seam. A `None` is RCPT 550.
pub trait InboxLookup: Send + Sync {
    fn lookup(
        &self,
        inbox_id: &InboxId,
    ) -> impl Future<Output = Option<(OrganizationId, PodId, InboxId)>> + Send;
}

/// In-memory map for tests and for a caller that already resolved the inbox.
#[derive(Debug, Clone, Default)]
pub struct FixedInboxLookup {
    entries: Vec<(InboxId, OrganizationId, PodId)>,
}

impl FixedInboxLookup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, inbox_id: InboxId, organization_id: OrganizationId, pod_id: PodId) {
        self.entries.push((inbox_id, organization_id, pod_id));
    }
}

impl InboxLookup for FixedInboxLookup {
    async fn lookup(&self, inbox_id: &InboxId) -> Option<(OrganizationId, PodId, InboxId)> {
        self.entries.iter().find_map(|(id, org, pod)| {
            id.eq_normalized(inbox_id)
                .then(|| (org.clone(), *pod, id.clone()))
        })
    }
}

/// Store-backed lookup: `inboxes::get_by_inbox_id` (normalized PK). A store error is `None`.
#[derive(Clone)]
pub struct StoreInboxLookup {
    pub pool: PgPool,
}

impl InboxLookup for StoreInboxLookup {
    async fn lookup(&self, inbox_id: &InboxId) -> Option<(OrganizationId, PodId, InboxId)> {
        let inbox = amk_store::inboxes::get_by_inbox_id(&self.pool, inbox_id)
            .await
            .ok()
            .flatten()?;
        Some((inbox.organization_id?, inbox.pod_id, inbox.inbox_id))
    }
}
