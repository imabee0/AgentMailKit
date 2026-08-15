//! Scope resolution for the triple-mounted API surface.
//!
//! Most collections are mounted three ways — `/v0/threads`, `/v0/pods/{pod_id}/threads` and
//! `/v0/inboxes/{inbox_id}/threads` — over one handler set. This module is the single place that
//! decides what a credential may reach, so a handler never re-derives the rule.
//!
//! # The critical rule: a denial is a filter, not a rejection
//!
//! A pod-scoped credential reaching an inbox in another pod, and every cross-organization
//! attempt at every mount, surfaces as **`not_found` (404)** — never `forbidden` (403). Hidden
//! things must be indistinguishable from absent things, which also means they must not leak
//! through counts, pagination totals or thread membership.
//!
//! Evidence, split by what each source actually shows:
//! * that a *scope* denial masks as `not_found` rather than `forbidden` is spec, not capture:
//!   `[SPEC:docs permissions]`, carried into the plan as "Pod-scoped key reaching an inbox in a
//!   different pod → `not_found`, NOT forbidden (scope/label denial masks as not_found per
//!   docs)". **No cross-tenant probe exists in `reference/`** — we hold one organization's key,
//!   so the cross-tenant case could not be observed;
//! * `reference/fixtures/05-error-catalog.http:32-35` shows only the *shape* of that answer, for
//!   a lookup of a nonexistent inbox: a full envelope, 404, with a `fix` string that names the
//!   credential's scope and restricted labels as the two reasons a resource can be invisible.
//!
//! The rule is enforced by shape, not by discipline:
//! * the only error this module can produce for a reachability decision is [`ScopeDenial`],
//!   which converts into one envelope only — `not_found` / 404. There is no boolean to misread
//!   and no 403 to reach for;
//! * [`Scope::resolve`] intersects the credential's scope with the mount and yields a
//!   [`ScopeFilter`] that **always pins an organization**. Every list query is built from it, so
//!   a cross-organization row cannot enter a result set — and therefore cannot enter a count;
//! * when the mount names a pod or inbox the credential does not itself prove, `resolve` yields
//!   a [`MountProbe`] instead of a window, so the mount's own resource must be looked up and
//!   [`MountProbe::settle`]d before any collection under it can be served;
//! * [`ScopeFilter::check`] takes a row **by value** and hands it back only when it is visible,
//!   so a denied row is not left in the caller's hands;
//! * an unknown coordinate on a row never matches a pinned one ([`ScopeFilter`] fails closed).
//!
//! # What this module does *not* decide
//!
//! Restricted-label visibility (`spam`, `blocked`, `unauthenticated`, `trash`) is one composed
//! rule owned by [`crate::labels`], and permission checks are owned by [`crate::permissions`]
//! over [`amk_types::KeyGrants`]. Scope is orthogonal: passing [`ScopeFilter::check`] means the
//! row is in the credential's *window*, never that it is visible.
//!
//! Nothing here models a Stalwart or JMAP concept; every identifier comes from `amk-types`.

use amk_types::{
    ids::{InboxId, OrganizationId, PodId},
    message::{Message, MessageItem},
    ErrorCode, ErrorEnvelope, GatewayError, Identity, Inbox, Pod, ScopeType, Thread, ThreadItem,
};
use uuid::Uuid;

/// The `fix` text served with a masked lookup.
///
/// Both live captures elide part of this string, so no fixture holds it whole.
/// `reference/fixtures/09b-unauthenticated-variant.txt:89-91` captures the longest run — the
/// entire tail, reproduced here **verbatim** (pinned by `CAPTURED_FIX_TAIL` below).
/// `reference/fixtures/05-error-catalog.http:33-35` captures the head instead, showing the same
/// sentence also names the credential's scope: "... scope (organization, pod, or inbox) ...".
/// Only the clause joining the two is ours. The envelope *key* is what the conformance diff
/// compares; one rendering lives here so the server never emits two.
const MASK_FIX: &str = "Visibility depends on the credential's scope (organization, pod, or \
     inbox), and some resources (e.g. restricted labels like spam or trash) are hidden without \
     their label-read permission. The corresponding list endpoint returns only the ids visible \
     to you.";

/// The part of [`MASK_FIX`] captured verbatim from the live API
/// (`reference/fixtures/09b-unauthenticated-variant.txt:90-91`, unwrapped). Its only job is to
/// hold [`MASK_FIX`] to that capture, so it lives with the test that does the holding.
#[cfg(test)]
const CAPTURED_FIX_TAIL: &str = "some resources (e.g. restricted labels like spam or trash) are \
     hidden without their label-read permission. The corresponding list endpoint returns only \
     the ids visible to you.";

/// What kind of resource a masked lookup was for. Chooses the `message` wording only — every
/// kind produces the same code and status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Organization,
    Pod,
    Inbox,
    Thread,
    Message,
    Draft,
    Attachment,
    Domain,
    Webhook,
    ApiKey,
}

impl ResourceKind {
    /// Every kind, so tests can assert the masking rule holds for all of them.
    pub const ALL: [ResourceKind; 10] = [
        ResourceKind::Organization,
        ResourceKind::Pod,
        ResourceKind::Inbox,
        ResourceKind::Thread,
        ResourceKind::Message,
        ResourceKind::Draft,
        ResourceKind::Attachment,
        ResourceKind::Domain,
        ResourceKind::Webhook,
        ResourceKind::ApiKey,
    ];

    /// The noun used in the error message, e.g. `Inbox` in the observed "Inbox not found"
    /// (fixture 05) and `Message` in "Message not found" (fixture 09b).
    pub const fn noun(self) -> &'static str {
        match self {
            ResourceKind::Organization => "Organization",
            ResourceKind::Pod => "Pod",
            ResourceKind::Inbox => "Inbox",
            ResourceKind::Thread => "Thread",
            ResourceKind::Message => "Message",
            ResourceKind::Draft => "Draft",
            ResourceKind::Attachment => "Attachment",
            ResourceKind::Domain => "Domain",
            ResourceKind::Webhook => "Webhook",
            ResourceKind::ApiKey => "API key",
        }
    }
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.noun())
    }
}

/// The masked outcome of a reachability decision — the only failure this module reports.
///
/// It carries no reason, because a reason is exactly what must not reach the client: "outside
/// your scope" and "does not exist" have to be the same answer. Handlers also construct it
/// directly for a genuinely absent row, so both paths produce one byte-identical body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{kind} not found")]
pub struct ScopeDenial {
    kind: ResourceKind,
}

impl ScopeDenial {
    pub const fn new(kind: ResourceKind) -> Self {
        Self { kind }
    }

    pub const fn kind(self) -> ResourceKind {
        self.kind
    }

    /// Always 404, delegated to the code catalog so the status has one owner (`amk-types`).
    /// There is deliberately no path to 403.
    pub fn status(self) -> u16 {
        ErrorCode::NotFound.status()
    }

    pub fn into_envelope(self) -> ErrorEnvelope {
        ErrorEnvelope::new(ErrorCode::NotFound, format!("{} not found", self.kind))
            .with_fix(MASK_FIX)
    }
}

