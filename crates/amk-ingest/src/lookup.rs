//! Crate-local inbox lookup. There is no store `get_by_address`; RCPT uses this trait
//! plus the local-domain allow-list. Comparison is [`InboxId::eq_normalized`] only.

use amk_types::{InboxId, OrganizationId, PodId};
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
