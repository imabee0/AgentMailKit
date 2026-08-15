//! Thread assignment for inbound mail.
//!
//! # The rule (observed, not assumed)
//!
//! `reference/fixtures/16-threading-matrix/summary.txt` — 18 crafted messages produced **17
//! distinct threads**. The only pair that merged was `a1`/`a2`, a reply carrying `In-Reply-To`
//! + `References` naming the root's Message-ID.
//!
//! * **R1** A message joins an existing thread *only* when it carries `In-Reply-To` and/or
//!   `References` pointing at a Message-ID already in that inbox's thread
//!   (`.../16-threading-matrix/a.txt`).
//! * **R2** Subject is **not** a grouping key. Identical subjects, `Re:`/`RE:`/`Fwd:`/`FW:`/
//!   `AW:`/`[list] ` prefixes, trailing whitespace, exact duplicates and empty subjects each
//!   opened their own thread (`b.txt`, `c.txt`, `d.txt`, `f.txt`). There is **no** subject
//!   normalisation fallback here — an earlier design assumed a subject+correspondent fallback
//!   and the matrix killed it. Do not reintroduce it.
//! * **R3** Correspondent identity is neither sufficient nor necessary (`b.txt`, `c.txt`).
//! * **R4** Threads are **per-inbox** and never span inboxes, even for identical mail to a
//!   second inbox in the same pod (`e.txt`). The inbox is therefore part of every lookup key —
//!   and that key folds ASCII case, because the live API resolves an inbox case-insensitively
//!   (`reference/fixtures/18-inbox-case-normalization.txt`; see [`InboxId::eq_normalized`]).
//!
//! # Why a trait
//!
//! The matrix leaves dimensions undetermined (its own "honest gaps": `In-Reply-To` vs
//! `References` in isolation, multi-hop chains, references into a *different* existing thread,
//! and authenticated mail — every probe was unauthenticated). [`ThreadAssigner`] exists so
//! those can change behind a swapped implementation without touching callers.
//! [`ReferenceChainThreading`] is the default: the observed rule, and — where the matrix is
//! silent — the choice that never merges two existing threads and never re-parents ordinary
//! mail. Every such choice is marked **UNDETERMINED** below.
//!
//! # Purity
//!
//! "Which thread contains this Message-ID, in this inbox" is storage's question, abstracted as
//! [`ThreadIndex`]. This module performs no I/O and mints no identifiers: a `New` outcome tells
//! the caller to create a thread, it does not invent the [`ThreadId`].

use amk_types::{InboxId, MessageId, MessageItem, ThreadId};
use std::collections::{BTreeMap, BTreeSet};

/// Storage's answer to "does this inbox already hold a message with this Message-ID, and if so
/// which thread is it in?".
///
/// Per **R4** the inbox is part of the key: an implementation must never answer from another
/// inbox's mail, even within the same pod. It must also compare inbox ids with
/// [`InboxId::eq_normalized`] / [`InboxId::normalized`] rather than `==` on the raw id — the
/// live API resolves `AMKCASE@…` and `amkcase@…` to one inbox
/// (`reference/fixtures/18-inbox-case-normalization.txt`), so an exact-match index would split
/// one inbox's threads in two on a spelling the sender chooses.
pub trait ThreadIndex {
    fn thread_of(&self, inbox_id: &InboxId, message_id: &MessageId) -> Option<ThreadId>;
}

/// The linkage headers of a message awaiting a thread, plus the inbox it was delivered to.
///
/// Deliberately carries **no subject and no correspondent** — neither is a grouping key (R2,
/// R3), so neither is available to be accidentally consulted.
#[derive(Debug, Clone, Copy)]
pub struct ThreadCandidate<'a> {
    /// The delivery inbox. Scopes every lookup (R4).
    pub inbox_id: &'a InboxId,
    /// The message's own Message-ID, when it has one. Used only to ignore self-references.
    pub message_id: Option<&'a MessageId>,
    pub in_reply_to: Option<&'a MessageId>,
    pub references: &'a [MessageId],
}

impl<'a> ThreadCandidate<'a> {
    pub fn new(inbox_id: &'a InboxId) -> Self {
        Self { inbox_id, message_id: None, in_reply_to: None, references: &[] }
    }

    pub fn with_message_id(mut self, message_id: &'a MessageId) -> Self {
        self.message_id = Some(message_id);
        self
    }

    pub fn with_in_reply_to(mut self, in_reply_to: &'a MessageId) -> Self {
        self.in_reply_to = Some(in_reply_to);
        self
    }

    pub fn with_references(mut self, references: &'a [MessageId]) -> Self {
        self.references = references;
        self
    }