impl From<ScopeDenial> for ErrorEnvelope {
    fn from(d: ScopeDenial) -> Self {
        d.into_envelope()
    }
}

/// A credential whose [`Identity`] is internally inconsistent and therefore unusable.
///
/// This is not a scope denial: nothing was looked up, so there is nothing to mask. It is an
/// auth-layer failure, and the auth layer answers with the bare gateway body.
///
/// Citation, precisely: `reference/fixtures/05-error-catalog.http:10-16` shows the auth layer
/// answering `403 {"message":"Forbidden"}` for a credential *it* rejects (an unknown key). A
/// self-contradictory **resolved** identity was never observed — it cannot be produced from
/// outside, only by our own credential store contradicting itself. Serving it as an auth-layer
/// failure is our choice: it is the closest observed neighbour, and it keeps an unresolvable
/// credential out of every handler rather than letting it widen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeResolutionError {
    /// `scope_type` is `pod` or `inbox` but no `pod_id` is present. `openapi.json`
    /// `type_auth:Identity` — `pod_id`: *"Present when scope_type is pod **or inbox**."*
    #[error("pod- or inbox-scoped credential carries no pod_id")]
    MissingPodId,
    /// `scope_type` is `inbox` but no `inbox_id` is present. `openapi.json`
    /// `type_auth:Identity` — `inbox_id`: *"Present when scope_type is inbox."*
    #[error("inbox-scoped credential carries no inbox_id")]
    MissingInboxId,
    /// The identity carries an id **narrower** than its own `scope_type`: an `organization`
    /// scope with a `pod_id`/`inbox_id`, or a `pod` scope with an `inbox_id`.
    ///
    /// The same spec sentences that make a missing id a contradiction make a surplus one a
    /// contradiction — they say *present when*, and nothing licenses the wider forms. Rejecting
    /// only the missing half would be the dangerous half to get wrong: an identity claiming
    /// `organization` while naming a single bound inbox would read the entire organization.
    #[error("identity carries an id narrower than its scope_type")]
    NarrowerIdThanScope,
    /// `scope_id` does not name the resource the credential is scoped to. `openapi.json`
    /// `type_auth:Identity` — `scope_id`: *"ID of the most specific scope the credential is
    /// bound to. Equals inbox_id when scope_type is inbox, pod_id when pod, organization_id when
    /// organization."* (Fixture 01 shows one org-scoped sample agreeing with that sentence; the
    /// sentence, not the sample, is the rule for all three tiers.)
    #[error("scope_id does not name the scoped resource")]
    ScopeIdMismatch,
}

impl ScopeResolutionError {
    /// The body the auth layer returns: `403 {"message":"Forbidden"}`.
    pub fn gateway_body(&self) -> GatewayError {
        GatewayError::forbidden()
    }
}

/// Which of the three mounts the request arrived on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    /// `/v0/<collection>`
    Organization,
    /// `/v0/pods/{pod_id}/<collection>`
    Pod(PodId),
    /// `/v0/inboxes/{inbox_id}/<collection>`
    Inbox(InboxId),
}

/// The window a credential carries, resolved once from its [`Identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Organization {
        organization_id: OrganizationId,
    },
    Pod {
        organization_id: OrganizationId,
        pod_id: PodId,
    },
    Inbox {
        organization_id: OrganizationId,
        /// Required, per `openapi.json` `type_auth:Identity`: `pod_id` is *"Present when
        /// scope_type is pod or inbox."* An optional pod here would be an invented state, and
        /// an expensive one — an inbox credential with no pod pinned matches any pod in the
        /// organization, so `/v0/pods/{any}/threads` would resolve for it.
        pod_id: PodId,
        inbox_id: InboxId,
    },
}

/// `scope_id` is a free-text column while `pod_id` is a UUID, so the two arrive in whatever
/// textual form their writers chose. Comparing the *text* would reject `{...}`-wrapped, `urn:`-
/// prefixed or uppercase renderings of the very same pod and lock the key out of its own data;
/// comparing the *values* cannot.
fn scope_id_names_pod(scope_id: &str, pod_id: PodId) -> bool {
    Uuid::parse_str(scope_id).is_ok_and(|u| u == pod_id.0)
}

impl Scope {
    /// Resolve a credential's identity into a window.
    ///
    /// Every arm rejects an identity that disagrees with `openapi.json` `type_auth:Identity`, in
    /// both directions: a narrow `scope_type` missing the id it is defined by, and a wide
    /// `scope_type` carrying an id narrower than itself. That combination is our own credential
    /// store contradicting itself, and the safe reading of a contradiction is "unusable", never
    /// "organization".
    pub fn from_identity(identity: &Identity) -> Result<Self, ScopeResolutionError> {
        let organization_id = identity.organization_id.clone();
        match identity.scope_type {
            ScopeType::Organization => {
                if identity.pod_id.is_some() || identity.inbox_id.is_some() {
                    return Err(ScopeResolutionError::NarrowerIdThanScope);
                }
                if identity.scope_id != organization_id.as_str() {
                    return Err(ScopeResolutionError::ScopeIdMismatch);
                }
                Ok(Scope::Organization { organization_id })
            }
            ScopeType::Pod => {
                if identity.inbox_id.is_some() {
                    return Err(ScopeResolutionError::NarrowerIdThanScope);
                }
                let pod_id = identity.pod_id.ok_or(ScopeResolutionError::MissingPodId)?;
                if !scope_id_names_pod(&identity.scope_id, pod_id) {
                    return Err(ScopeResolutionError::ScopeIdMismatch);
                }
                Ok(Scope::Pod { organization_id, pod_id })
            }
            ScopeType::Inbox => {
                let pod_id = identity.pod_id.ok_or(ScopeResolutionError::MissingPodId)?;
                let inbox_id = identity
                    .inbox_id
                    .clone()
                    .ok_or(ScopeResolutionError::MissingInboxId)?;
                // Case-folded, per fixture 18: the live API stores the lowercased address and
                // resolves lookups case-insensitively, so a `scope_id` recorded in the caller's
                // original casing still names this inbox.
                if !InboxId::new(identity.scope_id.clone()).eq_normalized(&inbox_id) {
                    return Err(ScopeResolutionError::ScopeIdMismatch);
                }
                Ok(Scope::Inbox { organization_id, pod_id, inbox_id })
            }
        }
    }

    pub fn organization_id(&self) -> &OrganizationId {
        match self {
            Scope::Organization { organization_id }
            | Scope::Pod { organization_id, .. }
            | Scope::Inbox { organization_id, .. } => organization_id,
        }
    }

