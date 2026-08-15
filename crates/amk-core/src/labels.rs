//! Label semantics: which mail a caller may see, and which labels a caller may change.
//!
//! # There is no label resource
//!
//! Labels are free-form strings attached to messages and threads. There is no label CRUD
//! endpoint: a label exists exactly as long as something carries it, and it changes only through
//! `add_labels` / `remove_labels` on a message or thread PATCH (`[SPEC:openapi]`
//! `type_messages:UpdateMessageRequest`, `type_threads:UpdateThreadRequest`). Nothing here models
//! a container that mail lives *in* — a label is a tag, never a location.
//!
//! # This module owns the whole visibility verdict
//!
//! Restricted-label admission has two inputs:
//!
//! 1. the credential holds the matching `label_*_read` flag — the partial gate exposed by
//!    [`crate::permissions::allows_label_read`], paired to its label by
//!    [`amk_types::api_key::label_read_flag`];
//! 2. the request set the matching `include_*` query flag.
//!
//! **How many of them apply is a property of the path, and there are three answers, not two.**
//! `include_{spam,blocked,unauthenticated,trash}` exists on **4 of the 33 paginated GETs**
//! (`[SPEC:openapi]`) — `/v0/threads`, `/v0/pods/{pod_id}/threads`, `/v0/inboxes/{inbox_id}/threads`
//! and `/v0/inboxes/{inbox_id}/messages`. It exists on no search endpoint and on no drafts list:
//!
//! | mode | constructor | rule |
//! |------|-------------|------|
//! | list carrying `include_*` | [`LabelAccess::list`] | permission **and** the matching flag |
//! | search | [`LabelAccess::search`] | permission only — restricted mail **is** returned |
//! | get-by-id | [`LabelAccess::by_id`] | permission only |
//!
//! [`admit`] is the single function that composes them, and `crate::permissions` deliberately
//! exposes no visibility verdict of its own — when both modules answered this question they
//! answered it differently, and the permission-only answer silently returned rows fixture `09b`
//! proves are not returned.
//!
//! ## What was observed, and what is inferred
//!
//! **Observed** (`reference/fixtures/09b-unauthenticated-variant.txt`, 1c) — the *exclusion* half.
//! With a credential that could fetch the `unauthenticated` message by id, and therefore held
//! `label_unauthenticated_read`:
//!
//! ```text
//! GET …/messages                            -> count=3   (the `sent` mail only)
//! GET …/messages?labels=received            -> count=0
//! GET …/messages?labels=unauthenticated     -> count=0
//! GET …/threads                             -> count=0
//! GET …/messages/<id>                       -> full object, labels included
//! ```
//!
//! So the permission alone does not admit a row to a list, and the `labels[]` query filter is a
//! *filter*, never a grant: naming the restricted label explicitly still returned nothing. That is
//! why [`admit`] takes no filter argument — a filter can only narrow what admission already let
//! through.
//!
//! **Observed** (`reference/fixtures/20-search-and-label-precedence.txt`, D) — search does **not**
//! hide restricted mail:
//!
//! ```text
//! PATCH …/messages/<id>  {"add_labels":["spam"]}
//! GET   …/messages?limit=50        -> count=5, the message ABSENT
//! GET   …/messages/search?q=FW:    -> count=1, the message STILL RETURNED
//! ```
//!
//! Same inbox, same credential, same moment. An earlier revision of this module had two modes and
//! ran search through the list rule; because no search endpoint has an `include_*` parameter, that
//! pinned search at [`IncludeFlags::NONE`] forever and made restricted mail unreachable by search
//! for every credential that will ever exist. Search behaves like get-by-id, not like list.
//!
//! **[INFERRED]** — two halves remain unobserved, and both fail closed:
//!
//! * No fixture sets an `include_*` flag, so nothing was observed to make restricted mail *appear*
//!   in a list. The flags are the only mechanism that could; treating them as
//!   necessary-and-sufficient alongside the permission matches the observation.
//! * Fixture 20's probe key was org-scoped and **unrestricted**, so its permission half was
//!   trivially satisfied. What is observed is that the *include-flag* half does not gate search;
//!   whether search would hide the message from a key lacking `label_spam_read` is unobserved.
//!   Search is therefore treated as permission-gated exactly like get-by-id, which is the reading
//!   that matches `09b` and fails closed.
//!
//! ## Admission is a storage predicate, not a post-filter
//!
//! [`excluded_labels`] returns the labels a query must exclude, for amk-store to push into the
//! `WHERE` clause. **Filtering an already-fetched page is not the mechanism** and must not be used
//! as one: with `limit=1`, dropping the hidden row from each page yields `count:0` *with* a
//! `next_page_token`, so walking the cursor counts the hidden mail exactly and the cursors
//! themselves disclose its ids and timestamps. The row must never be fetched.
//! [`admits`] exists for the single-resource path and as the in-memory statement of the same
//! predicate; a test pins the two to the same verdict so the pushed-down query cannot drift.
//!
//! A denial on the get-by-id path surfaces as `not_found`, never `forbidden`
//! (`reference/fixtures/05-error-catalog.http`: the `not_found` `fix` string reads *"… restricted
//! labels like spam or trash are hidden without their label-read permission …"*). See
//! [`LabelDenial::error_code`].
//!
//! # Mutation
//!
//! `[SPEC:openapi]` puts *"Cannot be system labels"* on `UpdateThreadRequest` only, and says
//! nothing of the sort about messages. Reading that as "threads are gated, messages are not" is
//! wrong, and it is a mistake this project made twice — two reviewers reached it independently and
//! it was written into a dispatch instruction. `reference/fixtures/19-message-label-patch-gate.txt`
//! settles it: a **message** PATCH rejects the same four labels with the same 400
//! `validation_error`. [`system_label_violations`] therefore gates **both** paths — see
//! [`system_labels`] for the observed set.
//!
//! # Not this module's job
//!
//! Subject normalisation is **ingest's**: fixture 16 observed that a message with an empty
//! Subject stores no `subject` field at all and that trailing subject whitespace is stripped.
//! That happens once, when the message is stored; nothing here reads a subject.
//!
//! Webhook payload construction is **amk-events'**. A webhook delivery carries no API key, so the
//! credential half of [`admit`] is undefined on that path and this module must not be consulted
//! there — fixture `09b` captured the `message.received.unauthenticated` delivery firing for mail
//! that every list endpoint hid, which is precisely the event a credential-shaped gate would
//! suppress.

use amk_types::api_key::KeyGrants;
use amk_types::error::ErrorCode;
use amk_types::message::labels::{self, RESTRICTED};
use amk_types::page::ListParams;
use amk_types::thread::Thread;
use amk_types::Timestamp;

use crate::permissions::allows_label_read;

// ---------------------------------------------------------------------------------------------
// The include_* flags
// ---------------------------------------------------------------------------------------------

/// The restricted labels the *request* asked to see (`include_*` query flags).
///
/// Held as a bitmask over `amk_types::message::labels::RESTRICTED`'s indices, resolved from that
/// array rather than hardcoded, so a reordering upstream cannot re-point a flag at another label.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncludeFlags(u8);

/// The mask is a `u8`, so the label catalog it indexes must fit in one.
const _: () = assert!(
    RESTRICTED.len() <= u8::BITS as usize,
    "IncludeFlags is a u8 bitmask over RESTRICTED; widen it before adding a ninth label"
);

impl IncludeFlags {
    /// Nothing requested — the state of a list query that names no `include_*` flag.
    pub const NONE: Self = Self(0);
    /// Every restricted label requested.
    ///
    /// The **width** is derived from `RESTRICTED` too, not written as `0b1111`: every bit
    /// *position* is already resolved at runtime so an upstream reorder cannot re-point a flag,
    /// and a hardcoded width would have left a fifth label permanently unrequestable — the same
    /// class of permanent-impossibility bug fixture 20 caught in the search mode.
    pub const ALL: Self = Self(((1u16 << RESTRICTED.len()) - 1) as u8);

    /// Build from the four per-label booleans, in the order the API names them.
    pub fn from_flags(spam: bool, blocked: bool, unauthenticated: bool, trash: bool) -> Self {
        let mut set = Self::NONE;
        for (on, label) in [
            (spam, labels::SPAM),
            (blocked, labels::BLOCKED),
            (unauthenticated, labels::UNAUTHENTICATED),
            (trash, labels::TRASH),
        ] {
            if on {
                set = set.with(label);
            }
        }
        set
    }