    /// Re-derive the candidate from a stored message — used when threading has to be recomputed
    /// (e.g. an import re-deriving grouping rather than trusting a foreign one).
    pub fn from_message_item(item: &'a MessageItem) -> Self {
        Self {
            inbox_id: &item.inbox_id,
            message_id: Some(&item.message_id),
            in_reply_to: item.in_reply_to.as_ref(),
            references: item.references.as_deref().unwrap_or(&[]),
        }
    }
}

/// Why a message did not join an existing thread. Diagnostic only — all three variants have the
/// same effect: the caller creates a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewThreadReason {
    /// No usable `In-Reply-To`/`References` link. This is the overwhelmingly common case in the
    /// matrix: **17 of the 18** probes (a1, b, c0–c7, d0–d2, e1, e2, f1, f2) carried no linkage
    /// header at all — a2 is the single exception, and the single merge.
    NoLinkage,
    /// Links were present but none names a Message-ID this inbox holds — including a link into
    /// *another* inbox's mail (R4).
    UnknownLinkage,
    /// **UNDETERMINED** (matrix gap G2). `References` alone resolves to more than one existing
    /// thread, with no `In-Reply-To` to break the tie. We do not merge the threads and we do not
    /// silently pick a winner; the message opens its own thread. This is a chosen reading, not
    /// an observed behaviour — the matrix never produced a cross-thread reference, so upstream's
    /// real answer is unknown.
    AmbiguousLinkage,
}

/// The outcome of thread assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadAssignment {
    /// Join this existing thread. `linked_by` is the referenced Message-ID that resolved it.
    Existing {
        thread_id: ThreadId,
        linked_by: MessageId,
    },
    /// Create a new thread; the caller mints the [`ThreadId`].
    New(NewThreadReason),
}

impl ThreadAssignment {
    pub fn thread_id(&self) -> Option<ThreadId> {
        match self {
            Self::Existing { thread_id, .. } => Some(*thread_id),
            Self::New(_) => None,
        }
    }

    pub fn is_new(&self) -> bool {
        matches!(self, Self::New(_))
    }
}