    /// Intersect this scope with the mount the request arrived on.
    ///
    /// Three outcomes, and the middle one is the point:
    ///
    /// * **[`ScopeDenial`]** — the mount names a pod or inbox the credential's *own* ids already
    ///   rule out (`/v0/pods/{other}/…` for a pod key). Decided here, on ids alone.
    /// * **[`Resolved::Probe`]** — the mount names a pod or inbox the credential neither proves
    ///   nor rules out (`/v0/inboxes/{any}/…` for a pod key: an address alone says nothing about
    ///   which pod holds it). Ids cannot settle this and neither can this crate — it takes a
    ///   lookup. Handing back a plain window here is how
    ///   `GET /v0/inboxes/foreign@x/threads` comes to answer `200 {"count":0}` while the same
    ///   URL for an *absent* inbox answers 404: two different answers for hidden and absent,
    ///   which is precisely the distinction this module exists to erase. The probe forces the
    ///   mount's own resource through one lookup, so both cases leave by the same door.
    /// * **[`Resolved::Ready`]** — every resource the mount names is one the credential is
    ///   itself scoped to (or scoped inside), so its existence is already proven and the window
    ///   is settled.
    ///
    /// A mount the credential can see but which is *wider* than the credential simply narrows:
    /// an inbox-scoped key on `/v0/threads` gets its own inbox's threads, not a rejection.
    pub fn resolve(&self, mount: &Mount) -> Result<Resolved, ScopeDenial> {
        let window = |pod_id: Option<PodId>, inbox_id: Option<InboxId>| ScopeFilter {
            organization_id: self.organization_id().clone(),
            pod_id,
            // Pinned in the stored form: fixture 18 shows the live API lowercases the address at
            // creation, so the canonical id is the lowercased one.
            inbox_id: inbox_id.map(|i| i.normalized()),
        };
        let probe = |filter: ScopeFilter, kind: ResourceKind| {
            Ok(Resolved::Probe(MountProbe { filter, kind, mount: mount.clone() }))
        };

        match (self, mount) {
            (Scope::Organization { .. }, Mount::Organization) => {
                Ok(Resolved::Ready(window(None, None)))
            }
            (Scope::Organization { .. }, Mount::Pod(p)) => {
                probe(window(Some(*p), None), ResourceKind::Pod)
            }
            (Scope::Organization { .. }, Mount::Inbox(i)) => {
                probe(window(None, Some(i.clone())), ResourceKind::Inbox)
            }

            (Scope::Pod { pod_id, .. }, Mount::Organization) => {
                Ok(Resolved::Ready(window(Some(*pod_id), None)))
            }
            (Scope::Pod { pod_id, .. }, Mount::Pod(p)) if p == pod_id => {
                Ok(Resolved::Ready(window(Some(*pod_id), None)))
            }
            (Scope::Pod { .. }, Mount::Pod(_)) => Err(ScopeDenial::new(ResourceKind::Pod)),
            // The inbox mount names only an address; which pod holds that address is a fact in
            // the store, not in the URL, so it must be probed.
            (Scope::Pod { pod_id, .. }, Mount::Inbox(i)) => {
                probe(window(Some(*pod_id), Some(i.clone())), ResourceKind::Inbox)
            }

            (Scope::Inbox { pod_id, inbox_id, .. }, Mount::Organization) => {
                Ok(Resolved::Ready(window(Some(*pod_id), Some(inbox_id.clone()))))
            }
            (Scope::Inbox { pod_id, inbox_id, .. }, Mount::Pod(p)) if p == pod_id => {
                Ok(Resolved::Ready(window(Some(*pod_id), Some(inbox_id.clone()))))
            }
            (Scope::Inbox { .. }, Mount::Pod(_)) => Err(ScopeDenial::new(ResourceKind::Pod)),
            (Scope::Inbox { pod_id, inbox_id, .. }, Mount::Inbox(i))
                if i.eq_normalized(inbox_id) =>
            {
                Ok(Resolved::Ready(window(Some(*pod_id), Some(inbox_id.clone()))))
            }
            (Scope::Inbox { .. }, Mount::Inbox(_)) => Err(ScopeDenial::new(ResourceKind::Inbox)),
        }
    }
}

/// The outcome of [`Scope::resolve`] when the request was not denied outright.
///
/// An enum rather than a flag on [`ScopeFilter`] so that the pending case cannot be skipped by
/// omission: there is no way to obtain a window from a [`Resolved::Probe`] except by settling it
/// (or by calling [`Resolved::window`], which says in its own name that the mount is unproven).
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a Resolved that is never matched leaves the mount's own resource unproven"]
pub enum Resolved {
    /// Settled: the credential itself proves every resource the mount names. Build queries from
    /// this window.
    Ready(ScopeFilter),
    /// The mount names a pod or inbox this credential has not been shown to reach. Look that
    /// resource up inside [`MountProbe::window`], then discharge with [`MountProbe::settle`].
    Probe(MountProbe),
}

impl Resolved {
    /// The window *without* discharging a pending probe.
    ///
    /// Correct when the handler is serving the mount's **own** resource — `GET /v0/inboxes/{id}`
    /// with an inbox mount — because there the lookup *is* the probe and its miss is already the
    /// masked answer. Reaching for this to serve a sub-collection is the bug [`Resolved`] exists
    /// to make visible.
    pub fn window(&self) -> &ScopeFilter {
        match self {
            Resolved::Ready(f) => f,
            Resolved::Probe(p) => &p.filter,
        }
    }

    /// The window, or `None` when a probe is still outstanding.
    pub fn into_ready(self) -> Option<ScopeFilter> {
        match self {
            Resolved::Ready(f) => Some(f),
            Resolved::Probe(_) => None,
        }
    }
}

/// A mount whose named resource has not been proven to lie inside the window.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "an unsettled MountProbe means the mount's resource was never checked"]
pub struct MountProbe {
    filter: ScopeFilter,
    kind: ResourceKind,
    mount: Mount,
}

impl MountProbe {
    /// The window the mount's own resource must be looked up in. Already pins the organization
    /// (and the pod, where the credential has one), so the probing query cannot itself reach
    /// another tenant.
    pub fn window(&self) -> &ScopeFilter {
        &self.filter
    }

    /// What the URL named.
    pub fn mount(&self) -> &Mount {
        &self.mount
    }

    /// The noun the denial will use — [`ResourceKind::Pod`] or [`ResourceKind::Inbox`].
    pub fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Discharge with the row the store returned for [`Self::mount`], or `None` when the store
    /// found nothing.
    ///
    /// Absent and out-of-window take the same branch and produce the identical [`ScopeDenial`] —
    /// that is the whole reason the probe exists.
    pub fn settle<T: Scoped>(self, row: Option<T>) -> Result<ScopeFilter, ScopeDenial> {
        match row {
            Some(r) if self.filter.admits(&r.resource_scope()) => Ok(self.filter),
            _ => Err(ScopeDenial::new(self.kind)),
        }
    }
}

/// Where a row lives. Unknown coordinates are `None` and never match a pinned one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceScope {
    pub organization_id: Option<OrganizationId>,
    pub pod_id: Option<PodId>,
    pub inbox_id: Option<InboxId>,
}

impl ResourceScope {
    pub fn organization(organization_id: OrganizationId) -> Self {
        Self { organization_id: Some(organization_id), pod_id: None, inbox_id: None }
    }

    pub fn pod(organization_id: OrganizationId, pod_id: PodId) -> Self {
        Self { organization_id: Some(organization_id), pod_id: Some(pod_id), inbox_id: None }
    }

    pub fn inbox(organization_id: OrganizationId, pod_id: PodId, inbox_id: InboxId) -> Self {
        Self {
            organization_id: Some(organization_id),
            pod_id: Some(pod_id),
            inbox_id: Some(inbox_id),
        }
    }
}