    /// Read the flags off a list query. Every flag defaults to **false** when absent: restricted
    /// mail is hidden unless it was asked for.
    pub fn from_params(params: &ListParams) -> Self {
        Self::from_flags(
            params.include_spam.unwrap_or(false),
            params.include_blocked.unwrap_or(false),
            params.include_unauthenticated.unwrap_or(false),
            params.include_trash.unwrap_or(false),
        )
    }

    /// Request a restricted label. A label that is not restricted is not representable here and
    /// is ignored.
    #[must_use]
    pub fn with(self, label: &str) -> Self {
        match index_of(label) {
            Some(i) => Self(self.0 | (1 << i)),
            None => self,
        }
    }

    /// Whether this request asked for `label`. Always false for a label that is not restricted.
    pub fn contains(self, label: &str) -> bool {
        match index_of(label) {
            Some(i) => self.0 & (1 << i) != 0,
            None => false,
        }
    }
}

fn index_of(label: &str) -> Option<usize> {
    RESTRICTED.iter().position(|&l| l == label)
}

// ---------------------------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------------------------

/// Which of the two admission inputs the *path* applies. Private: a handler names the path by
/// picking a constructor, and cannot assemble a fourth rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One of the four paginated GETs that carry `include_*`.
    List(IncludeFlags),
    /// A search endpoint. No `include_*` parameter exists here and restricted mail is returned.
    Search,
    /// A single-resource fetch by id.
    ById,
}

/// The admission inputs for one request: what the credential may read, and which rule the path
/// applies. Built by [`LabelAccess::list`], [`LabelAccess::search`] or [`LabelAccess::by_id`] so
/// the three paths cannot drift.
#[derive(Debug, Clone, Copy)]
pub struct LabelAccess<'a> {
    grants: &'a KeyGrants,
    mode: Mode,
}

impl<'a> LabelAccess<'a> {
    /// One of the four paginated GETs that carry `include_*` — `/v0/threads`,
    /// `/v0/pods/{pod_id}/threads`, `/v0/inboxes/{inbox_id}/threads`,
    /// `/v0/inboxes/{inbox_id}/messages`. Both the `label_*_read` permission and the matching
    /// `include_*` flag are required.
    ///
    /// **Only those four.** Any other paginated GET — every search endpoint, every drafts list —
    /// has no such parameter, so routing it here would gate it on a flag its caller has no way to
    /// set.
    pub fn list(grants: &'a KeyGrants, requested: IncludeFlags) -> Self {
        Self { grants, mode: Mode::List(requested) }
    }

    /// A search endpoint: the permission alone decides, and restricted mail **is** returned.
    ///
    /// `reference/fixtures/20-search-and-label-precedence.txt` (D): a message labelled `spam`
    /// disappeared from `GET …/messages` and was still returned by `GET …/messages/search`, for
    /// the same credential at the same moment. Search has no `include_*` parameter, so applying
    /// the list rule here would hide it permanently rather than pending a flag.
    pub fn search(grants: &'a KeyGrants) -> Self {
        Self { grants, mode: Mode::Search }
    }

    /// A get-by-id: there are no `include_*` flags on this path, so the permission alone decides.
    /// Fixture `09b` observed exactly this asymmetry — the message every list endpoint refused was
    /// returned in full by id.
    pub fn by_id(grants: &'a KeyGrants) -> Self {
        Self { grants, mode: Mode::ById }
    }

    /// Whether the *request* asked for `label`. Only a list path can fail to ask: the other two
    /// have no parameter to ask with, and treating their silence as a refusal is what made
    /// restricted mail unreachable by search.
    fn requested(&self, label: &str) -> bool {
        match self.mode {
            Mode::List(flags) => flags.contains(label),
            Mode::Search | Mode::ById => true,
        }
    }
}

/// Why an item was withheld. For logs and metrics; **never for the response body**, where every
/// variant renders as the same `not_found` (see [`LabelDenial::error_code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialKind {
    /// The credential lacks the matching `label_*_read` permission.
    NotPermitted,
    /// The credential could read it, but the request did not set the matching `include_*` flag.
    NotRequested,
}

/// A withheld item, and why.
///
/// The offending label is **not** public. Publishing it lets a handler template the masked fact
/// straight back into the `message` or `fix` of the 404 — turning "Message not found" into a
/// confirmation that the message exists and is spam or trash, which is the disclosure the 404 was
/// chosen to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelDenial {
    kind: DenialKind,
    label: &'static str,
}

impl LabelDenial {
    pub fn kind(&self) -> DenialKind {
        self.kind
    }

    /// The offending label, for this crate's own tests and reasoning only — see the type docs.
    ///
    /// `dead_code` outside `cfg(test)` is the point, not an oversight: no handler can reach this,
    /// so no handler can template the masked label into a 404 body.
    #[allow(dead_code)]
    pub(crate) fn label(&self) -> &'static str {
        self.label
    }

    /// The wire code when a *single-resource* fetch is denied: `not_found`, never `forbidden`.
    /// The live `not_found` `fix` string documents this masking verbatim
    /// (`reference/fixtures/05-error-catalog.http`).
    ///
    /// A list denial produces no error at all: the row was never fetched.
    pub fn error_code(&self) -> ErrorCode {
        ErrorCode::NotFound
    }
}

/// The one admission decision. Every restricted label on the item must be admitted; a single
/// unadmitted one withholds the item.
///
/// A missing permission is reported in preference to a missing `include_*` flag, whatever order
/// the labels appear in: the permission denial is the security-relevant one, and its consequence
/// must not depend on label ordering.
pub fn admit<S: AsRef<str>>(
    item_labels: &[S],
    access: &LabelAccess<'_>,
) -> Result<(), LabelDenial> {
    for label in item_labels.iter().map(AsRef::as_ref) {
        if let Some(&restricted) = RESTRICTED.iter().find(|&&r| r == label) {
            if !allows_label_read(access.grants, restricted) {
                return Err(LabelDenial { kind: DenialKind::NotPermitted, label: restricted });
            }
        }
    }
    for label in item_labels.iter().map(AsRef::as_ref) {
        if let Some(&restricted) = RESTRICTED.iter().find(|&&r| r == label) {
            if !access.requested(restricted) {
                return Err(LabelDenial { kind: DenialKind::NotRequested, label: restricted });
            }
        }
    }
    Ok(())
}

/// [`admit`] as a predicate.
pub fn admits<S: AsRef<str>>(item_labels: &[S], access: &LabelAccess<'_>) -> bool {
    admit(item_labels, access).is_ok()
}