/// Decides which thread an inbound message belongs to.
///
/// Object-safe on purpose: callers hold a `&dyn ThreadAssigner` so the rule can be replaced when
/// one of the matrix's undetermined dimensions is settled.
pub trait ThreadAssigner {
    fn assign(&self, index: &dyn ThreadIndex, candidate: &ThreadCandidate<'_>) -> ThreadAssignment;
}

/// The observed rule: strict RFC Message-ID reference-chain linkage, scoped per inbox.
///
/// Subject and correspondent are not consulted — they are not even reachable from
/// [`ThreadCandidate`].
///
/// # Precedence: `In-Reply-To` decides, `References` only advises
///
/// `In-Reply-To` names the direct parent; `References` is an ancestry *list* that routinely
/// survives a forward and can therefore name a message sitting in some other local thread.
/// So a resolvable `In-Reply-To` wins outright, and `References` is consulted only when
/// `In-Reply-To` is absent or names nothing this inbox holds. Case (a) sent both together, so
/// which one alone suffices is **UNDETERMINED** (matrix gap G1); R1 is stated as "In-Reply-To
/// and/or References", so either is honoured, and the precedence between them is our choice.
///
/// The alternative — treating any disagreement between the two headers as ambiguity — opens a
/// brand-new thread for an ordinary reply whose `References` happens to reach into another
/// thread, which is worse for real mail and no safer: neither policy ever merges two threads.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceChainThreading;

impl ThreadAssigner for ReferenceChainThreading {
    fn assign(&self, index: &dyn ThreadIndex, candidate: &ThreadCandidate<'_>) -> ThreadAssignment {
        let own = candidate.message_id.and_then(canonical_link);
        // A message referencing itself cannot join its own not-yet-existing thread, and a
        // redelivery of an id this inbox already holds is storage's dedup problem, not a
        // grouping signal. **UNDETERMINED** (matrix gap G6: duplicate Message-IDs untested).
        let usable = |raw: &MessageId| -> Option<MessageId> {
            canonical_link(raw).filter(|link| own.as_ref() != Some(link))
        };

        // Whether any link worth resolving was carried — the only thing separating "no linkage
        // headers" from "linkage headers that name nothing we hold".
        let mut had_link = false;

        // 1. `In-Reply-To`: the direct parent. If it resolves, that is the answer.
        if let Some(link) = candidate.in_reply_to.and_then(&usable) {
            had_link = true;
            if let Some(thread_id) = index.thread_of(candidate.inbox_id, &link) {
                return ThreadAssignment::Existing { thread_id, linked_by: link };
            }
        }

        // 2. `References`, only now. Every entry carried is examined — but we never walk from a
        // referenced message on to *its* references: multi-hop transitivity is UNDETERMINED
        // (gap G1), and a real chain lists its ancestors in `References` anyway. The loop reads
        // one finite list, so it has nothing to terminate; `visited` exists to keep a repeated
        // entry from being resolved twice, not to bound the walk.
        let mut visited: BTreeSet<MessageId> = BTreeSet::new();
        let mut resolved: Option<(ThreadId, MessageId)> = None;
        let mut ambiguous = false;

        for raw in candidate.references {
            let Some(link) = usable(raw) else {
                continue;
            };
            had_link = true;
            if !visited.insert(link.clone()) {
                continue;
            }
            if let Some(thread_id) = index.thread_of(candidate.inbox_id, &link) {
                match &resolved {
                    None => resolved = Some((thread_id, link)),
                    Some((seen, _)) if *seen == thread_id => {}
                    Some(_) => ambiguous = true,
                }
            }
        }

        if ambiguous {
            return ThreadAssignment::New(NewThreadReason::AmbiguousLinkage);
        }
        match resolved {
            Some((thread_id, linked_by)) => ThreadAssignment::Existing { thread_id, linked_by },
            None if had_link => ThreadAssignment::New(NewThreadReason::UnknownLinkage),
            None => ThreadAssignment::New(NewThreadReason::NoLinkage),
        }
    }
}

/// Normalise a linkage header value for comparison against stored Message-IDs.
///
/// Only surrounding folding whitespace is stripped — RFC 5322 permits FWS around the `msg-id`,
/// so `"\t<a@b> "` and `"<a@b>"` are the same header value. Nothing else is rewritten.
///
/// **Not** re-bracketed. Stored `message_id`s are angle-bracketed
/// (`reference/fixtures/03-id-formats.http`), but that fixture establishes the form of the
/// *stored id*, not that an unbracketed `In-Reply-To: root@probe.test` should be coerced into
/// `<root@probe.test>`. No fixture shows an unbracketed linkage header at all, and coercion is
/// the one path by which a message could join a thread upstream would have left alone — a
/// sender chooses that header, so the lenient reading is attacker-reachable. Strict wins.
///
/// **[INFERRED]** the cost: a sloppy real-world sender that omits the brackets will
/// under-thread (its reply opens a new thread) rather than mis-thread. A swaks probe sending
/// `In-Reply-To: <bare-id-without-brackets>` to a probe inbox would settle it outright — we
/// control a sender, so this gap is closable whenever the orchestrator wants it closed.
///
/// Comparison is otherwise **byte-exact** — no case folding: RFC 5322's `id-left` is
/// case-sensitive, and the matrix says nothing about malformed ids (gap G6). Blank and `<>`
/// values carry no linkage and are dropped.
fn canonical_link(id: &MessageId) -> Option<MessageId> {
    let trimmed = id.as_str().trim();
    if trimmed.is_empty() || trimmed == "<>" {
        return None;
    }
    Some(MessageId::new(trimmed))
}

/// An in-memory [`ThreadIndex`], for tests and for callers that need to thread a batch before
/// anything is persisted. Message-ID keys are normalised exactly as [`ReferenceChainThreading`]
/// normalises links, so the fake cannot drift from the rule it is used to exercise, and inbox
/// keys are [`InboxId::normalized`] so one inbox spelled two ways is one index (R4).
#[derive(Debug, Clone, Default)]
pub struct InMemoryThreadIndex {
    entries: BTreeMap<(InboxId, MessageId), ThreadId>,
}

impl InMemoryThreadIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `inbox_id` holds `message_id` in `thread_id`. A blank Message-ID is not
    /// indexable and is ignored.
    pub fn insert(&mut self, inbox_id: InboxId, message_id: &MessageId, thread_id: ThreadId) {
        if let Some(key) = canonical_link(message_id) {
            self.entries.insert((inbox_id.normalized(), key), thread_id);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ThreadIndex for InMemoryThreadIndex {
    fn thread_of(&self, inbox_id: &InboxId, message_id: &MessageId) -> Option<ThreadId> {
        let key = canonical_link(message_id)?;
        self.entries.get(&(inbox_id.normalized(), key)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "amk-probe@agentmail.to";
    const PROBE2: &str = "amk-probe2@agentmail.to";

    /// Delivers messages the way ingest will: assign, then index the result. Mints a fresh
    /// ThreadId on `New`, which is exactly the caller's job.
    #[derive(Default)]
    struct Ingest {
        index: InMemoryThreadIndex,
        /// One entry per delivery that opened a new thread, in delivery order.
        new_reasons: Vec<NewThreadReason>,
    }

    impl Ingest {
        /// Returns the thread the message landed in.
        fn deliver(
            &mut self,
            inbox: &str,
            message_id: &str,
            in_reply_to: Option<&str>,
            references: &[&str],
        ) -> ThreadId {
            let inbox_id = InboxId::new(inbox);
            let mid = MessageId::new(message_id);
            let irt = in_reply_to.map(MessageId::new);
            let refs: Vec<MessageId> = references.iter().map(|r| MessageId::new(*r)).collect();

            let mut candidate = ThreadCandidate::new(&inbox_id)
                .with_message_id(&mid)
                .with_references(&refs);
            if let Some(irt) = irt.as_ref() {
                candidate = candidate.with_in_reply_to(irt);
            }

            let thread_id = match ReferenceChainThreading.assign(&self.index, &candidate) {
                ThreadAssignment::Existing { thread_id, .. } => thread_id,
                ThreadAssignment::New(reason) => {
                    self.new_reasons.push(reason);
                    ThreadId::new_random()
                }
            };
            self.index.insert(inbox_id, &mid, thread_id);
            thread_id
        }

        /// A message with no linkage headers at all.
        fn deliver_plain(&mut self, inbox: &str, message_id: &str) -> ThreadId {
            self.deliver(inbox, message_id, None, &[])
        }
    }

    fn assign_with(
        index: &InMemoryThreadIndex,
        candidate: &ThreadCandidate<'_>,
    ) -> ThreadAssignment {
        ReferenceChainThreading.assign(index, candidate)
    }

    // ---------------------------------------------------------------------------------------
    // The matrix, replayed. reference/fixtures/16-threading-matrix/{summary,a,b,c,d,e,f}.txt
    // ---------------------------------------------------------------------------------------

    /// 18 messages in, 17 distinct threads out; the only merge is the a1/a2 In-Reply-To pair.
    /// Message-IDs, subjects-per-case and inboxes are the fixture's own. The summary's
    /// "In-Reply-To/Refs" column reads `no` for every row but a2, so 17 of the 18 deliveries
    /// must land on `NoLinkage`.
    /// reference/fixtures/16-threading-matrix/summary.txt
    #[test]
    fn matrix_replays_to_seventeen_threads_with_only_the_reply_pair_merged() {
        let mut ingest = Ingest::default();

        // (a) control: reply carries In-Reply-To AND References -> root. a.txt
        let a1 = ingest.deliver_plain(PROBE, "<a-root-d64ee47e@probe.test>");
        let a2 = ingest.deliver(
            PROBE,
            "<a-reply-d64ee47e@probe.test>",
            Some("<a-root-d64ee47e@probe.test>"),
            &["<a-root-d64ee47e@probe.test>"],
        );
        // (b) subject identical to a1, different From, no linkage. b.txt
        let b = ingest.deliver_plain(PROBE, "<b-d64ee47e@other.test>");
        // (c) subject-prefix variants, same sender, no linkage. c.txt
        let c: Vec<ThreadId> = (0..8)
            .map(|i| ingest.deliver_plain(PROBE, &format!("<c{i}-d64ee47e@probe.test>")))
            .collect();
        // (d) exact-duplicate and near-identical subjects, same sender, no linkage. d.txt
        let d: Vec<ThreadId> = (0..3)
            .map(|i| ingest.deliver_plain(PROBE, &format!("<d{i}-d64ee47e@probe.test>")))
            .collect();
        // (e) identical mail to two inboxes in one pod. e.txt
        let e1 = ingest.deliver_plain(PROBE, "<e1-d64ee47e@probe.test>");
        let e2 = ingest.deliver_plain(PROBE2, "<e2-d64ee47e@probe.test>");
        // (f) empty subject, twice, same sender. f.txt
        let f1 = ingest.deliver_plain(PROBE, "<f1-d64ee47e@probe.test>");
        let f2 = ingest.deliver_plain(PROBE, "<f2-d64ee47e@probe.test>");

        assert_eq!(a1, a2, "a1/a2: the In-Reply-To pair is the one observed merge");

        let all: Vec<ThreadId> = [a1, a2, b]
            .into_iter()
            .chain(c.iter().copied())
            .chain(d.iter().copied())
            .chain([e1, e2, f1, f2])
            .collect();
        assert_eq!(all.len(), 18, "the matrix sent 18 messages");
        let distinct: BTreeSet<ThreadId> = all.iter().copied().collect();
        assert_eq!(distinct.len(), 17, "18 messages -> 17 distinct threads");

        // Every non-(a) case is its own thread — pairwise, not merely "not equal to a1".
        let singles = [
            b, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], d[0], d[1], d[2], e1, e2, f1, f2,
        ];
        let unique: BTreeSet<ThreadId> = singles.iter().copied().collect();
        assert_eq!(unique.len(), singles.len(), "each unlinked message opens its own thread");
        assert!(!unique.contains(&a1), "no unlinked message joined the a1/a2 thread");

        // The recount the doc on NoLinkage cites: a1 is unlinked too, so it is 17, not 16.
        assert_eq!(
            ingest.new_reasons,
            vec![NewThreadReason::NoLinkage; 17],
            "every probe but a2 carried no linkage header (summary.txt column In-Reply-To/Refs)"
        );
    }

    /// Subject is not even an input, so the closest observable assertion is that two messages
    /// differing in nothing an implementation could see still separate. b/c/d/f.txt
    #[test]
    fn identical_messages_without_linkage_never_group() {
        let mut ingest = Ingest::default();
        let d0 = ingest.deliver_plain(PROBE, "<d0-d64ee47e@probe.test>");
        let d1 = ingest.deliver_plain(PROBE, "<d1-d64ee47e@probe.test>");
        assert_ne!(d0, d1, "byte-identical subject + sender + no linkage -> separate threads");
    }

    /// e.txt: threads are per-inbox. Case (e) itself used a distinct Message-ID per inbox, so
    /// the sharper form — one Message-ID held by two inboxes — is asserted at the index in
    /// `in_memory_index_is_keyed_by_inbox_and_normalises_ids`. Here: a reply naming a
    /// Message-ID the *other* inbox holds must not reach it.
    #[test]
    fn linkage_never_crosses_inboxes() {
        let mut ingest = Ingest::default();
        let e1 = ingest.deliver_plain(PROBE, "<e1-d64ee47e@probe.test>");
        // A reply to e1's Message-ID, but delivered to the *other* inbox.
        let inbox2 = InboxId::new(PROBE2);
        let mid = MessageId::new("<e2-reply-d64ee47e@probe.test>");
        let parent = MessageId::new("<e1-d64ee47e@probe.test>");
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox2)
                .with_message_id(&mid)
                .with_in_reply_to(&parent),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::UnknownLinkage));
        assert_ne!(out.thread_id(), Some(e1));
    }

    /// R4's other half: one inbox spelled two ways is one inbox. Live, `AMKCASE@…` and
    /// `amkcase@…` resolve to the same inbox (reference/fixtures/18-inbox-case-normalization.txt),
    /// so a reply addressed to the shouted spelling must still find its parent's thread.
    #[test]
    fn inbox_scoping_folds_ascii_case() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain("amkcase@agentmail.to", "<case-root@probe.test>");
        let reply = ingest.deliver(
            "AMKCASE@AgentMail.to",
            "<case-reply@probe.test>",
            Some("<case-root@probe.test>"),
            &[],
        );
        assert_eq!(reply, root, "case variants of one inbox share one thread index");
    }