/// A row that knows where it lives, so a handler cannot pass the wrong coordinates.
pub trait Scoped {
    const KIND: ResourceKind;
    fn resource_scope(&self) -> ResourceScope;
}

impl Scoped for Pod {
    const KIND: ResourceKind = ResourceKind::Pod;
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope {
            organization_id: self.organization_id.clone(),
            pod_id: Some(self.pod_id),
            inbox_id: None,
        }
    }
}

impl Scoped for Inbox {
    const KIND: ResourceKind = ResourceKind::Inbox;
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope {
            organization_id: self.organization_id.clone(),
            pod_id: Some(self.pod_id),
            inbox_id: Some(self.inbox_id.clone()),
        }
    }
}

impl Scoped for ThreadItem {
    const KIND: ResourceKind = ResourceKind::Thread;
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope {
            organization_id: self.organization_id.clone(),
            pod_id: self.pod_id,
            inbox_id: Some(self.inbox_id.clone()),
        }
    }
}

// NOTE: there is deliberately no `impl Scoped for Thread`. A [`Thread`] carries `messages` plus
// the aggregates `message_count` and `size`; checking only the thread's own coordinates would
// hand that membership back unexamined, and the trait's one-coordinate shape cannot express
// "and every message in it". [`ScopeFilter::check_thread`] does that check explicitly.

impl Scoped for MessageItem {
    const KIND: ResourceKind = ResourceKind::Message;
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope {
            organization_id: self.organization_id.clone(),
            pod_id: self.pod_id,
            inbox_id: Some(self.inbox_id.clone()),
        }
    }
}

impl Scoped for Message {
    const KIND: ResourceKind = ResourceKind::Message;
    fn resource_scope(&self) -> ResourceScope {
        self.item.resource_scope()
    }
}

/// The intersection of a credential's scope and the request's mount: the only window a handler
/// is allowed to see through.
///
/// **Every** store query built from this filter must apply *all* pinned coordinates — including
/// the queries behind `count`, `next_page_token` and a thread's message list. A row excluded
/// here must never have been counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFilter {
    organization_id: OrganizationId,
    pod_id: Option<PodId>,
    inbox_id: Option<InboxId>,
}

impl ScopeFilter {
    /// Always pinned: no query may span organizations.
    pub fn organization_id(&self) -> &OrganizationId {
        &self.organization_id
    }

    /// Pinned when the credential or the mount names a pod.
    pub fn pod_id(&self) -> Option<&PodId> {
        self.pod_id.as_ref()
    }

    /// Pinned, in lowercased form, when the credential or the mount names an inbox.
    ///
    /// Fixture 18: the live API stores the lowercased address, so a store query must compare
    /// against this value case-insensitively (or against a lowercased column) — never against
    /// the caller's original casing.
    pub fn inbox_id(&self) -> Option<&InboxId> {
        self.inbox_id.as_ref()
    }

    /// Mask for a row that the store did not return (absent, or excluded by this filter).
    /// Identical to the body a hidden row produces — that is the point.
    pub fn not_found(&self, kind: ResourceKind) -> ScopeDenial {
        ScopeDenial::new(kind)
    }

    fn admits(&self, at: &ResourceScope) -> bool {
        if at.organization_id.as_ref() != Some(&self.organization_id) {
            return false;
        }
        if let Some(pod) = &self.pod_id {
            if at.pod_id.as_ref() != Some(pod) {
                return false;
            }
        }
        if let Some(inbox) = &self.inbox_id {
            // Case-folded per fixture 18; `==` here would hide a row from the very credential
            // bound to it whenever the two casings differ.
            match &at.inbox_id {
                Some(row) if row.eq_normalized(inbox) => {}
                _ => return false,
            }
        }
        true
    }

    /// Hand back the row only if it is inside the window; otherwise the masked outcome.
    ///
    /// Takes the row by value so a denied row is consumed rather than left available.
    pub fn check<T: Scoped>(&self, value: T) -> Result<T, ScopeDenial> {
        self.check_at(T::KIND, &value.resource_scope(), value)
    }

    /// A thread is admitted only when the thread **and every message in it** are inside the
    /// window.
    ///
    /// Threads never span inboxes (`reference/fixtures/16-threading-matrix/`), so a thread whose
    /// membership straddles the window is a storage defect, not a case to paper over. Dropping
    /// the offending messages would mean re-deriving `message_count` and `size` here and serving
    /// a body the store never produced — a plausible-looking answer that hides the defect. The
    /// whole thread is masked instead.
    pub fn check_thread(&self, thread: Thread) -> Result<Thread, ScopeDenial> {
        let denied = || ScopeDenial::new(ResourceKind::Thread);
        if !self.admits(&thread.item.resource_scope()) {
            return Err(denied());
        }
        if !thread
            .messages
            .iter()
            .all(|m| self.admits(&m.resource_scope()))
        {
            return Err(denied());
        }
        Ok(thread)
    }

    /// As [`ScopeFilter::check`], for a resource whose coordinates the caller supplies.
    ///
    /// **Deliberately crate-private.** [`ResourceKind::ALL`] names ten kinds and only four have
    /// `Scoped` impls — `Draft`, `Attachment`, `Domain`, `Webhook`, `ApiKey` and `Organization`
    /// have no wire type in `amk-types` yet, so today the only way to call this from outside
    /// would be with hand-built coordinates. Coordinates built from the *filter* rather than
    /// from the *row* make the check a tautology that always passes and reports nothing, and a
    /// security check that cannot fail is worse than none. It reopens as `pub` in the same
    /// change that lands those types with their own `Scoped` impls.
    pub(crate) fn check_at<T>(
        &self,
        kind: ResourceKind,
        at: &ResourceScope,
        value: T,
    ) -> Result<T, ScopeDenial> {
        if self.admits(at) {
            Ok(value)
        } else {
            Err(ScopeDenial::new(kind))
        }
    }