/// The labels a query **must exclude** for this credential and this request.
///
/// This is the admission mechanism for every collection endpoint: amk-store pushes it into the
/// query so a row carrying any of these labels is never read, never counted, and never reachable
/// by walking `next_page_token`. Dropping such rows from a page after the fact leaves the gap
/// visible — see the module docs.
///
/// Empty means nothing is hidden from this caller. **A search query for a credential that holds
/// the label-read flags excludes nothing** — fixture 20 observed the reference API returning a
/// spam-labelled message from search at the moment the list endpoint hid it, so an exclusion here
/// would be a conformance failure rather than a safe default.
///
/// It is [`admit`]'s own verdict, applied one label at a time, so the pushed-down `WHERE` clause
/// cannot drift from the in-memory answer.
pub fn excluded_labels(access: &LabelAccess<'_>) -> Vec<&'static str> {
    RESTRICTED
        .into_iter()
        .filter(|&label| !admits(&[label], access))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Threads with partially hidden members
// ---------------------------------------------------------------------------------------------

/// What [`redact_thread`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRedaction {
    /// Every member is admitted and so is every label on the thread itself; nothing is touched.
    Unchanged,
    /// Something was withheld: members removed and every aggregate recomputed from what remains,
    /// and/or restricted labels stripped from `item.labels`. A label can offend on its own —
    /// whether `ThreadItem.labels` is the union of its members' labels is unobserved.
    Redacted,
    /// No member is admitted. The handler must answer `not_found` — the thread itself must not be
    /// returned, because its aggregates would describe nothing the caller may see.
    Withheld,
}

/// Remove the messages this caller may not see from a thread, **recompute every aggregate that
/// counted them, and strip the restricted labels this caller may not see from `item.labels`**.
///
/// `Thread` carries `messages: Vec<Message>` alongside scalars derived from those messages —
/// `message_count`, `size`, `last_message_id`, `timestamp`, `senders`, `recipients`, `preview`,
/// `attachments`. Filtering the vector alone leaves each scalar still counting, sizing and naming
/// the hidden mail, which discloses more than the message would have.
///
/// `item.labels` is the same failure one level up, and worse: a thread returned with no spam
/// member, `message_count: 1`, and `"spam"` in `labels` names the very fact the redaction exists to
/// conceal — the fact [`LabelDenial::label`] is `pub(crate)` to keep out of a 404. Offending labels
/// are **stripped**, never rebuilt as the union of the surviving members' labels: whether
/// `ThreadItem.labels` IS that union is unobserved (register C2 — fixture 16's only threaded
/// example has two members carrying identical labels, so it cannot discriminate), and stripping is
/// correct under either rule while disclosing nothing.
///
/// The postcondition is therefore `admits(&thread.item.labels, access)` for any body this function
/// returns, and a thread whose own labels offend is redacted even when every member is admitted.
///
/// **[INFERRED], and deliberately fail-closed.** Nothing observed says upstream does this: no
/// fixture contains a thread whose members carry different restricted labels, so upstream's
/// behaviour for a mixed thread is unknown. Worse, this function may be *unreachable* — if
/// `ThreadItem.labels` is the union of its members' labels, then admitting the thread admits every
/// member and no mixed case exists. That `ThreadItem.labels` is that union is itself unobserved,
/// which is exactly why the fail-closed path is written rather than assumed away. All redaction
/// logic lives in this one function so the assumption can be re-examined in one place.
///
/// Values are **filtered, never rebuilt**: `senders`, `recipients`, `attachments` and `labels` keep
/// only entries a remaining message (or, for labels, this credential) still accounts for, so
/// upstream's own composition and ordering survive. `created_at` and `updated_at` are storage
/// metadata rather than message aggregates and are left alone — a residual channel: `updated_at`
/// still reflects the moment hidden mail arrived.
///
/// `subject` is **not** recomputed. Nothing observed derives a thread's subject from its current
/// membership, and fixture `16-threading-matrix/a.txt` shows the thread keeping the ROOT's subject
/// (`"AMKthreadA d64ee47e"`) while its reply carries `"Re: AMKthreadA d64ee47e"` — so hiding the
/// root and re-deriving from `messages[0]` would rewrite the subject to a value no artifact
/// supports. Leaving it is the residual channel upstream itself already has.
pub fn redact_thread(thread: &mut Thread, access: &LabelAccess<'_>) -> ThreadRedaction {
    let Thread { item, messages } = thread;
    let leaks_a_label = item
        .labels
        .iter()
        .any(|l| !admits(std::slice::from_ref(l), access));
    let hides_a_member = messages.iter().any(|m| !admits(&m.item.labels, access));
    if !leaks_a_label && !hides_a_member {
        return ThreadRedaction::Unchanged;
    }

    item.labels
        .retain(|l| admits(std::slice::from_ref(l), access));
    if !hides_a_member {
        // Every member survives, so no aggregate counted anything this caller may not see; only
        // the thread's own label list did. Recomputing aggregates here could only invent values.
        return ThreadRedaction::Redacted;
    }

    messages.retain(|m| admits(&m.item.labels, access));
    let Some(last) = messages.last() else {
        return ThreadRedaction::Withheld;
    };

    item.last_message_id = last.item.message_id.clone();
    item.timestamp = last.item.timestamp;
    item.preview = last.item.preview.clone();
    item.message_count = messages.len() as u64;
    item.size = messages.iter().map(|m| m.item.size).sum();
    item.received_timestamp = latest_timestamp_labelled(messages, labels::RECEIVED);
    item.sent_timestamp = latest_timestamp_labelled(messages, labels::SENT);

    item.senders
        .retain(|s| messages.iter().any(|m| &m.item.from == s));
    item.recipients.retain(|r| {
        messages.iter().any(|m| {
            m.item.to.contains(r)
                || m.item
                    .cc
                    .iter()
                    .chain(m.item.bcc.iter())
                    .flatten()
                    .any(|a| a == r)
        })
    });
    if let Some(attachments) = item.attachments.as_mut() {
        attachments.retain(|a| {
            messages.iter().any(|m| {
                m.item
                    .attachments
                    .iter()
                    .flatten()
                    .any(|kept| kept.attachment_id == a.attachment_id)
            })
        });
    }

    ThreadRedaction::Redacted
}

fn latest_timestamp_labelled(
    messages: &[amk_types::message::Message],
    label: &str,
) -> Option<Timestamp> {
    messages
        .iter()
        .filter(|m| m.item.labels.iter().any(|l| l == label))
        .map(|m| m.item.timestamp)
        .max()
}

// ---------------------------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------------------------

/// The labels a client PATCH may neither add nor remove, on a **message or a thread** alike.
///
/// **`[TESTED]` — the exact set, not a floor.** `reference/fixtures/19-message-label-patch-gate.txt`
/// PATCHed every candidate onto a live message: `sent`, `received`, `bounced` and `scheduled`
/// returned 400 `validation_error` ("Cannot use system label: …"); `unread`, all four restricted
/// labels, and an arbitrary user tag were accepted.
///
/// Two earlier readings died here, and both are worth remembering:
///
/// * The restricted labels are **not** system. An earlier revision inferred they must be — they
///   carry the pipeline's abuse and authentication verdicts, so a client setting them looked like
///   forgery. Reasonable, and wrong: the live API accepts `{"add_labels":["spam"]}` on a message.
///   **Restricted and system are independent axes** — restricted governs who may SEE a label,
///   system governs who may SET one.
/// * The gate was thought to apply to threads only, because `type_threads:UpdateThreadRequest`
///   says "Cannot be system labels" while `type_messages:UpdateMessageRequest` says nothing. Two
///   reviewers and the orchestrator agreed on that reading and it was written into a dispatch
///   instruction. The live API gates messages too.
///
/// `scheduled` IS system — inferred here first ("a client adding it would assert a schedule no job
/// exists for"), flagged by a reviewer as unevidenced, then observed. The inference was right.
///
/// Not exercised by the probe: `complained`. Its system-ness is **unobserved**, so it is absent
/// from the set rather than assumed either way.
pub fn system_labels() -> Vec<&'static str> {
    labels::SYSTEM.to_vec()
}

/// Whether a client PATCH is forbidden from touching this label — on a **message exactly as on a
/// thread**. Match is exact: labels are free-form strings and nothing here case-folds them (see
/// [`apply_mutation`]).
///
/// The spec text tempts the opposite reading, because `type_threads:UpdateThreadRequest` says
/// "Cannot be system labels" and `type_messages:UpdateMessageRequest` says nothing. Two reviewers
/// and this project's own dispatch instruction reached that reading; fixture 19 PATCHed all four
/// onto a live **message** and got 400 `validation_error` each time.
pub fn is_system(label: &str) -> bool {
    system_labels().contains(&label)
}

/// Which request field a rejected label came from. The names are the wire field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationField {
    Add,
    Remove,
}

impl MutationField {
    pub fn as_field_name(self) -> &'static str {
        match self {
            MutationField::Add => "add_labels",
            MutationField::Remove => "remove_labels",
        }
    }
}

/// A client-supplied label that the client is not allowed to set on a **message or a thread**.
///
/// `field` and `index` together are the observed `errors[].path`. Fixture 19's verbatim body:
///
/// ```json
/// {"code":"custom","message":"Cannot use system label: bounced","path":["add_labels",0]}
/// ```
///
/// — field name **then** array index. The index is **per field**: `remove_labels` is validated
/// after `add_labels`, so a running count across both would report `["remove_labels",2]` for the
/// first element of `remove_labels`, and the label alone cannot stand in for the position because
/// it is not unique (`{"add_labels":["sent","sent"]}` is a legal request body).
///
/// amk-http renders the path as `[field.as_field_name(), index]`; amk-core does not build it
/// itself, because `ValidationIssue::path` is a `Vec<serde_json::Value>` and this crate takes no
/// serde_json dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemLabelViolation {
    pub field: MutationField,
    /// Position within `field`'s own array — **not** across both arrays.
    pub index: usize,
    pub label: String,
}