    // ---------------------------------------------------------------------------------------
    // Linkage forms. Case (a) sent In-Reply-To AND References together; isolation is matrix
    // gap G1, and R1 ("In-Reply-To and/or References") is what these two pin down.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn in_reply_to_alone_joins_the_parent_thread() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<root@probe.test>");
        let reply = ingest.deliver(PROBE, "<r1@probe.test>", Some("<root@probe.test>"), &[]);
        assert_eq!(reply, root);
    }

    #[test]
    fn references_alone_joins_the_parent_thread() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<root@probe.test>");
        let reply = ingest.deliver(PROBE, "<r1@probe.test>", None, &["<root@probe.test>"]);
        assert_eq!(reply, root);
    }

    /// The required "In-Reply-To naming a Message-ID we do not have".
    #[test]
    fn in_reply_to_naming_an_unknown_message_opens_a_new_thread() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<root@probe.test>");
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<orphan@probe.test>");
        let ghost = MessageId::new("<never-seen@elsewhere.test>");
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&ghost),
        );
        assert_eq!(
            out,
            ThreadAssignment::New(NewThreadReason::UnknownLinkage),
            "an unresolvable parent is not a reason to guess a thread"
        );
        assert_ne!(out.thread_id(), Some(root));
    }

    #[test]
    fn no_linkage_headers_are_distinguished_from_unresolvable_ones() {
        let index = InMemoryThreadIndex::new();
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<x@probe.test>");
        let out = assign_with(&index, &ThreadCandidate::new(&inbox).with_message_id(&mid));
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::NoLinkage));
    }

    /// Blank / `<>` linkage values carry nothing and must not read as "linked but unknown".
    #[test]
    fn empty_linkage_values_are_no_linkage_at_all() {
        let index = InMemoryThreadIndex::new();
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<x@probe.test>");
        let blank = MessageId::new("   ");
        let empty_brackets = MessageId::new("<>");
        let refs = vec![empty_brackets];
        let out = assign_with(
            &index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&blank)
                .with_references(&refs),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::NoLinkage));
    }

    /// RFC 5322 permits folding whitespace around a `msg-id`, so it is stripped.
    #[test]
    fn linkage_matches_across_folding_whitespace() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<root@probe.test>");
        let padded = ingest.deliver(PROBE, "<r1@probe.test>", Some("\t<root@probe.test> "), &[]);
        assert_eq!(padded, root);
    }

    /// An *unbracketed* linkage header does NOT match a bracketed stored id. No fixture shows an
    /// unbracketed In-Reply-To; re-bracketing one would let a sender join a thread upstream may
    /// well have left alone, and the sender picks that header. [INFERRED] under-threading is the
    /// accepted cost; a swaks probe with an unbracketed In-Reply-To would settle it.
    #[test]
    fn an_unbracketed_linkage_header_is_not_coerced_into_a_match() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<root@probe.test>");
        let bare = ingest.deliver(PROBE, "<r2@probe.test>", Some("root@probe.test"), &[]);
        assert_ne!(bare, root, "a bare addr-spec is not the stored bracketed id");
    }

    /// No case folding: `id-left` is case-sensitive, and the matrix never tested malformed ids.
    /// (Inbox ids DO fold — see `inbox_scoping_folds_ascii_case`; Message-IDs do not.)
    #[test]
    fn linkage_does_not_case_fold() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<Root@probe.test>");
        let reply = ingest.deliver(PROBE, "<r1@probe.test>", Some("<root@probe.test>"), &[]);
        assert_ne!(reply, root, "differing-case Message-IDs are different ids");
    }

    // ---------------------------------------------------------------------------------------
    // Chains: scan coverage, self-reference, precedence, ambiguity.
    // ---------------------------------------------------------------------------------------

    /// Every entry of `References` is examined, so the match resolves wherever it sits — first,
    /// middle or last.
    #[test]
    fn a_reference_match_resolves_at_any_position_in_the_list() {
        for match_at in 0..5usize {
            let mut ingest = Ingest::default();
            let root = ingest.deliver_plain(PROBE, "<chain-root@probe.test>");
            let refs: Vec<String> = (0..5)
                .map(|i| {
                    if i == match_at {
                        "<chain-root@probe.test>".to_string()
                    } else {
                        format!("<filler-{i}@nowhere.test>")
                    }
                })
                .collect();
            let borrowed: Vec<&str> = refs.iter().map(String::as_str).collect();
            let got = ingest.deliver(PROBE, "<tail@probe.test>", None, &borrowed);
            assert_eq!(got, root, "match_at={match_at}");
        }
    }

    /// A chain of nothing but unknown ids must not accidentally resolve.
    #[test]
    fn a_chain_of_only_unknown_ids_opens_a_new_thread() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<chain-root@probe.test>");
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<tail@probe.test>");
        let refs: Vec<MessageId> = (0..8)
            .map(|i| MessageId::new(format!("<filler-{i}@nowhere.test>")))
            .collect();
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_references(&refs),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::UnknownLinkage));
        assert_ne!(out.thread_id(), Some(root));
    }

    /// A message naming its own Message-ID — in In-Reply-To and twice in References — is not
    /// linked to anything: it cannot join its own not-yet-existing thread, and a redelivery of
    /// an id the inbox already holds is storage's dedup problem, not a grouping signal
    /// (matrix gap G6). Self-references are dropped *before* `had_link`, so the outcome is
    /// `NoLinkage`, not `UnknownLinkage`.
    #[test]
    fn self_references_are_not_linkage() {
        let mut ingest = Ingest::default();
        let first = ingest.deliver_plain(PROBE, "<loop@probe.test>");

        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<loop@probe.test>");
        let refs = vec![
            MessageId::new("<loop@probe.test>"),
            MessageId::new("<loop@probe.test>"),
        ];
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&mid)
                .with_references(&refs),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::NoLinkage));
        assert_ne!(out.thread_id(), Some(first));
    }

    /// Links that all point into the same thread — including a repeated entry — resolve to it
    /// once. Only the links actually carried are read; nothing is followed onward.
    #[test]
    fn repeated_and_agreeing_links_resolve_to_the_one_thread() {
        let mut ingest = Ingest::default();
        let a = ingest.deliver_plain(PROBE, "<m-a@probe.test>");
        let b = ingest.deliver(PROBE, "<m-b@probe.test>", Some("<m-a@probe.test>"), &[]);
        assert_eq!(a, b);
        let c = ingest.deliver(
            PROBE,
            "<m-c@probe.test>",
            Some("<m-b@probe.test>"),
            &["<m-a@probe.test>", "<m-b@probe.test>", "<m-a@probe.test>"],
        );
        assert_eq!(c, a, "links that all agree resolve to that one thread");
    }

    /// Precedence: a resolvable `In-Reply-To` decides, even when `References` reaches into a
    /// different local thread — the routine shape after a forward. The reply joins its direct
    /// parent; the two existing threads are untouched.
    #[test]
    fn a_resolvable_in_reply_to_beats_references_into_another_thread() {
        let mut ingest = Ingest::default();
        let t1 = ingest.deliver_plain(PROBE, "<x1@probe.test>");
        let t2 = ingest.deliver_plain(PROBE, "<x2@probe.test>");
        assert_ne!(t1, t2);

        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<merge@probe.test>");
        let parent = MessageId::new("<x1@probe.test>");
        let refs = vec![MessageId::new("<x2@probe.test>")];
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&parent)
                .with_references(&refs),
        );
        assert_eq!(
            out,
            ThreadAssignment::Existing {
                thread_id: t1,
                linked_by: MessageId::new("<x1@probe.test>")
            },
            "the direct parent wins; t2 is neither joined nor merged"
        );
        assert_ne!(out.thread_id(), Some(t2));
    }

    /// UNDETERMINED (matrix gap G2). With no `In-Reply-To` to break the tie, `References`
    /// spanning two threads opens a new one: we neither merge nor silently pick. Chosen
    /// behaviour, not observed.
    #[test]
    fn references_alone_spanning_two_threads_open_a_new_thread() {
        let mut ingest = Ingest::default();
        let t1 = ingest.deliver_plain(PROBE, "<x1@probe.test>");
        let t2 = ingest.deliver_plain(PROBE, "<x2@probe.test>");
        assert_ne!(t1, t2);

        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<merge@probe.test>");
        let refs = vec![
            MessageId::new("<x1@probe.test>"),
            MessageId::new("<x2@probe.test>"),
        ];
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_references(&refs),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::AmbiguousLinkage));
        assert!(out.thread_id().is_none(), "no thread is merged and none is picked");
    }

    /// An `In-Reply-To` that resolves to nothing cannot break the tie either — References is
    /// consulted, and its ambiguity stands.
    #[test]
    fn an_unresolvable_in_reply_to_does_not_break_a_reference_tie() {
        let mut ingest = Ingest::default();
        let _t1 = ingest.deliver_plain(PROBE, "<x1@probe.test>");
        let _t2 = ingest.deliver_plain(PROBE, "<x2@probe.test>");
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<merge@probe.test>");
        let ghost = MessageId::new("<ghost@nowhere.test>");
        let refs = vec![
            MessageId::new("<x1@probe.test>"),
            MessageId::new("<x2@probe.test>"),
        ];
        let out = assign_with(
            &ingest.index,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&ghost)
                .with_references(&refs),
        );
        assert_eq!(out, ThreadAssignment::New(NewThreadReason::AmbiguousLinkage));
    }

    /// One unit either side of the ambiguity boundary, References-only: one distinct thread
    /// joins, two do not, and three do not.
    #[test]
    fn ambiguity_boundary_is_two_distinct_threads() {
        let mut ingest = Ingest::default();
        let t1 = ingest.deliver_plain(PROBE, "<y1@probe.test>");
        let _t2 = ingest.deliver_plain(PROBE, "<y2@probe.test>");
        let _t3 = ingest.deliver_plain(PROBE, "<y3@probe.test>");
        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<z@probe.test>");

        let one = vec![MessageId::new("<y1@probe.test>")];
        let two = vec![
            MessageId::new("<y1@probe.test>"),
            MessageId::new("<y2@probe.test>"),
        ];
        let three = vec![
            MessageId::new("<y1@probe.test>"),
            MessageId::new("<y2@probe.test>"),
            MessageId::new("<y3@probe.test>"),
        ];

        let candidate = |refs: &[MessageId]| -> ThreadAssignment {
            let refs = refs.to_vec();
            assign_with(
                &ingest.index,
                &ThreadCandidate::new(&inbox)
                    .with_message_id(&mid)
                    .with_references(&refs),
            )
        };
        assert_eq!(
            candidate(&one),
            ThreadAssignment::Existing {
                thread_id: t1,
                linked_by: MessageId::new("<y1@probe.test>")
            }
        );
        assert_eq!(candidate(&two), ThreadAssignment::New(NewThreadReason::AmbiguousLinkage));
        assert_eq!(candidate(&three), ThreadAssignment::New(NewThreadReason::AmbiguousLinkage));
    }

    /// An unknown link alongside a resolvable one does not spoil the resolution — only a second
    /// *known* thread does.
    #[test]
    fn unknown_links_beside_a_known_one_still_join() {
        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<k1@probe.test>");
        let got = ingest.deliver(
            PROBE,
            "<k2@probe.test>",
            Some("<ghost@nowhere.test>"),
            &["<k1@probe.test>", "<ghost2@nowhere.test>"],
        );
        assert_eq!(got, root);
    }

    // ---------------------------------------------------------------------------------------
    // Wiring
    // ---------------------------------------------------------------------------------------

    /// `from_message_item` must read linkage out of the stored shape, so re-derived threading
    /// agrees with ingest-time threading.
    ///
    /// The JSON below is SYNTHETIC, not a capture: the linkage fields and Message-IDs are case
    /// (a)'s webhook payload (reference/fixtures/16-threading-matrix/a.txt), and the remaining
    /// required fields (`size`, `updated_at`, `created_at`, `to`) are shaped after the live list
    /// item in amk-types `message.rs::LIVE_ITEM` (fixture 03). Only the linkage fields are under
    /// test here.
    #[test]
    fn candidate_from_message_item_uses_its_linkage_headers() {
        let synthetic = r#"{"inbox_id":"amk-probe@agentmail.to",
            "thread_id":"23d8a68d-9c1b-41be-9677-acccf72e5dfe",
            "message_id":"<a-reply-d64ee47e@probe.test>",
            "labels":["received","unread","unauthenticated"],
            "timestamp":"2026-08-15T05:44:16.768Z","from":"Alice Probe <alice@probe.test>",
            "to":["amk-probe@agentmail.to"],"subject":"Re: AMKthreadA d64ee47e",
            "in_reply_to":"<a-root-d64ee47e@probe.test>",
            "references":["<a-root-d64ee47e@probe.test>"],"size":1241,
            "updated_at":"2026-08-15T05:44:16.768Z","created_at":"2026-08-15T05:44:16.768Z"}"#;
        let item: MessageItem = serde_json::from_str(synthetic).unwrap();

        let mut ingest = Ingest::default();
        let root = ingest.deliver_plain(PROBE, "<a-root-d64ee47e@probe.test>");
        let out = assign_with(&ingest.index, &ThreadCandidate::from_message_item(&item));
        assert_eq!(out.thread_id(), Some(root));
    }

    /// The index answers per inbox even for the same Message-ID (R4), folds inbox case
    /// (fixture 18), normalises the Message-ID exactly as linkage does, and reports nothing for
    /// an inbox it holds no mail for.
    #[test]
    fn in_memory_index_is_keyed_by_inbox_and_normalises_ids() {
        let mut index = InMemoryThreadIndex::new();
        let t1 = ThreadId::new_random();
        let t2 = ThreadId::new_random();
        let mid = MessageId::new("<same@probe.test>");
        index.insert(InboxId::new(PROBE), &mid, t1);
        index.insert(InboxId::new(PROBE2), &mid, t2);
        assert_eq!(index.len(), 2);
        assert_eq!(index.thread_of(&InboxId::new(PROBE), &mid), Some(t1));
        assert_eq!(index.thread_of(&InboxId::new(PROBE2), &mid), Some(t2));

        // Inbox case folds: one inbox spelled two ways is one index, not two.
        index.insert(InboxId::new("AMK-Probe@AgentMail.to"), &mid, t1);
        assert_eq!(index.len(), 2, "a case variant must not create a second index");
        assert_eq!(index.thread_of(&InboxId::new("AMK-PROBE@AGENTMAIL.TO"), &mid), Some(t1));

        // Message-ID lookup strips folding whitespace, and nothing else.
        assert_eq!(
            index.thread_of(&InboxId::new(PROBE), &MessageId::new(" <same@probe.test> ")),
            Some(t1),
            "lookup normalises exactly as linkage does"
        );
        assert_eq!(
            index.thread_of(&InboxId::new(PROBE), &MessageId::new("same@probe.test")),
            None,
            "unbracketed is a different id; the index does not coerce either"
        );
        assert_eq!(index.thread_of(&InboxId::new("other@agentmail.to"), &mid), None);
    }

    /// The rule must be usable through the trait object — that is the point of the boundary.
    #[test]
    fn assigner_is_object_safe() {
        let assigner: &dyn ThreadAssigner = &ReferenceChainThreading;
        let mut index = InMemoryThreadIndex::new();
        let root_thread = ThreadId::new_random();
        let root = MessageId::new("<root@probe.test>");
        index.insert(InboxId::new(PROBE), &root, root_thread);

        let inbox = InboxId::new(PROBE);
        let mid = MessageId::new("<reply@probe.test>");
        let index_ref: &dyn ThreadIndex = &index;
        let out = assigner.assign(
            index_ref,
            &ThreadCandidate::new(&inbox)
                .with_message_id(&mid)
                .with_in_reply_to(&root),
        );
        assert_eq!(out.thread_id(), Some(root_thread));
        assert!(!out.is_new());
    }
}