    /// Keep only the rows inside the window.
    ///
    /// **For non-paginated collections only, and it is not the whole admission rule.**
    ///
    /// * It filters by *scope*. Restricted-label exclusion (`spam`, `blocked`, `unauthenticated`,
    ///   `trash`) is a separate, composed rule owned by [`crate::labels`]; a row that survives
    ///   this call may still be inexpressible in a list response.
    /// * Post-filtering a *page* leaks. The store selects N rows, this drops some, and the page
    ///   comes back with a short `count` — or `count: 0` — while still carrying a
    ///   `next_page_token`. Walking those tokens counts the hidden rows one page at a time, and
    ///   the observable difference between "empty page with a token" and "last page" is exactly
    ///   the signal the masking rule forbids. Admission must therefore be a **storage-layer
    ///   predicate**: the pinned coordinates belong in the `WHERE` clause that keyset pagination
    ///   runs over, so hidden rows are never selected, never counted and never advance a cursor.
    pub fn retain_visible<T: Scoped>(&self, items: Vec<T>) -> Vec<T> {
        items
            .into_iter()
            .filter(|i| self.admits(&i.resource_scope()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amk_types::{
        ids::*, message::MessageItem, ErrorCode, Identity, Inbox, Pod, ScopeType, ThreadItem,
        Timestamp,
    };

    // ---- fixtures -------------------------------------------------------------------------
    // Organization id is the live one from reference/fixtures/01-auth-me.http.
    const ORG: &str = "133c9cbe-f996-4094-a8d5-0c6603e022ea";
    const OTHER_ORG: &str = "9f4e1d2c-0000-4000-8000-aaaaaaaaaaaa";
    const INBOX: &str = "amk-probe@agentmail.to";
    const OTHER_INBOX: &str = "amk-probe2@agentmail.to";

    fn org() -> OrganizationId {
        OrganizationId::new(ORG)
    }
    fn other_org() -> OrganizationId {
        OrganizationId::new(OTHER_ORG)
    }

    /// Live `GET /v0/auth/me` body, verbatim (reference/fixtures/01-auth-me.http).
    const LIVE_ORG_IDENTITY: &str = r#"{"api_key_id":"3c5547b5-e7ff-474e-9871-83e82251568e",
        "organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "scope_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea","scope_type":"organization"}"#;

    fn org_identity() -> Identity {
        serde_json::from_str(LIVE_ORG_IDENTITY).unwrap()
    }

    fn pod_identity(pod: PodId) -> Identity {
        Identity {
            api_key_id: Some(ApiKeyId::new("key-pod")),
            organization_id: org(),
            scope_id: pod.to_string(),
            scope_type: ScopeType::Pod,
            pod_id: Some(pod),
            inbox_id: None,
        }
    }

    fn inbox_identity(pod: PodId, inbox: &str) -> Identity {
        Identity {
            api_key_id: Some(ApiKeyId::new("key-inbox")),
            organization_id: org(),
            scope_id: inbox.to_owned(),
            scope_type: ScopeType::Inbox,
            pod_id: Some(pod),
            inbox_id: Some(InboxId::new(inbox)),
        }
    }

    fn inbox_record(o: &OrganizationId, pod: PodId, addr: &str) -> Inbox {
        Inbox {
            organization_id: Some(o.clone()),
            pod_id: pod,
            inbox_id: InboxId::new(addr),
            email: addr.to_owned(),
            client_id: None,
            display_name: None,
            metadata: None,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    fn pod_record(o: &OrganizationId, pod: PodId) -> Pod {
        Pod {
            organization_id: Some(o.clone()),
            pod_id: pod,
            client_id: None,
            name: "p".into(),
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    fn message_record(o: Option<OrganizationId>, pod: Option<PodId>, addr: &str) -> MessageItem {
        MessageItem {
            organization_id: o,
            pod_id: pod,
            inbox_id: InboxId::new(addr),
            thread_id: ThreadId::new_random(),
            message_id: MessageId::new("<x@email.amazonses.com>"),
            labels: vec!["received".into()],
            timestamp: Timestamp::now(),
            from: "a@b.c".into(),
            to: vec![addr.to_owned()],
            cc: None,
            bcc: None,
            subject: None,
            preview: None,
            attachments: None,
            in_reply_to: None,
            references: None,
            headers: None,
            smtp_id: None,
            size: 1,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    fn thread_record(o: &OrganizationId, pod: PodId, addr: &str) -> ThreadItem {
        ThreadItem {
            organization_id: Some(o.clone()),
            pod_id: Some(pod),
            inbox_id: InboxId::new(addr),
            thread_id: ThreadId::new_random(),
            labels: vec![],
            timestamp: Timestamp::now(),
            received_timestamp: None,
            sent_timestamp: None,
            senders: vec![],
            recipients: vec![],
            subject: None,
            preview: None,
            attachments: None,
            last_message_id: MessageId::new("<x@y.z>"),
            message_count: 1,
            size: 1,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    fn full_message(o: &OrganizationId, pod: PodId, addr: &str) -> Message {
        Message {
            item: message_record(Some(o.clone()), Some(pod), addr),
            reply_to: None,
            text: None,
            html: None,
            extracted_text: None,
            extracted_html: None,
        }
    }

    fn full_thread(o: &OrganizationId, pod: PodId, addr: &str, messages: Vec<Message>) -> Thread {
        Thread { item: thread_record(o, pod, addr), messages }
    }

    /// The window for a mount the credential's own ids settle. Panics on a probe, so a test that
    /// expects a settled window cannot silently accept an unproven one.
    fn ready(scope: &Scope, mount: Mount) -> ScopeFilter {
        match scope.resolve(&mount).unwrap() {
            Resolved::Ready(f) => f,
            Resolved::Probe(_) => panic!("expected a settled window for {mount:?}, got a probe"),
        }
    }

    fn probe_for(scope: &Scope, mount: Mount) -> MountProbe {
        match scope.resolve(&mount).unwrap() {
            Resolved::Probe(p) => p,
            Resolved::Ready(_) => panic!("expected a probe for {mount:?}, got a settled window"),
        }
    }

    // ---- credential -> scope --------------------------------------------------------------

    #[test]
    fn live_org_identity_resolves_to_an_organization_scope() {
        let scope = Scope::from_identity(&org_identity()).unwrap();
        assert_eq!(scope.organization_id(), &org());
        assert!(matches!(scope, Scope::Organization { .. }));
    }

    #[test]
    fn an_identity_carrying_an_id_narrower_than_its_scope_type_is_unusable() {
        // openapi.json type_auth:Identity — pod_id "Present when scope_type is pod or inbox",
        // inbox_id "Present when scope_type is inbox". A wider scope_type carrying a narrower id
        // is as much a contradiction as a narrow one missing its id, and it is the dangerous
        // direction: read as "organization", a credential bound to ONE inbox reads the whole org.
        let pod = PodId::new_random();

        let mut id = org_identity();
        id.pod_id = Some(pod);
        assert_eq!(
            Scope::from_identity(&id),
            Err(ScopeResolutionError::NarrowerIdThanScope),
            "scope_type=organization with a pod_id must not widen to Scope::Organization"
        );

        let mut id = org_identity();
        id.inbox_id = Some(InboxId::new(INBOX));
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::NarrowerIdThanScope));

        // The exact combination the reviewers demonstrated reading the entire organization.
        let mut id = org_identity();
        id.pod_id = Some(pod);
        id.inbox_id = Some(InboxId::new(INBOX));
        assert!(Scope::from_identity(&id).is_err());

        let mut id = pod_identity(pod);
        id.inbox_id = Some(InboxId::new(INBOX));
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::NarrowerIdThanScope));
    }

    #[test]
    fn a_credential_whose_scope_id_disagrees_with_its_scope_is_unusable() {
        let mut id = pod_identity(PodId::new_random());
        id.scope_id = "not-the-pod".into();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::ScopeIdMismatch));

        let mut id = org_identity();
        id.scope_id = "some-other-org".into();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::ScopeIdMismatch));

        let mut id = inbox_identity(PodId::new_random(), INBOX);
        id.scope_id = OTHER_INBOX.into();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::ScopeIdMismatch));
    }

    #[test]
    fn a_scope_type_missing_the_id_that_defines_it_is_unusable() {
        let mut id = pod_identity(PodId::new_random());
        id.pod_id = None;
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::MissingPodId));