impl SystemLabelViolation {
    /// The `errors[].message` the live API returns, verbatim from fixture 19
    /// (`"Cannot use system label: bounced"`). A validation error conceals nothing, so naming the
    /// offending label here is correct — unlike the `not_found` masking path.
    pub fn message(&self) -> String {
        format!("Cannot use system label: {}", self.label)
    }
}

/// Every system label a client PATCH tried to add or remove, on **either** a thread or a message,
/// each carrying the position it occupied in its own array. Empty means the mutation is allowed.
///
/// A non-empty result rejects the **whole** mutation — fixture 19's cleanup attempt
/// `{"remove_labels":["spam","bounced"]}` returned 400 as a unit rather than removing the legal
/// `spam` and refusing `bounced`. There is no partial application: the caller returns
/// `validation_error` and mutates nothing.
///
/// All violations are returned rather than just the first, because the envelope carries an
/// `errors[]` array.
///
/// Applies to messages as well as threads. The spec text says otherwise by omission, and that
/// omission misled two reviewers and this project's own dispatch; fixture 19 observed a message
/// PATCH returning 400 for each of the four.
///
/// The pipeline does not call this — it owns those labels and applies [`apply_mutation`] directly,
/// which is exactly why the gate lives at the request boundary rather than inside the mutation.
pub fn system_label_violations<A: AsRef<str>, R: AsRef<str>>(
    add: &[A],
    remove: &[R],
) -> Vec<SystemLabelViolation> {
    // `enumerate` per field, not across the chain: the index is a position within `add_labels` or
    // within `remove_labels`, which is what the observed path spells.
    let adds = add
        .iter()
        .enumerate()
        .map(|(i, l)| (MutationField::Add, i, l.as_ref()));
    let removes = remove
        .iter()
        .enumerate()
        .map(|(i, l)| (MutationField::Remove, i, l.as_ref()));
    adds.chain(removes)
        .filter(|(_, _, label)| is_system(label))
        .map(|(field, index, label)| SystemLabelViolation { field, index, label: label.to_owned() })
        .collect()
}

/// Apply an add/remove pair to a label list.
///
/// * `remove` wins over `add` for a label named in both — **`[TESTED]` on a message**, not read
///   across from the thread schema. `[SPEC:openapi]` states *"Takes priority over `add_labels` (in
///   the event of duplicate labels passed in)"* on `type_threads:UpdateThreadRequest` only, and
///   `type_messages:UpdateMessageRequest` says merely "Label or labels to remove from message"; an
///   earlier revision applied the thread sentence to messages anyway.
///   `reference/fixtures/20-search-and-label-precedence.txt` (C) probed it live —
///   `{"add_labels":["probe-conflict"],"remove_labels":["probe-conflict"]}` returned 200 with
///   `labels: ["received","unread"]`, the label absent. The generalisation was right and no longer
///   rests on being right.
/// * Existing order and existing contents are preserved exactly; new labels are appended in the
///   order given, and a label already present is not appended again.
/// * A duplicate already on the record **stays**. Collapsing it would rewrite stored labels on a
///   PATCH that asked for nothing, which is a normalisation this module does not perform.
/// * Matching is exact — `Spam` is not `spam`.
///
/// This is the shared mechanism, deliberately ungated: both client paths (thread and message) call
/// [`system_label_violations`] first, while the pipeline path calls this directly because setting
/// `unauthenticated` or `sent` is precisely its job.
pub fn apply_mutation<A: AsRef<str>, R: AsRef<str>>(
    current: &[String],
    add: &[A],
    remove: &[R],
) -> Vec<String> {
    let removed: Vec<&str> = remove.iter().map(AsRef::as_ref).collect();
    let mut out: Vec<String> = current
        .iter()
        .filter(|label| !removed.contains(&label.as_str()))
        .cloned()
        .collect();
    for label in add.iter().map(AsRef::as_ref) {
        if !removed.contains(&label) && !out.iter().any(|l| l == label) {
            out.push(label.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use amk_types::api_key::ApiKeyPermissions;
    use amk_types::ids::{AttachmentId, InboxId, MessageId, ThreadId};
    use amk_types::message::{Attachment, Message, MessageItem};
    use amk_types::thread::ThreadItem;

    fn v(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|s| (*s).to_string()).collect()
    }

    /// A restricted credential holding exactly the named `label_*_read` flags.
    fn reader(spam: bool, blocked: bool, unauthenticated: bool, trash: bool) -> KeyGrants {
        KeyGrants::Restricted(ApiKeyPermissions {
            message_read: Some(true),
            label_spam_read: spam.then_some(true),
            label_blocked_read: blocked.then_some(true),
            label_unauthenticated_read: unauthenticated.then_some(true),
            label_trash_read: trash.then_some(true),
            ..Default::default()
        })
    }

    fn no_label_flags() -> KeyGrants {
        reader(false, false, false, false)
    }

    fn all_label_flags() -> KeyGrants {
        reader(true, true, true, true)
    }

    // --- the include_* flags ------------------------------------------------------------------

    #[test]
    fn each_flag_maps_to_its_own_label_and_no_other() {
        // Guards against a reorder of amk_types' RESTRICTED silently re-pointing a flag.
        assert_eq!(RESTRICTED.len(), 4);
        let cases = [
            (IncludeFlags::from_flags(true, false, false, false), labels::SPAM),
            (IncludeFlags::from_flags(false, true, false, false), labels::BLOCKED),
            (IncludeFlags::from_flags(false, false, true, false), labels::UNAUTHENTICATED),
            (IncludeFlags::from_flags(false, false, false, true), labels::TRASH),
        ];
        for (set, expected) in cases {
            for &label in RESTRICTED.iter() {
                assert_eq!(set.contains(label), label == expected, "{label} in {set:?}");
            }
        }
        assert_eq!(IncludeFlags::from_flags(true, true, true, true), IncludeFlags::ALL);
    }

    #[test]
    fn the_all_mask_is_as_wide_as_the_label_catalog() {
        // The width is the one thing that used to be a hardcoded copy of RESTRICTED.len(): a fifth
        // restricted label upstream would have stayed permanently unrequestable behind `0b1111`.
        for &label in RESTRICTED.iter() {
            assert!(IncludeFlags::ALL.contains(label), "{label} is outside the ALL mask");
        }
        assert_eq!(
            RESTRICTED
                .iter()
                .filter(|l| IncludeFlags::ALL.contains(l))
                .count(),
            RESTRICTED.len()
        );
    }

    #[test]
    fn a_non_restricted_label_is_never_requestable() {
        assert!(!IncludeFlags::ALL.contains(labels::RECEIVED));
        assert!(!IncludeFlags::ALL.contains(labels::BOUNCED));
        assert_eq!(IncludeFlags::NONE.with("anything-else"), IncludeFlags::NONE);
    }

    #[test]
    fn include_flags_default_to_false_when_the_query_omits_them() {
        assert_eq!(IncludeFlags::from_params(&ListParams::default()), IncludeFlags::NONE);
        let asked = ListParams { include_trash: Some(true), ..ListParams::default() };
        let flags = IncludeFlags::from_params(&asked);
        assert!(flags.contains(labels::TRASH));
        assert!(!flags.contains(labels::SPAM));
    }

    // --- admission: the four combinations -----------------------------------------------------

    #[test]
    fn restricted_label_with_flag_but_without_permission_is_hidden() {
        let grants = no_label_flags();
        let access = LabelAccess::list(&grants, IncludeFlags::ALL);
        let denial = admit(&v(&["received", "spam"]), &access).unwrap_err();
        assert_eq!(denial.kind(), DenialKind::NotPermitted);
        assert_eq!(denial.label(), labels::SPAM);
    }

    #[test]
    fn restricted_label_with_permission_but_without_flag_is_hidden_from_a_list() {
        // Fixture 09b in one assertion: the credential HELD label_unauthenticated_read and the
        // list endpoints still returned nothing.
        let grants = all_label_flags();
        let access = LabelAccess::list(&grants, IncludeFlags::NONE);
        let denial = admit(&v(&["received", "spam"]), &access).unwrap_err();
        assert_eq!(denial.kind(), DenialKind::NotRequested);
        assert_eq!(denial.label(), labels::SPAM);
    }

    #[test]
    fn restricted_label_with_both_flag_and_permission_is_listed() {
        let grants = reader(true, false, false, false);
        let access =
            LabelAccess::list(&grants, IncludeFlags::from_flags(true, false, false, false));
        assert!(admits(&v(&["received", "spam"]), &access));
    }

    #[test]
    fn restricted_label_with_neither_reports_the_permission_denial() {
        let grants = no_label_flags();
        let access = LabelAccess::list(&grants, IncludeFlags::NONE);
        let denial = admit(&v(&["spam"]), &access).unwrap_err();
        assert_eq!(denial.kind(), DenialKind::NotPermitted, "the security-relevant denial wins");
    }

    #[test]
    fn a_permission_denial_outranks_a_flag_omission_in_either_label_order() {
        // trash is readable but not requested; spam is requested but not readable.
        let grants = reader(false, false, false, true);
        let access =
            LabelAccess::list(&grants, IncludeFlags::from_flags(true, false, false, false));
        for order in [v(&["trash", "spam"]), v(&["spam", "trash"])] {
            let denial = admit(&order, &access).unwrap_err();
            assert_eq!(
                denial.kind(),
                DenialKind::NotPermitted,
                "order {order:?} changed the verdict"
            );
            assert_eq!(denial.label(), labels::SPAM);
        }
    }

    #[test]
    fn every_restricted_label_on_the_item_must_be_admitted() {
        let grants = reader(true, false, false, false);
        let access =
            LabelAccess::list(&grants, IncludeFlags::from_flags(true, false, false, false));
        assert!(admits(&v(&["spam"]), &access));
        assert!(
            !admits(&v(&["spam", "trash"]), &access),
            "admitting spam must not carry trash in"
        );
    }

    #[test]
    fn ordinary_mail_needs_no_permission_and_no_flag() {
        let grants = no_label_flags();
        let access = LabelAccess::list(&grants, IncludeFlags::NONE);
        assert!(admits(&v(&["received", "unread"]), &access));
        assert!(admits(&v(&["sent"]), &access));
        assert!(admits(&v(&["bounced"]), &access), "a bounce verdict is not restricted mail");
        assert!(admits::<String>(&[], &access), "an unlabeled item is not restricted");
        assert!(
            admits(&v(&["Spam"]), &access),
            "labels match exactly; `Spam` is a free-form label, not the restricted one"
        );
    }

    #[test]
    fn an_unrestricted_credential_still_needs_the_include_flag_on_a_list() {
        // The permission half is satisfied by construction; the request half is not.
        let grants = KeyGrants::Unrestricted;
        assert!(!admits(&v(&["spam"]), &LabelAccess::list(&grants, IncludeFlags::NONE)));
        assert!(admits(&v(&["spam"]), &LabelAccess::list(&grants, IncludeFlags::ALL)));
        assert!(admits(&v(&["spam"]), &LabelAccess::by_id(&grants)));
    }

    // --- admission: the observed list/get-by-id asymmetry -------------------------------------

    #[test]
    fn fixture_09b_list_counts_are_reproduced() {
        // reference/fixtures/09b-unauthenticated-variant.txt (1c), verbatim:
        //   GET …/messages                        -> count=3   (the three `sent` messages)
        //   GET …/messages?labels=received        -> count=0
        //   GET …/messages?labels=unauthenticated -> count=0
        // The credential could fetch the unauthenticated message BY ID, so it held
        // label_unauthenticated_read; the list endpoints hid it anyway.
        let rows = [
            v(&["sent"]),
            v(&["sent"]),
            v(&["sent"]),
            v(&["received", "unread", "unauthenticated"]),
        ];
        let grants = all_label_flags();
        let access = LabelAccess::list(&grants, IncludeFlags::NONE);
        let excluded = excluded_labels(&access);

        // The storage predicate, applied as a query would apply it.
        let count = |filter: &[&str]| {
            rows.iter()
                .filter(|row| !row.iter().any(|l| excluded.contains(&l.as_str())))
                .filter(|row| filter.iter().all(|f| row.iter().any(|l| l == f)))
                .count()
        };
        assert_eq!(count(&[]), 3, "fixture 09b: count=3, the sent mail only");
        assert_eq!(count(&["received"]), 0, "fixture 09b: count=0");
        assert_eq!(count(&["unauthenticated"]), 0, "fixture 09b: count=0");

        // An explicit label filter is a filter, never a grant.
        assert!(!admits(&rows[3], &access));
        // The include_* flag, not the filter, is what admits it. [INFERRED]
        let flagged = LabelAccess::list(&grants, IncludeFlags::ALL);
        assert!(admits(&rows[3], &flagged));
    }

    /// The `count=` on a fixture-20 section-D observation line, plus the rest of that line.
    ///
    /// The fixture is the authority; a comment quoting it is a copy that can rot. This reads the
    /// capture at test time so that editing the file to say something else fails the test rather
    /// than silently disagreeing with it.
    fn fixture_20_observation(needle: &str) -> (u64, String) {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../reference/fixtures/20-search-and-label-precedence.txt"
        );
        let text = std::fs::read_to_string(path).expect("fixture 20 is on disk");
        let line = text
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("fixture 20 no longer records {needle:?}"));
        let (_, after) = line.split_once("count=").expect("the line reports a count");
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        (digits.parse().expect("count is a number"), line.to_string())
    }

    #[test]
    fn fixture_20_search_returns_the_message_the_list_endpoint_hid() {
        // reference/fixtures/20-search-and-label-precedence.txt (D). Same inbox, same unrestricted
        // credential, same moment: PATCH add_labels=["spam"], then search still finds the message
        // while the plain list no longer contains it.
        let (search_before, _) = fixture_20_observation("search BEFORE");
        let (search_after, after_line) = fixture_20_observation("search AFTER");
        let (_, list_line) = fixture_20_observation("plain list");
        assert!(search_before > 0, "the probe found the message before labelling it");
        assert_eq!(
            search_after, search_before,
            "fixture 20 records search returning the same count after the spam label as before"
        );
        assert!(after_line.contains("STILL FOUND"));
        assert!(list_line.contains("message ABSENT"));

        let grants = KeyGrants::Unrestricted;
        let row = v(&["received", "unread", "spam"]);

        let listed = LabelAccess::list(&grants, IncludeFlags::from_params(&ListParams::default()));
        assert!(!admits(&row, &listed), "the list endpoint hid it");

        let searched = LabelAccess::search(&grants);
        assert!(admits(&row, &searched), "search returned it");
        assert!(
            excluded_labels(&searched).is_empty(),
            "a search query must push down NO label exclusion for this credential — hiding mail \
             the reference API returns is a conformance failure, not a safe default"
        );
    }

    #[test]
    fn search_is_permission_gated_exactly_like_get_by_id() {
        // [INFERRED], fail-closed: fixture 20's probe key was unrestricted, so only the
        // include-flag half of the rule was observed. The permission half is read across from 09b,
        // where the get-by-id path needed it.
        let row = v(&["received", "spam"]);
        let permitted = reader(true, false, false, false);
        let denied = no_label_flags();
        for (grants, expected) in [(&permitted, true), (&denied, false)] {
            assert_eq!(admits(&row, &LabelAccess::search(grants)), expected);
            assert_eq!(
                admits(&row, &LabelAccess::by_id(grants)),
                expected,
                "search and get-by-id must reach the same verdict"
            );
        }
        let denial = admit(&row, &LabelAccess::search(&denied)).unwrap_err();
        assert_eq!(denial.kind(), DenialKind::NotPermitted);
        assert_eq!(excluded_labels(&LabelAccess::search(&denied)), RESTRICTED.to_vec());
        assert_eq!(
            excluded_labels(&LabelAccess::search(&permitted)),
            vec![labels::BLOCKED, labels::UNAUTHENTICATED, labels::TRASH]
        );
    }

    #[test]
    fn only_a_list_path_can_withhold_for_a_missing_flag() {
        // The include_* parameter exists on 4 of the 33 paginated GETs. A path that has no such
        // parameter must never be routed through the list rule: `IncludeFlags::NONE` would be
        // permanent and no request could ever lift it.
        let grants = all_label_flags();
        let row = v(&["spam"]);
        assert_eq!(
            admit(&row, &LabelAccess::list(&grants, IncludeFlags::NONE))
                .unwrap_err()
                .kind(),
            DenialKind::NotRequested
        );
        for access in [LabelAccess::search(&grants), LabelAccess::by_id(&grants)] {
            assert!(admits(&row, &access), "no flag exists to be missing on this path");
        }
    }

    #[test]
    fn get_by_id_needs_the_permission_but_never_a_flag() {
        // Same fixture: the unauthenticated message was retrievable by id with no include_* flag
        // in sight, and a credential without the label-read permission gets not_found instead.
        let row = v(&["received", "unread", "unauthenticated"]);
        let permitted = reader(false, false, true, false);
        assert!(admits(&row, &LabelAccess::by_id(&permitted)));

        let denied = no_label_flags();
        let denial = admit(&row, &LabelAccess::by_id(&denied)).unwrap_err();
        assert_eq!(denial.kind(), DenialKind::NotPermitted);
        assert_eq!(denial.label(), labels::UNAUTHENTICATED);
        assert_eq!(
            denial.error_code(),
            ErrorCode::NotFound,
            "a label denial must not confirm the resource exists"
        );
        assert_eq!(denial.error_code().status(), 404);
    }

    // --- admission is a storage predicate -----------------------------------------------------

    #[test]
    fn the_storage_predicate_and_the_in_memory_verdict_never_disagree() {
        // The pushed-down WHERE clause and admit() must reach the same answer for every
        // credential, every request and every combination of restricted labels on a row —
        // otherwise a row excluded in memory is fetched by the query, or vice versa.
        for perm_bits in 0u8..16 {
            let grants = reader(
                perm_bits & 1 != 0,
                perm_bits & 2 != 0,
                perm_bits & 4 != 0,
                perm_bits & 8 != 0,
            );
            // 0..16 are the list queries; 16 is search and 17 is get-by-id, neither of which has
            // a flag to vary.
            for req_bits in 0u8..18 {
                let access = match req_bits {
                    16 => LabelAccess::search(&grants),
                    17 => LabelAccess::by_id(&grants),
                    bits => LabelAccess::list(
                        &grants,
                        IncludeFlags::from_flags(
                            bits & 1 != 0,
                            bits & 2 != 0,
                            bits & 4 != 0,
                            bits & 8 != 0,
                        ),
                    ),
                };
                let excluded = excluded_labels(&access);
                for row_bits in 0u8..16 {
                    let mut row = vec!["received".to_string()];
                    for (i, label) in RESTRICTED.iter().enumerate() {
                        if row_bits & (1 << i) != 0 {
                            row.push((*label).to_string());
                        }
                    }
                    let by_query = !row.iter().any(|l| excluded.contains(&l.as_str()));
                    assert_eq!(
                        by_query,
                        admits(&row, &access),
                        "predicate/verdict drift: perms={perm_bits:04b} req={req_bits:04b} row={row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_caller_that_may_see_everything_excludes_nothing() {
        let grants = all_label_flags();
        assert!(excluded_labels(&LabelAccess::list(&grants, IncludeFlags::ALL)).is_empty());
        assert!(excluded_labels(&LabelAccess::search(&grants)).is_empty());
        assert!(excluded_labels(&LabelAccess::by_id(&grants)).is_empty());
        assert!(excluded_labels(&LabelAccess::search(&KeyGrants::Unrestricted)).is_empty());
        assert!(excluded_labels(&LabelAccess::by_id(&KeyGrants::Unrestricted)).is_empty());

        // ...and the default list query excludes all four, even for that caller.
        let bare = LabelAccess::list(&grants, IncludeFlags::from_params(&ListParams::default()));
        assert_eq!(excluded_labels(&bare), RESTRICTED.to_vec());
    }

    // --- thread redaction ---------------------------------------------------------------------

    /// amk-core does not depend on chrono, so timestamps come in the way the wire delivers them.
    fn ts(rfc3339: &str) -> Timestamp {
        serde_json::from_str(&format!("\"{rfc3339}\"")).expect("test timestamp must parse")
    }

    fn message(id: &str, labels: &[&str], from: &str, to: &[&str], size: u64) -> Message {
        let stamp = ts(&format!("2026-08-15T05:54:{:02}.000Z", size % 60));
        Message {
            item: MessageItem {
                organization_id: None,
                pod_id: None,
                inbox_id: InboxId::new("amk-probe@agentmail.to"),
                thread_id: ThreadId::new_random(),
                message_id: MessageId::new(id),
                labels: v(labels),
                timestamp: stamp,
                from: from.to_string(),
                to: to.iter().map(|s| (*s).to_string()).collect(),
                cc: None,
                bcc: None,
                subject: Some(format!("subject {id}")),
                preview: Some(format!("preview {id}")),
                attachments: None,
                in_reply_to: None,
                references: None,
                headers: None,
                smtp_id: None,
                size,
                updated_at: stamp,
                created_at: stamp,
            },
            reply_to: None,
            text: None,
            html: None,
            extracted_text: None,
            extracted_html: None,
        }
    }

    fn at(mut m: Message, second: u32) -> Message {
        let stamp = ts(&format!("2026-08-15T05:54:{second:02}.000Z"));
        m.item.timestamp = stamp;
        m.item.updated_at = stamp;
        m.item.created_at = stamp;
        m
    }

    fn with_cc(mut m: Message, cc: &[&str]) -> Message {
        m.item.cc = Some(cc.iter().map(|s| (*s).to_string()).collect());
        m
    }

    fn with_attachment(mut m: Message, filename: &str) -> Message {
        m.item.attachments = Some(vec![Attachment {
            attachment_id: AttachmentId::new_random(),
            filename: Some(filename.to_string()),
            size: 7,
            content_type: Some("text/plain".to_string()),
            content_disposition: None,
            content_id: None,
        }]);
        m
    }

    /// The filenames of an attachment list, which is what a test can name; `AttachmentId` is a
    /// random UUID.
    fn filenames(attachments: &Option<Vec<Attachment>>) -> Vec<String> {
        attachments
            .iter()
            .flatten()
            .map(|a| a.filename.clone().unwrap_or_default())
            .collect()
    }

    fn with_subject(mut m: Message, subject: &str) -> Message {
        m.item.subject = Some(subject.to_string());
        m
    }

    fn distinct(values: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for value in values {
            if !out.contains(&value) {
                out.push(value);
            }
        }
        out
    }

    fn thread(messages: Vec<Message>) -> Thread {
        let last = messages.last().unwrap().clone();
        let attachments: Vec<Attachment> = messages
            .iter()
            .flat_map(|m| m.item.attachments.iter().flatten().cloned())
            .collect();
        Thread {
            item: ThreadItem {
                organization_id: None,
                pod_id: None,
                inbox_id: InboxId::new("amk-probe@agentmail.to"),
                thread_id: ThreadId::new_random(),
                labels: distinct(messages.iter().flat_map(|m| m.item.labels.clone())),
                timestamp: last.item.timestamp,
                received_timestamp: None,
                sent_timestamp: None,
                senders: distinct(messages.iter().map(|m| m.item.from.clone())),
                recipients: distinct(messages.iter().flat_map(|m| {
                    m.item
                        .to
                        .iter()
                        .chain(m.item.cc.iter().flatten())
                        .chain(m.item.bcc.iter().flatten())
                        .cloned()
                        .collect::<Vec<_>>()
                })),
                subject: messages[0].item.subject.clone(),
                preview: last.item.preview.clone(),
                attachments: (!attachments.is_empty()).then_some(attachments),
                last_message_id: last.item.message_id.clone(),
                message_count: messages.len() as u64,
                size: messages.iter().map(|m| m.item.size).sum(),
                updated_at: last.item.updated_at,
                created_at: messages[0].item.created_at,
            },
            messages,
        }
    }

    #[test]
    fn a_fully_visible_thread_is_left_exactly_as_it_was() {
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = thread(vec![
            message("<a@x>", &["received"], "alice@x", &["amk-probe@agentmail.to"], 10),
            message("<b@x>", &["sent"], "amk-probe@agentmail.to", &["alice@x"], 20),
        ]);
        let before = t.clone();
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Unchanged);
        assert_eq!(t, before);
    }

    /// Two visible messages and one hidden one, with every aggregate given enough variety that a
    /// deleted branch changes the answer: an attachment and a recipient that only the hidden
    /// message accounts for, and two surviving `received` members at different timestamps.
    fn mixed_thread() -> Thread {
        thread(vec![
            at(
                with_attachment(
                    message("<a@x>", &["received"], "alice@x", &["amk-probe@agentmail.to"], 10),
                    "att-a.txt",
                ),
                1,
            ),
            at(
                with_cc(
                    message("<b@x>", &["received"], "carol@x", &["amk-probe@agentmail.to"], 20),
                    &["dave@x"],
                ),
                2,
            ),
            at(
                with_attachment(
                    message(
                        "<c@x>",
                        &["received", "spam"],
                        "mallory@evil",
                        &["amk-probe@agentmail.to", "victim@x"],
                        999,
                    ),
                    "att-evil.txt",
                ),
                3,
            ),
        ])
    }

    #[test]
    fn hiding_a_member_recomputes_every_aggregate_that_counted_it() {
        // The defect this pins: filtering `messages` alone leaves message_count, size,
        // last_message_id, senders, recipients, attachments and preview describing the hidden
        // message.
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = mixed_thread();
        assert_eq!(t.item.message_count, 3);
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Redacted);

        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.item.message_count, 2);
        assert_eq!(t.item.size, 30, "size must not include the hidden message");
        assert_eq!(t.item.last_message_id, MessageId::new("<b@x>"));
        assert_eq!(t.item.timestamp, t.messages[1].item.timestamp);
        assert_eq!(t.item.preview.as_deref(), Some("preview <b@x>"));
        assert_eq!(t.item.senders, v(&["alice@x", "carol@x"]), "the hidden sender must be gone");
        assert_eq!(
            t.item.recipients,
            v(&["amk-probe@agentmail.to", "dave@x"]),
            "a recipient only the hidden message named must be gone, a cc'd one must stay"
        );
        assert_eq!(
            filenames(&t.item.attachments),
            v(&["att-a.txt"]),
            "the hidden message's attachment must be gone"
        );
        assert_eq!(
            t.item.received_timestamp,
            Some(t.messages[1].item.timestamp),
            "the LATEST surviving `received` member, not the earliest"
        );
        assert_ne!(t.item.received_timestamp, Some(t.messages[0].item.timestamp));
        assert_eq!(t.item.sent_timestamp, None);
    }

    #[test]
    fn redaction_strips_the_label_it_exists_to_hide() {
        // The disclosure this pins: the body came back with no spam message, message_count
        // recomputed, and `spam` still standing in `labels` — naming exactly the fact the
        // redaction conceals, which is why LabelDenial::label is pub(crate).
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = mixed_thread();
        assert!(t.item.labels.contains(&"spam".to_string()));
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Redacted);

        assert_eq!(t.item.labels, v(&["received"]));
        assert!(
            admits(&t.item.labels, &access),
            "the returned body must satisfy the same admission rule that hid the member"
        );

        // A credential that MAY see spam gets the label and the message both.
        let permitted = reader(true, false, false, false);
        let mut t = mixed_thread();
        assert_eq!(
            redact_thread(&mut t, &LabelAccess::by_id(&permitted)),
            ThreadRedaction::Unchanged
        );
        assert!(t.item.labels.contains(&"spam".to_string()));
    }

    #[test]
    fn a_thread_labelled_beyond_its_members_is_redacted_even_when_every_member_is_visible() {
        // Whether ThreadItem.labels is the union of its members' labels is unobserved (register
        // C2). If it is not, a thread can name `spam` while every member is admissible — and
        // returning it unchanged would leak the label with no member to blame.
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = thread(vec![
            at(message("<a@x>", &["received"], "alice@x", &["amk-probe@agentmail.to"], 10), 1),
            at(message("<b@x>", &["received"], "alice@x", &["amk-probe@agentmail.to"], 20), 2),
        ]);
        t.item.labels.push("spam".to_string());
        // Two aggregates deliberately set to values recomputation would CHANGE, so "no aggregate is
        // re-derived" is a claim the assertions can fail. Without them the early return below could
        // be deleted with every test still green: a mutation run found exactly that. `senders` gains
        // a member nobody sent from (recomputation drops it); `received_timestamp` stays None while
        // both members carry `received` (recomputation would fill it in).
        t.item.senders.push("ghost@nowhere".to_string());
        assert_eq!(t.item.received_timestamp, None);

        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Redacted);
        assert_eq!(t.item.labels, v(&["received"]));
        assert!(admits(&t.item.labels, &access));
        assert_eq!(t.messages.len(), 2, "no member offended, so no member is dropped");
        assert_eq!(t.item.message_count, 2, "and no aggregate is re-derived");
        assert_eq!(t.item.size, 30);
        assert_eq!(
            t.item.senders,
            v(&["alice@x", "ghost@nowhere"]),
            "a labels-only redaction hid no message, so it may not rewrite membership aggregates"
        );
        assert_eq!(
            t.item.received_timestamp, None,
            "and it may not invent a value the unredacted thread did not carry"
        );
    }

    #[test]
    fn redaction_never_rewrites_the_subject() {
        // fixture 16-threading-matrix/a.txt: the thread keeps the ROOT's subject while its reply
        // carries "Re: …". Nothing observed derives a thread subject from current membership, so
        // hiding the root must not promote the reply's subject — that value has no artifact behind
        // it.
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let root = "AMKthreadA d64ee47e";
        let mut t = thread(vec![
            with_subject(
                message("<a-root@probe.test>", &["received", "spam"], "alice@x", &["probe@x"], 10),
                root,
            ),
            with_subject(
                message("<a-reply@probe.test>", &["received"], "bob@x", &["probe@x"], 20),
                "Re: AMKthreadA d64ee47e",
            ),
        ]);
        assert_eq!(t.item.subject.as_deref(), Some(root));
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Redacted);
        assert_eq!(
            t.item.subject.as_deref(),
            Some(root),
            "the root is hidden; its subject is still the thread's"
        );
    }

    #[test]
    fn a_participant_of_both_a_hidden_and_a_visible_message_is_kept() {
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = thread(vec![
            message("<a@x>", &["received"], "alice@x", &["amk-probe@agentmail.to"], 10),
            message("<b@x>", &["received", "trash"], "alice@x", &["amk-probe@agentmail.to"], 20),
        ]);
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Redacted);
        assert_eq!(t.item.senders, vec!["alice@x".to_string()]);
    }

    #[test]
    fn a_thread_with_no_visible_member_is_withheld_entirely() {
        let grants = no_label_flags();
        let access = LabelAccess::by_id(&grants);
        let mut t = thread(vec![message(
            "<a@x>",
            &["received", "unauthenticated"],
            "alice@x",
            &["amk-probe@agentmail.to"],
            10,
        )]);
        assert_eq!(redact_thread(&mut t, &access), ThreadRedaction::Withheld);
        assert!(t.messages.is_empty(), "the handler must answer not_found, not an empty thread");
    }

    // --- mutation: the system-label gate ------------------------------------------------------

    #[test]
    fn the_labels_the_spec_names_are_all_rejected_on_a_thread_patch() {
        // Named from the spec sentence itself — "Cannot add or remove system labels (sent,
        // received, bounced, etc.)" — not from our own constant. Iterating our constant and
        // asserting membership is how `bounced` went missing in the first place.
        for label in ["sent", "received", "bounced"] {
            assert!(is_system(label), "the spec names {label} as a system label");
            assert_eq!(
                system_label_violations(&[label], &[] as &[&str]),
                vec![SystemLabelViolation {
                    field: MutationField::Add,
                    index: 0,
                    label: label.to_string()
                }],
                "adding {label} must be rejected"
            );
            assert_eq!(
                system_label_violations(&[] as &[&str], &[label]),
                vec![SystemLabelViolation {
                    field: MutationField::Remove,
                    index: 0,
                    label: label.to_string()
                }],
                "removing {label} must be rejected"
            );
        }
        assert_eq!(MutationField::Add.as_field_name(), "add_labels");
        assert_eq!(MutationField::Remove.as_field_name(), "remove_labels");
    }

    #[test]
    fn the_system_set_is_exactly_what_the_live_api_rejects() {
        // fixture 19: each of these was PATCHed onto a live message and returned 400.
        for label in [
            labels::SENT,
            labels::RECEIVED,
            labels::BOUNCED,
            labels::SCHEDULED,
        ] {
            assert!(is_system(label), "{label} was rejected live");
        }
        // And each of these was ACCEPTED live — asserting the negative is the half that would
        // have caught the earlier over-inclusion, which no test did.
        for label in RESTRICTED {
            assert!(
                !is_system(label),
                "{label} is restricted, not system — a client may set it (fixture 19)"
            );
        }
        assert!(
            !is_system(labels::UNREAD),
            "removing `unread` is the documented way to mark read"
        );
        assert!(!is_system("project-x"));
        assert_eq!(
            system_labels().len(),
            4,
            "the set is exactly the four observed; adding one needs a probe, not a reason"
        );
    }

    #[test]
    fn a_message_patch_is_gated_exactly_as_a_thread_patch_is() {
        // Inverted after fixture 19. This test previously asserted that messages were UNGATED,
        // reasoning from the spec's silence — which is the reading the live API disproves. A test
        // written from spec prose can encode the misreading and then defend it, so this one now
        // names the fixture instead.
        assert!(!system_label_violations(&["sent"], &[] as &[&str]).is_empty());

        // The documented mark-as-read flow stays legal: `unread` is not a system label.
        let after = apply_mutation(&v(&["received", "unread"]), &["read"], &["unread"]);
        assert_eq!(after, v(&["received", "read"]));

        // A client may set a RESTRICTED label on a message — observed 200 in fixture 19. This is
        // the pairing that matters: restricted and system are independent axes.
        assert!(system_label_violations(&["spam"], &[] as &[&str]).is_empty());
        assert_eq!(
            apply_mutation(&v(&["received"]), &["spam"], &[] as &[&str]),
            v(&["received", "spam"])
        );
    }

    #[test]
    fn all_offending_labels_are_reported_not_just_the_first() {
        // `trash` is deliberately in this input and deliberately NOT in the expectation: it is
        // restricted, not system, so a client may set it (fixture 19).
        let bad = system_label_violations(&["sent", "starred", "trash"], &["received"]);
        assert_eq!(bad.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), ["sent", "received"]);
    }

    #[test]
    fn each_violation_carries_the_position_the_wire_path_names() {
        // fixture 19's verbatim body:
        //   {"code":"custom","message":"Cannot use system label: bounced","path":["add_labels",0]}
        // Field name THEN array index — so amk-http renders [field.as_field_name(), index], and
        // without `index` it could not reproduce the envelope at all.
        let one = system_label_violations(&["bounced"], &[] as &[&str]);
        assert_eq!((one[0].field.as_field_name(), one[0].index), ("add_labels", 0));
        assert_eq!(one[0].message(), "Cannot use system label: bounced");

        // The index is per FIELD. `remove_labels` is validated after `add_labels`, so a running
        // count across both would spell ["remove_labels", 2] for its first element.
        let both = system_label_violations(&["x", "sent"], &["received", "y"]);
        assert_eq!(
            both.iter()
                .map(|b| (b.field.as_field_name(), b.index, b.label.as_str()))
                .collect::<Vec<_>>(),
            [("add_labels", 1, "sent"), ("remove_labels", 0, "received")]
        );

        // The label cannot stand in for the position: it is not unique within one field.
        let dup = system_label_violations(&["sent", "sent"], &[] as &[&str]);
        assert_eq!(dup.iter().map(|b| b.index).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn one_offending_label_rejects_the_whole_mutation() {
        // fixture 19: {"remove_labels":["spam","bounced"]} returned 400 as a unit — the legal
        // `spam` was NOT removed. The gate is a whole-request verdict, so a caller that saw any
        // violation applies nothing; nothing here offers a partially-applied result to reach for.
        let violations = system_label_violations(&[] as &[&str], &["spam", "bounced"]);
        assert_eq!(
            violations
                .iter()
                .map(|b| (b.field.as_field_name(), b.index, b.label.as_str()))
                .collect::<Vec<_>>(),
            [("remove_labels", 1, "bounced")],
            "the legal `spam` at index 0 is not reported, and is not applied either"
        );
        // What partial application would have produced, and did not: the gate runs first and the
        // caller returns validation_error without reaching apply_mutation.
        assert_eq!(
            apply_mutation(&v(&["received", "spam"]), &[] as &[&str], &["spam", "bounced"]),
            v(&["received"])
        );
    }

    #[test]
    fn a_free_form_label_that_merely_resembles_a_system_one_is_allowed() {
        assert!(system_label_violations(
            &["Sent", "spam-suspect", "trashed", "bounce"],
            &[] as &[&str]
        )
        .is_empty());
    }

    // --- mutation: mechanics ------------------------------------------------------------------

    #[test]
    fn remove_wins_over_add_for_a_label_named_in_both() {
        // [TESTED] on a MESSAGE — reference/fixtures/20-search-and-label-precedence.txt (C):
        //   PATCH …/messages/{mid} {"add_labels":["probe-conflict"],
        //                           "remove_labels":["probe-conflict"]}
        //   -> 200  labels: ["received","unread"]      # the label is ABSENT: remove won
        // The rule was previously read across from the thread schema, where [SPEC:openapi] says
        // "Takes priority over `add_labels`"; the message schema says nothing. The generalisation
        // was right, and no longer depends on being right.
        assert_eq!(
            apply_mutation(&v(&["received", "unread"]), &["probe-conflict"], &["probe-conflict"]),
            v(&["received", "unread"])
        );
        assert_eq!(apply_mutation(&v(&["a"]), &["b"], &["b"]), v(&["a"]));
        assert_eq!(apply_mutation(&v(&["a", "b"]), &["b"], &["b"]), v(&["a"]));
    }

    #[test]
    fn mutation_preserves_order_and_appends_a_new_label_once() {
        assert_eq!(
            apply_mutation(&v(&["received", "unread"]), &["urgent"], &[] as &[&str]),
            v(&["received", "unread", "urgent"])
        );
        assert_eq!(
            apply_mutation(&v(&["received"]), &["received", "urgent", "urgent"], &[] as &[&str]),
            v(&["received", "urgent"]),
            "re-adding an existing label is a no-op, and a repeated add lands once"
        );
    }

    #[test]
    fn a_duplicate_already_on_the_record_survives_a_patch_that_did_not_ask_to_change_it() {
        // Collapsing it would rewrite stored labels on a request that asked for nothing — an
        // invented normalisation in a module that declares it performs none.
        assert_eq!(
            apply_mutation(&v(&["a", "a", "b"]), &[] as &[&str], &[] as &[&str]),
            v(&["a", "a", "b"])
        );
        assert_eq!(
            apply_mutation(&v(&["a", "a", "b"]), &["a"], &[] as &[&str]),
            v(&["a", "a", "b"]),
            "adding a label that is already there changes nothing, duplicates included"
        );
        assert_eq!(
            apply_mutation(&v(&["a", "a", "b"]), &[] as &[&str], &["a"]),
            v(&["b"]),
            "removing a label removes every copy of it"
        );
    }

    #[test]
    fn mutation_is_case_exact_and_removing_an_absent_label_is_a_no_op() {
        assert_eq!(apply_mutation(&v(&["spam"]), &[] as &[&str], &["Spam"]), v(&["spam"]));
        assert_eq!(apply_mutation(&v(&["a"]), &[] as &[&str], &["zzz"]), v(&["a"]));
        assert_eq!(
            apply_mutation(&v(&["a"]), &["A"], &[] as &[&str]),
            v(&["a", "A"]),
            "case differences make distinct labels; no normalisation happens here"
        );
    }

    #[test]
    fn the_pipeline_path_sets_restricted_labels_through_the_same_mechanism() {
        let after =
            apply_mutation(&v(&["received", "unread"]), &[labels::UNAUTHENTICATED], &[] as &[&str]);
        assert_eq!(
            after,
            v(&["received", "unread", "unauthenticated"]),
            "matches the stored labels in fixture 09b"
        );
    }
}