        // openapi.json: pod_id is "Present when scope_type is pod OR INBOX". Treating it as
        // optional for an inbox key lets that key match any pod in the organization.
        let mut id = inbox_identity(PodId::new_random(), INBOX);
        id.pod_id = None;
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::MissingPodId));

        let mut id = inbox_identity(PodId::new_random(), INBOX);
        id.inbox_id = None;
        id.scope_id = INBOX.into();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::MissingInboxId));
    }

    #[test]
    fn an_inbox_scope_always_pins_a_pod() {
        let pod = PodId::new_random();
        match Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap() {
            Scope::Inbox { pod_id, .. } => assert_eq!(pod_id, pod),
            other => panic!("expected an inbox scope, got {other:?}"),
        }
    }

    #[test]
    fn scope_id_is_compared_by_uuid_value_not_by_formatting() {
        // scope_id is a text column, pod_id is a UUID. Braces, a urn: prefix and uppercase are
        // all the SAME pod; rejecting them locks a key out of its own data.
        let pod = PodId::from(uuid::uuid!("9047724b-2879-416b-8424-82ef81ab9397"));
        for rendering in [
            "9047724b-2879-416b-8424-82ef81ab9397",
            "9047724B-2879-416B-8424-82EF81AB9397",
            "{9047724b-2879-416b-8424-82ef81ab9397}",
            "urn:uuid:9047724b-2879-416b-8424-82ef81ab9397",
            "9047724b2879416b842482ef81ab9397",
        ] {
            let mut id = pod_identity(pod);
            id.scope_id = rendering.into();
            assert!(
                Scope::from_identity(&id).is_ok(),
                "{rendering} names the same pod and must resolve"
            );
        }
        // A different UUID, and a non-UUID, both still fail.
        let mut id = pod_identity(pod);
        id.scope_id = PodId::new_random().to_string();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::ScopeIdMismatch));
        let mut id = pod_identity(pod);
        id.scope_id = "9047724b-2879-416b-8424".into();
        assert_eq!(Scope::from_identity(&id), Err(ScopeResolutionError::ScopeIdMismatch));
    }

    #[test]
    fn an_inbox_scope_id_is_compared_case_insensitively() {
        // Fixture 18: the live API resolves AMKCASE@… and AmKcAsE@… to the same inbox, so a
        // scope_id recorded in the caller's original casing still names the bound inbox.
        let mut id = inbox_identity(PodId::new_random(), "amkcase@agentmail.to");
        id.scope_id = "AmKcAsE@agentmail.to".into();
        assert!(Scope::from_identity(&id).is_ok());
    }

    #[test]
    fn an_unresolvable_credential_surfaces_as_the_bare_auth_layer_body() {
        // fixture 05-error-catalog.http: the auth layer answers {"message":"Forbidden"} — the
        // bare gateway shape, never the app envelope.
        let mut id = pod_identity(PodId::new_random());
        id.pod_id = None;
        let err = Scope::from_identity(&id).unwrap_err();
        assert_eq!(
            serde_json::to_string(&err.gateway_body()).unwrap(),
            r#"{"message":"Forbidden"}"#
        );
    }

    // ---- the critical rule: denial is masked as not_found ----------------------------------

    #[test]
    fn pod_scoped_key_reaching_an_inbox_in_another_pod_is_not_found_never_forbidden() {
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let foreign = inbox_record(&org(), theirs, OTHER_INBOX);
        let denial = filter.check(foreign).unwrap_err();

        assert_eq!(denial.status(), 404, "a scope denial is a filter, not a rejection");
        let env = denial.into_envelope();
        assert_eq!(env.code, ErrorCode::NotFound);
        assert_eq!(env.status(), 404);
        assert_eq!(env.name, "NotFoundError");
        assert_eq!(env.message, "Inbox not found");
        assert_ne!(env.status(), 403);
    }

    #[test]
    fn every_denial_is_not_found_whatever_the_resource() {
        for kind in ResourceKind::ALL {
            let denial = ScopeDenial::new(kind);
            assert_eq!(denial.status(), 404, "{kind:?}");
            let env = denial.into_envelope();
            assert_eq!(env.code, ErrorCode::NotFound, "{kind:?} must mask as not_found");
            assert_eq!(env.status(), 404, "{kind:?}");
            assert!(env.suggestions.is_empty(), "a mask must not hint at what it hid");
            assert!(env.errors.is_empty());
        }
    }

    #[test]
    fn the_denial_body_is_identical_for_hidden_and_absent_resources() {
        // Two genuinely different code paths: a row that EXISTS but is outside the window, and a
        // row the store never returned. Their bodies must be byte-identical.
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let hidden = filter
            .check(inbox_record(&org(), theirs, OTHER_INBOX))
            .unwrap_err()
            .into_envelope();
        let absent = filter.not_found(ResourceKind::Inbox).into_envelope();

        assert_eq!(
            serde_json::to_value(&hidden).unwrap(),
            serde_json::to_value(&absent).unwrap(),
            "hidden and absent must be the same answer"
        );
        // And the same again for a probe: a foreign mount and an absent mount both leave here.
        let hidden_mount = probe_for(&scope, Mount::Inbox(InboxId::new(OTHER_INBOX)))
            .settle(Some(inbox_record(&org(), theirs, OTHER_INBOX)))
            .unwrap_err()
            .into_envelope();
        let absent_mount = probe_for(&scope, Mount::Inbox(InboxId::new(OTHER_INBOX)))
            .settle(None::<Inbox>)
            .unwrap_err()
            .into_envelope();
        assert_eq!(
            serde_json::to_value(&hidden_mount).unwrap(),
            serde_json::to_value(&absent_mount).unwrap()
        );
    }

    #[test]
    fn the_mask_fix_reproduces_the_live_capture() {
        // One rendering, anchored on the longest live capture (fixture 09b:90-91). The head
        // clause is the one fixture 05:33-35 shows instead.
        let env = ScopeDenial::new(ResourceKind::Message).into_envelope();
        let fix = env.fix.unwrap();
        assert!(fix.ends_with(CAPTURED_FIX_TAIL), "fix must end with the 09b capture: {fix}");
        assert!(
            fix.contains("scope (organization, pod, or inbox)"),
            "fix must carry the scope half captured in fixture 05: {fix}"
        );
    }

    // ---- cross-organization at all three mounts --------------------------------------------

    #[test]
    fn cross_org_access_is_masked_at_all_three_mounts() {
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&org_identity()).unwrap();

        for mount in [
            Mount::Organization,
            Mount::Pod(pod),
            Mount::Inbox(InboxId::new(INBOX)),
        ] {
            let resolved = scope.resolve(&mount).unwrap();
            let filter = resolved.window();
            // Every window pins the credential's organization, so a store query built from it
            // can never see another org's rows.
            assert_eq!(filter.organization_id(), &org(), "{mount:?}");

            let foreign = inbox_record(&other_org(), pod, INBOX);
            assert!(filter.check(foreign).is_err(), "{mount:?} leaked across organizations");
        }
    }

    #[test]
    fn cross_org_is_masked_even_when_pod_and_inbox_ids_collide() {
        // Same pod id and same inbox address in a different organization: only the org differs.
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&pod_identity(pod)).unwrap();
        let filter = ready(&scope, Mount::Pod(pod));

        assert!(filter
            .check(inbox_record(&other_org(), pod, INBOX))
            .is_err());
        assert!(filter.check(inbox_record(&org(), pod, INBOX)).is_ok(), "same org must pass");
    }

    // ---- mount x scope intersection --------------------------------------------------------

    #[test]
    fn inbox_scoped_key_on_the_org_mount_is_filtered_not_rejected() {
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();

        // The org mount is reachable — it names no resource the credential cannot see.
        let filter = ready(&scope, Mount::Organization);
        assert_eq!(filter.inbox_id(), Some(&InboxId::new(INBOX)), "narrowed to its own inbox");

        assert!(filter.check(inbox_record(&org(), pod, INBOX)).is_ok());
        assert!(
            filter
                .check(inbox_record(&org(), pod, OTHER_INBOX))
                .is_err(),
            "a sibling inbox in the same pod must be invisible"
        );
    }

    #[test]
    fn a_pod_key_on_an_inbox_mount_must_probe_before_a_collection_is_served() {
        // The regression: the mount is accepted (an address alone proves nothing about which pod
        // holds it), so returning a plain window here made
        // GET /v0/inboxes/foreign@x/threads answer 200 {"count":0} — a different answer from the
        // absent case, which is exactly the distinction the masking rule forbids.
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();

        let probe = probe_for(&scope, Mount::Inbox(InboxId::new(OTHER_INBOX)));
        assert_eq!(probe.kind(), ResourceKind::Inbox);
        assert_eq!(probe.window().pod_id(), Some(&mine), "the probing lookup stays pinned");

        // Foreign pod -> masked.
        assert!(probe
            .settle(Some(inbox_record(&org(), theirs, OTHER_INBOX)))
            .is_err());
        // Absent -> masked identically.
        let probe = probe_for(&scope, Mount::Inbox(InboxId::new(OTHER_INBOX)));
        assert!(probe.settle(None::<Inbox>).is_err());
        // Its own pod -> settled, and the window it yields is the one it probed in.
        let probe = probe_for(&scope, Mount::Inbox(InboxId::new(INBOX)));
        let window = probe.window().clone();
        let settled = probe
            .settle(Some(inbox_record(&org(), mine, INBOX)))
            .unwrap();
        assert_eq!(settled, window);
    }

    #[test]
    fn an_org_key_must_probe_both_the_pod_and_the_inbox_mounts() {
        // The credential proves its organization and nothing narrower, so /v0/pods/{p}/… and
        // /v0/inboxes/{i}/… name resources whose existence is still unproven.
        let scope = Scope::from_identity(&org_identity()).unwrap();
        let pod = PodId::new_random();

        let probe = probe_for(&scope, Mount::Pod(pod));
        assert_eq!(probe.kind(), ResourceKind::Pod);
        assert!(probe.settle(None::<Pod>).is_err(), "an absent pod mount masks");
        assert!(probe_for(&scope, Mount::Pod(pod))
            .settle(Some(pod_record(&other_org(), pod)))
            .is_err());
        assert!(probe_for(&scope, Mount::Pod(pod))
            .settle(Some(pod_record(&org(), pod)))
            .is_ok());

        let probe = probe_for(&scope, Mount::Inbox(InboxId::new(INBOX)));
        assert_eq!(probe.kind(), ResourceKind::Inbox);
        assert!(probe
            .settle(Some(inbox_record(&other_org(), pod, INBOX)))
            .is_err());
    }

    #[test]
    fn a_credential_proves_the_resources_it_is_scoped_to_and_no_others() {
        // No probe where the credential itself is the proof; a probe everywhere else.
        let pod = PodId::new_random();
        let inbox_scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();
        let pod_scope = Scope::from_identity(&pod_identity(pod)).unwrap();
        let org_scope = Scope::from_identity(&org_identity()).unwrap();

        for (scope, mount) in [
            (&org_scope, Mount::Organization),
            (&pod_scope, Mount::Organization),
            (&pod_scope, Mount::Pod(pod)),
            (&inbox_scope, Mount::Organization),
            (&inbox_scope, Mount::Pod(pod)),
            (&inbox_scope, Mount::Inbox(InboxId::new(INBOX))),
        ] {
            assert!(
                matches!(scope.resolve(&mount).unwrap(), Resolved::Ready(_)),
                "{scope:?} on {mount:?} names nothing it does not already prove"
            );
        }
    }

    #[test]
    fn a_pod_mount_naming_a_pod_outside_the_credential_is_not_found() {
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();

        let denial = scope.resolve(&Mount::Pod(theirs)).unwrap_err();
        assert_eq!(denial.into_envelope().code, ErrorCode::NotFound);
        // ... and one unit the other side of the boundary: its own pod resolves.
        assert!(scope.resolve(&Mount::Pod(mine)).is_ok());
    }

    #[test]
    fn an_inbox_mount_naming_another_inbox_is_not_found_for_an_inbox_key() {
        let scope = Scope::from_identity(&inbox_identity(PodId::new_random(), INBOX)).unwrap();
        assert!(scope
            .resolve(&Mount::Inbox(InboxId::new(OTHER_INBOX)))
            .is_err());
        assert!(scope.resolve(&Mount::Inbox(InboxId::new(INBOX))).is_ok());
        // Fixture 18: a differently-cased path parameter names the same inbox.
        assert!(scope
            .resolve(&Mount::Inbox(InboxId::new(INBOX.to_uppercase())))
            .is_ok());
    }

    #[test]
    fn an_inbox_key_on_a_pod_mount_outside_its_pod_is_not_found() {
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&inbox_identity(mine, INBOX)).unwrap();
        assert!(scope.resolve(&Mount::Pod(theirs)).is_err());
        assert!(scope.resolve(&Mount::Pod(mine)).is_ok());
    }

    #[test]
    fn org_key_on_a_pod_mount_cannot_reach_a_resource_in_a_different_pod() {
        // The mount is part of the window: /v0/pods/{A}/threads must not answer with a thread
        // that lives in pod B, even for an organization-scoped credential.
        let (a, b) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&org_identity()).unwrap();
        let filter = probe_for(&scope, Mount::Pod(a))
            .settle(Some(pod_record(&org(), a)))
            .unwrap();

        assert!(filter.check(thread_record(&org(), b, INBOX)).is_err());
        assert!(filter.check(thread_record(&org(), a, INBOX)).is_ok());
    }

    #[test]
    fn org_key_reaches_every_pod_and_inbox_in_its_own_organization() {
        let scope = Scope::from_identity(&org_identity()).unwrap();
        let filter = ready(&scope, Mount::Organization);
        assert_eq!(filter.pod_id(), None);
        assert_eq!(filter.inbox_id(), None);

        for pod in [PodId::new_random(), PodId::new_random()] {
            assert!(filter.check(inbox_record(&org(), pod, INBOX)).is_ok());
            assert!(filter.check(pod_record(&org(), pod)).is_ok());
        }
    }

    #[test]
    fn pod_key_on_the_org_mount_sees_only_its_own_pod() {
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();
        let filter = ready(&scope, Mount::Organization);
        assert_eq!(filter.pod_id(), Some(&mine), "the org mount is narrowed, not opened");
        assert!(filter.check(pod_record(&org(), theirs)).is_err());
        assert!(filter.check(pod_record(&org(), mine)).is_ok());
    }

    #[test]
    fn an_inbox_key_cannot_read_its_parent_pod() {
        // The pod object carries organization-wide context (name, client_id); an inbox-scoped
        // credential is narrower than any pod, so the pod itself is masked.
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();
        let filter = ready(&scope, Mount::Organization);
        assert!(filter.check(pod_record(&org(), pod)).is_err());
    }

    // ---- no leakage through counts, pagination or thread membership ------------------------

    #[test]
    fn retain_visible_drops_foreign_rows_so_counts_cannot_leak() {
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let page = vec![
            thread_record(&org(), mine, INBOX),
            thread_record(&org(), theirs, OTHER_INBOX),
            thread_record(&other_org(), mine, INBOX),
        ];
        let raw_len = page.len();
        let visible = filter.retain_visible(page);
        assert_eq!(visible.len(), 1, "scope-foreign rows must not survive");
        assert_ne!(visible.len(), raw_len);
    }

    #[test]
    fn a_thread_is_masked_when_any_of_its_messages_is_outside_the_window() {
        // Threads never span inboxes (fixture 16), so a straddling thread is a storage defect.
        // Masking it keeps `message_count` and `size` the store's own numbers; silently dropping
        // messages would serve a body that never existed and hide the defect.
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let clean = full_thread(&org(), pod, INBOX, vec![full_message(&org(), pod, INBOX)]);
        assert!(filter.check_thread(clean).is_ok());

        let straddling = full_thread(
            &org(),
            pod,
            INBOX,
            vec![
                full_message(&org(), pod, INBOX),
                full_message(&org(), pod, OTHER_INBOX),
            ],
        );
        let denial = filter.check_thread(straddling).unwrap_err();
        assert_eq!(denial.kind(), ResourceKind::Thread);

        // The thread's own coordinates being foreign is masked too.
        let foreign = full_thread(&org(), pod, OTHER_INBOX, vec![]);
        assert!(filter.check_thread(foreign).is_err());
    }

    #[test]
    fn thread_membership_cannot_carry_messages_from_another_inbox() {
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let messages = vec![
            message_record(Some(org()), Some(pod), INBOX),
            message_record(Some(org()), Some(pod), OTHER_INBOX),
        ];
        assert_eq!(filter.retain_visible(messages).len(), 1);
    }

    // ---- fail-closed on unknown coordinates -------------------------------------------------

    #[test]
    fn a_row_with_unknown_coordinates_is_masked_not_admitted() {
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&pod_identity(pod)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        // organization unknown -> masked (never "matches everything")
        assert!(filter
            .check(message_record(None, Some(pod), INBOX))
            .is_err());
        // pod unknown, while the filter pins a pod -> masked
        assert!(filter
            .check(message_record(Some(org()), None, INBOX))
            .is_err());
        // both known and matching -> visible
        assert!(filter
            .check(message_record(Some(org()), Some(pod), INBOX))
            .is_ok());
    }

    #[test]
    fn check_at_masks_a_row_outside_the_window_and_names_the_kind() {
        // Crate-private until the remaining ResourceKinds have wire types; still exercised, so
        // the day it reopens it is not reopening untested.
        let (mine, theirs) = (PodId::new_random(), PodId::new_random());
        let scope = Scope::from_identity(&pod_identity(mine)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        let inside = ResourceScope::pod(org(), mine);
        assert_eq!(filter.check_at(ResourceKind::Draft, &inside, 7), Ok(7));

        let outside = ResourceScope::pod(org(), theirs);
        let denial = filter
            .check_at(ResourceKind::Draft, &outside, 7)
            .unwrap_err();
        assert_eq!(denial.kind(), ResourceKind::Draft);
        assert_eq!(denial.into_envelope().message, "Draft not found");

        // Empty coordinates fail closed rather than matching the pinned organization.
        assert!(filter
            .check_at(ResourceKind::Webhook, &ResourceScope::default(), 7)
            .is_err());
    }

    #[test]
    fn resource_scope_constructors_pin_exactly_what_they_name() {
        let (o, pod) = (org(), PodId::new_random());
        let i = InboxId::new(INBOX);

        let s = ResourceScope::organization(o.clone());
        assert_eq!((s.organization_id.as_ref(), s.pod_id, s.inbox_id), (Some(&o), None, None));

        let s = ResourceScope::pod(o.clone(), pod);
        assert_eq!((s.organization_id.as_ref(), s.pod_id, s.inbox_id), (Some(&o), Some(pod), None));

        let s = ResourceScope::inbox(o.clone(), pod, i.clone());
        assert_eq!(
            (s.organization_id.as_ref(), s.pod_id, s.inbox_id),
            (Some(&o), Some(pod), Some(i))
        );

        // An org-only coordinate is admitted by an org-only window and masked by a narrower one.
        let org_scope = Scope::from_identity(&org_identity()).unwrap();
        let org_window = ready(&org_scope, Mount::Organization);
        assert!(org_window
            .check_at(ResourceKind::Organization, &ResourceScope::organization(o.clone()), ())
            .is_ok());

        let pod_scope = Scope::from_identity(&pod_identity(pod)).unwrap();
        let pod_window = ready(&pod_scope, Mount::Organization);
        assert!(pod_window
            .check_at(ResourceKind::Organization, &ResourceScope::organization(o), ())
            .is_err());
    }

    #[test]
    fn inbox_matching_folds_case_per_fixture_18() {
        // Live probe (reference/fixtures/18-inbox-case-normalization.txt): {"username":"AmkCase"}
        // is stored and returned as amkcase@agentmail.to, and GET resolves AMKCASE@ and AmKcAsE@
        // with 200. Exact matching would therefore diverge from upstream on exactly the input a
        // caller controls, so this asserts case-INsensitivity outright — no PartialEq round-trip
        // that would pass whichever way the id type behaved.
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&inbox_identity(pod, INBOX)).unwrap();
        let filter = ready(&scope, Mount::Organization);

        assert_eq!(
            filter.inbox_id(),
            Some(&InboxId::new(INBOX.to_lowercase())),
            "the pinned id is the stored, lowercased form"
        );
        for variant in [INBOX.to_uppercase(), "AmK-PrObE@AgentMail.To".to_owned()] {
            assert!(
                filter.check(inbox_record(&org(), pod, &variant)).is_ok(),
                "{variant} is the same inbox and must be visible"
            );
        }
        // Folding case must not merge distinct addresses.
        assert!(filter
            .check(inbox_record(&org(), pod, OTHER_INBOX))
            .is_err());
    }

    #[test]
    fn checking_moves_the_value_so_a_denied_row_cannot_be_used() {
        // Compile-level property: check() consumes the row and only hands it back when visible.
        let pod = PodId::new_random();
        let scope = Scope::from_identity(&pod_identity(pod)).unwrap();
        let filter = ready(&scope, Mount::Organization);
        let row = inbox_record(&org(), pod, INBOX);
        let back = filter.check(row).expect("visible");
        assert_eq!(back.inbox_id, InboxId::new(INBOX));
    }
}
