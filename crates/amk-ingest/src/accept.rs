//! Parse, authenticate, thread and persist inbound mail.
//!
//! `mail-parser` / `mail-auth` types stay inside this module. Persist uses only
//! [`amk_store::messages::NewMessage`] and [`amk_store::threads::NewThread`].
//! A join calls [`amk_store::threads::record_member`].

use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;

use amk_core::scope::{Mount, Resolved, Scope};
use amk_core::threading::{
    InMemoryThreadIndex, ReferenceChainThreading, ThreadAssigner, ThreadAssignment, ThreadCandidate,
};
use amk_store::blobs::{BlobStore, FsBlobStore};
use amk_store::messages::{self, NewMessage};
use amk_store::threads::{self, NewThread, ThreadMember};
use amk_store::StoreError;
use amk_types::ids::{AttachmentId, InboxId, MessageId, OrganizationId, PodId, ThreadId};
use amk_types::message::labels;
use amk_types::message::Attachment;
use amk_types::Timestamp;
use chrono::{DateTime, Utc};
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{AuthenticatedMessage, DkimResult, MessageAuthenticator, SpfResult};
use mail_parser::{HeaderName, Message, MessageParser, MimeHeaders, PartType};
use sqlx::PgPool;

use crate::error::IngestError;

/// Envelope facts used for SPF. Header From is stored; MAIL FROM is not.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub mail_from: String,
    pub client_ip: IpAddr,
    pub ehlo_host: String,
}

/// Inbox already accepted at RCPT.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub organization_id: OrganizationId,
    pub pod_id: PodId,
    pub inbox_id: InboxId,
}

/// What [`accept`] persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub message_id: MessageId,
    pub thread_id: ThreadId,
    pub inbox_id: InboxId,
}

/// Inputs for the library persist entry (future HTTP ingest fallback — not a route).
#[derive(Debug, Clone)]
pub struct AcceptRequest<'a> {
    pub raw: &'a [u8],
    pub envelope: Envelope,
    pub dest: Delivery,
    pub max_message_bytes: usize,
    /// The blob the raw bytes were stored under, if they were. Passed IN rather than computed
    /// here: the raw form needs no parse, so `StorePersist` can store it before this function
    /// ever runs, and a row can therefore never point at a raw object that does not exist.
    pub raw_blob_id: Option<String>,
    /// Where DECODED attachment bodies go. Unlike the raw bytes, these only exist after the
    /// parse, so they cannot be passed in the way `raw_blob_id` is -- the store itself has to be.
    /// `None` keeps the metadata-only behaviour (`GET .../attachments/{id}` is then a 404), which
    /// is also what keeps the unit tests filesystem-free.
    pub blobs: Option<&'a FsBlobStore>,
}

/// SPF/DKIM via `mail-auth`, or a test stub. No `mail_auth::` type is public.
#[derive(Clone)]
pub struct Authenticator {
    inner: AuthInner,
}

#[derive(Clone)]
enum AuthInner {
    Live {
        resolver: Box<MessageAuthenticator>,
    },
    /// Always SPF=none, no DKIM pass.
    StubNone,
    /// Always SPF hardfail (09b branch 2): DATA 250, store nothing.
    StubFail,
    /// SPF pass iff **envelope** MAIL FROM domain equals `pass_domain`.
    /// Never inspects header From — that is the case-12 pin.
    StubEnvelope {
        pass_domain: String,
    },
}

/// Internal verdict. `Fail` is 09b hardfail, not a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthOutcome {
    Pass,
    None,
    Hardfail,
}

impl std::fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            AuthInner::Live { .. } => f
                .debug_struct("Authenticator")
                .field("mode", &"live")
                .finish(),
            AuthInner::StubNone => f
                .debug_struct("Authenticator")
                .field("mode", &"none")
                .finish(),
            AuthInner::StubFail => f
                .debug_struct("Authenticator")
                .field("mode", &"fail")
                .finish(),
            AuthInner::StubEnvelope { pass_domain } => f
                .debug_struct("Authenticator")
                .field("mode", &"envelope")
                .field("pass_domain", pass_domain)
                .finish(),
        }
    }
}

impl Authenticator {
    pub fn live() -> Result<Self, IngestError> {
        let resolver = MessageAuthenticator::new_system_conf()
            .or_else(|_| MessageAuthenticator::new_cloudflare())
            .map_err(|e| IngestError::Io(format!("mail-auth resolver: {e}")))?;
        Ok(Self { inner: AuthInner::Live { resolver: Box::new(resolver) } })
    }

    /// SPF=none, no DKIM pass. Tests inject this so case 5 does not depend on live DNS.
    pub fn unresolved_is_none() -> Self {
        Self { inner: AuthInner::StubNone }
    }

    /// SPF hardfail stub. Fixture 09b branch 2: gateway 250, store nothing.
    pub fn spf_fail() -> Self {
        Self { inner: AuthInner::StubFail }
    }

    /// SPF pass only when envelope MAIL FROM's domain is `pass_domain` (ASCII-folded).
    /// Header From is not consulted.
    pub fn envelope_spf_pass(pass_domain: impl Into<String>) -> Self {
        Self { inner: AuthInner::StubEnvelope { pass_domain: pass_domain.into() } }
    }

    fn stub_outcome(&self, envelope: &Envelope) -> Option<AuthOutcome> {
        match &self.inner {
            AuthInner::Live { .. } => None,
            AuthInner::StubNone => Some(AuthOutcome::None),
            AuthInner::StubFail => Some(AuthOutcome::Hardfail),
            AuthInner::StubEnvelope { pass_domain } => {
                let domain = envelope_mail_from_domain(&envelope.mail_from);
                if domain.eq_ignore_ascii_case(pass_domain) {
                    Some(AuthOutcome::Pass)
                } else {
                    Some(AuthOutcome::None)
                }
            }
        }
    }

    async fn outcome(&self, raw: &[u8], envelope: &Envelope) -> AuthOutcome {
        if let Some(stub) = self.stub_outcome(envelope) {
            return stub;
        }
        let AuthInner::Live { resolver } = &self.inner else {
            return AuthOutcome::None;
        };
        let sender = envelope.mail_from.as_str();
        let ehlo = if envelope.ehlo_host.is_empty() {
            "localhost"
        } else {
            envelope.ehlo_host.as_str()
        };
        let params = SpfParameters::verify_mail_from(envelope.client_ip, ehlo, "localhost", sender);
        let spf = resolver.verify_spf(params).await;
        if matches!(spf.result(), SpfResult::Fail) {
            return AuthOutcome::Hardfail;
        }
        let spf_pass = matches!(spf.result(), SpfResult::Pass);
        let dkim_pass = match AuthenticatedMessage::parse(raw) {
            Some(msg) => resolver
                .verify_dkim(&msg)
                .await
                .iter()
                .any(|o| o.result() == &DkimResult::Pass),
            None => false,
        };
        if spf_pass || dkim_pass {
            AuthOutcome::Pass
        } else {
            AuthOutcome::None
        }
    }
}

fn envelope_mail_from_domain(mail_from: &str) -> &str {
    mail_from.rsplit_once('@').map(|(_, d)| d).unwrap_or("")
}

/// Persist seam so SMTP session tests that never reach DATA do not need a pool.
///
/// `Ok(None)` is SPF hardfail: the session still answers 250 and stores nothing.
pub trait Persist: Send + Sync {
    fn persist(
        &self,
        raw: &[u8],
        envelope: &Envelope,
        dest: &Delivery,
        max_message_bytes: usize,
    ) -> impl Future<Output = Result<Option<Accepted>, IngestError>> + Send;
}

/// [`accept`] against a real pool.
#[derive(Clone)]
pub struct StorePersist {
    pub pool: PgPool,
    pub auth: Authenticator,
    /// Where the original bytes go. `None` keeps the pre-blob behaviour -- parse and discard --
    /// so a deployment without a configured blob root still accepts mail rather than refusing it.
    /// `GET .../raw` is then a 404, which is the honest answer.
    pub blobs: Option<FsBlobStore>,
}

impl Persist for StorePersist {
    async fn persist(
        &self,
        raw: &[u8],
        envelope: &Envelope,
        dest: &Delivery,
        max_message_bytes: usize,
    ) -> Result<Option<Accepted>, IngestError> {
        // The raw bytes go to the blob store BEFORE the row is written, so a row can never point
        // at an object that does not exist. The other order fails in the worse direction: a
        // `raw_blob_id` referring to nothing is a 404 on a message the API otherwise serves
        // happily, which reads as data loss rather than as a missing feature.
        //
        // A blob-store failure is NOT fatal to the delivery. The message itself is fine; refusing
        // it would turn "the disk is full" into "we reject your mail", and an MTA that bounces on
        // a local storage problem loses mail that a 4xx would have had redelivered. Logged loudly,
        // stored without its raw.
        let raw_blob_id = match &self.blobs {
            None => None,
            Some(store) => match store.put(raw).await {
                Ok(id) => Some(id.to_string()),
                Err(e) => {
                    tracing::error!(error = %e, "could not store raw MIME; accepting without it");
                    None
                }
            },
        };
        accept(
            &self.pool,
            &self.auth,
            AcceptRequest {
                raw,
                envelope: envelope.clone(),
                dest: dest.clone(),
                max_message_bytes,
                raw_blob_id,
                blobs: self.blobs.as_ref(),
            },
        )
        .await
    }
}

fn reject(message: &str) -> IngestError {
    IngestError::rejected(554, message)
}

/// Parse + auth + persist. Size `cap` is accepted; `cap + 1` is rejected.
///
/// `Ok(None)` is 09b SPF hardfail: accepted at the gateway, not stored.
pub async fn accept(
    pool: &PgPool,
    auth: &Authenticator,
    req: AcceptRequest<'_>,
) -> Result<Option<Accepted>, IngestError> {
    if req.raw.len() > req.max_message_bytes {
        return Err(IngestError::rejected(552, "Message size exceeds limit"));
    }

    let parsed = MessageParser::default()
        .parse(req.raw)
        .ok_or_else(|| reject("Malformed message"))?;
    reject_hostile(req.raw, &parsed)?;

    let from_headers: Vec<_> = parsed.header_values(HeaderName::From).collect();
    if from_headers.len() > 1 {
        return Err(reject("Multiple From headers"));
    }
    let from = format_addresses(parsed.from());
    if from.is_empty() {
        return Err(reject("Missing From"));
    }
    if has_crlf(&from) {
        return Err(reject("CR/LF in From"));
    }

    let to = address_list(parsed.to());
    if to.iter().any(|a| has_crlf(a)) {
        return Err(reject("CR/LF in To"));
    }

    let subject = normalize_subject(parsed.subject());
    if subject.as_deref().is_some_and(has_crlf) {
        return Err(reject("CR/LF in Subject"));
    }

    let raw_headers = raw_header_map(req.raw, &parsed);
    if header_value_has_crlf(&raw_headers, "From") {
        return Err(reject("CR/LF in From"));
    }
    if header_value_has_crlf(&raw_headers, "To") {
        return Err(reject("CR/LF in To"));
    }
    if header_value_has_crlf(&raw_headers, "Subject") {
        return Err(reject("CR/LF in Subject"));
    }

    let Some(mid_raw) = parsed.message_id() else {
        return Err(reject("Missing Message-ID"));
    };
    let message_id = MessageId::bracketed(mid_raw.trim());
    if message_id.as_str().trim().is_empty() || message_id.as_str() == "<>" {
        return Err(reject("Missing Message-ID"));
    }

    let collected = collect_attachments(&parsed)?;
    // Bodies to the blob store BEFORE the row is written, mirroring the raw-blob ordering in
    // `StorePersist::persist` and for the same reason: the map may under-promise (a store failure
    // drops that entry, loudly) but must never point at an object that does not exist. Failure is
    // non-fatal per body, not per message -- one unwritable attachment must not take down a
    // delivery, and must not take the OTHER attachments' bodies with it.
    let mut attachment_blobs: Option<BTreeMap<String, String>> = None;
    if let (Some(store), Some(pairs)) = (req.blobs, collected.as_ref()) {
        let mut map = BTreeMap::new();
        for (meta, body) in pairs {
            match store.put(body).await {
                Ok(id) => {
                    map.insert(meta.attachment_id.to_string(), id.to_string());
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        attachment = %meta.attachment_id,
                        "could not store attachment body; keeping metadata without it"
                    );
                }
            }
        }
        attachment_blobs = (!map.is_empty()).then_some(map);
    }
    let attachments = collected.map(|pairs| pairs.into_iter().map(|(m, _)| m).collect::<Vec<_>>());

    let in_reply_to = structured_in_reply_to(&parsed);
    let references = structured_references(&parsed);
    let headers = inbound_header_map(&raw_headers);

    let text = keep_body(parsed.body_text(0).as_deref());
    let html = keep_body(parsed.body_html(0).as_deref());
    let preview = keep_body(parsed.body_preview(200).as_deref());
    let extracted_text = extracted_from_body(text.as_deref());
    let extracted_html = None;
    let cc = nonempty_list(address_list(parsed.cc()));
    let bcc = nonempty_list(address_list(parsed.bcc()));
    let reply_to = nonempty_list(address_list(parsed.reply_to()));

    let timestamp = parsed
        .date()
        .and_then(|d| DateTime::<Utc>::from_timestamp(d.to_timestamp(), 0))
        .map(Timestamp::from)
        .unwrap_or_else(Timestamp::now);

    let outcome = auth.outcome(req.raw, &req.envelope).await;
    if outcome == AuthOutcome::Hardfail {
        return Ok(None);
    }
    let msg_labels = inbound_labels(outcome == AuthOutcome::Pass);

    let filter = dest_filter(&req.dest);
    if messages::get(pool, &filter, &req.dest.inbox_id, &message_id, &[])
        .await?
        .is_some()
    {
        return Err(reject("Duplicate Message-ID"));
    }

    let mut index = InMemoryThreadIndex::new();
    let mut lookup_ids: Vec<MessageId> = Vec::new();
    if let Some(id) = &in_reply_to {
        lookup_ids.push(id.clone());
    }
    if let Some(refs) = &references {
        lookup_ids.extend(refs.iter().cloned());
    }
    fill_thread_index(pool, &filter, &req.dest.inbox_id, &lookup_ids, &mut index).await?;

    let refs_slice = references.as_deref().unwrap_or(&[]);
    let mut candidate = ThreadCandidate::new(&req.dest.inbox_id)
        .with_message_id(&message_id)
        .with_references(refs_slice);
    if let Some(irt) = in_reply_to.as_ref() {
        candidate = candidate.with_in_reply_to(irt);
    }
    let assignment = ReferenceChainThreading.assign(&index, &candidate);
    let (thread_id, new_thread) = match assignment {
        ThreadAssignment::Existing { thread_id, .. } => (thread_id, false),
        ThreadAssignment::New(_) => (ThreadId::new_random(), true),
    };

    let size = req.raw.len() as u64;
    if new_thread {
        threads::insert(
            pool,
            NewThread {
                thread_id,
                organization_id: req.dest.organization_id.clone(),
                pod_id: req.dest.pod_id,
                inbox_id: req.dest.inbox_id.clone(),
                labels: msg_labels.clone(),
                timestamp,
                received_timestamp: Some(timestamp),
                sent_timestamp: None,
                senders: vec![from.clone()],
                recipients: to.clone(),
                subject: subject.clone(),
                preview: preview.clone(),
                last_message_id: message_id.clone(),
                message_count: 1,
                size,
            },
        )
        .await?;
    }

    let insert_result = messages::insert(
        pool,
        NewMessage {
            inbox_id: req.dest.inbox_id.clone(),
            message_id: message_id.clone(),
            organization_id: req.dest.organization_id.clone(),
            pod_id: req.dest.pod_id,
            thread_id,
            labels: msg_labels,
            timestamp,
            from: from.clone(),
            to: to.clone(),
            cc,
            bcc,
            subject,
            preview: preview.clone(),
            attachments,
            in_reply_to,
            references,
            headers,
            smtp_id: None,
            size,
            reply_to,
            text,
            html,
            extracted_text,
            extracted_html,
            raw_blob_id: req.raw_blob_id.clone(),
            attachment_blobs,
        },
    )
    .await;

    match insert_result {
        Ok(()) => {
            if !new_thread {
                threads::record_member(
                    pool,
                    thread_id,
                    ThreadMember {
                        last_message_id: message_id.clone(),
                        timestamp,
                        sent_timestamp: None,
                        sender: from,
                        recipients: to,
                        preview,
                        size,
                    },
                )
                .await?;
            }
            Ok(Some(Accepted { message_id, thread_id, inbox_id: req.dest.inbox_id.clone() }))
        }
        Err(e) if is_unique_violation(&e) => Err(reject("Duplicate Message-ID")),
        Err(e) => Err(e.into()),
    }
}

/// Mutant 3 target: SPF=none / no DKIM pass writes `unauthenticated`.
fn inbound_labels(authenticated: bool) -> Vec<String> {
    let mut out = vec![labels::RECEIVED.to_string(), labels::UNREAD.to_string()];
    if !authenticated {
        out.push(labels::UNAUTHENTICATED.to_string());
    }
    out
}

async fn fill_thread_index(
    pool: &PgPool,
    filter: &amk_core::scope::ScopeFilter,
    inbox_id: &InboxId,
    ids: &[MessageId],
    index: &mut InMemoryThreadIndex,
) -> Result<(), IngestError> {
    let mut set = tokio::task::JoinSet::new();
    for id in ids {
        let pool = pool.clone();
        let filter = filter.clone();
        let inbox = inbox_id.clone();
        let key = MessageId::bracketed(id.as_str().trim());
        set.spawn(async move { messages::get(&pool, &filter, &inbox, &key, &[]).await });
    }
    while let Some(joined) = set.join_next().await {
        let existing =
            joined.map_err(|e| IngestError::Io(format!("thread index lookup: {e}")))??;
        if let Some(existing) = existing {
            index.insert(inbox_id.clone(), &existing.item.message_id, existing.item.thread_id);
        }
    }
    Ok(())
}

fn dest_filter(dest: &Delivery) -> amk_core::scope::ScopeFilter {
    let scope = Scope::Inbox {
        organization_id: dest.organization_id.clone(),
        pod_id: dest.pod_id,
        inbox_id: dest.inbox_id.clone(),
    };
    match scope.resolve(&Mount::Organization) {
        Ok(Resolved::Ready(f)) => f,
        other => {
            panic!("invariant: inbox scope on organization mount is Ready, got {other:?}")
        }
    }
}

fn is_unique_violation(err: &StoreError) -> bool {
    match err {
        StoreError::Database(sqlx::Error::Database(db)) => db.is_unique_violation(),
        _ => false,
    }
}

fn reject_hostile(raw: &[u8], msg: &Message<'_>) -> Result<(), IngestError> {
    if header_section_is_not_utf8(raw) {
        return Err(reject("8-bit in header"));
    }
    let top = msg.headers();
    if !top.iter().any(|h| h.name == HeaderName::ContentType) {
        return Err(reject("Missing Content-Type"));
    }
    if conflicting_cte(top) {
        return Err(reject("Conflicting Content-Transfer-Encoding"));
    }
    if unterminated_multipart(raw, msg) {
        return Err(reject("Unterminated multipart boundary"));
    }
    if multipart_depth(msg, 0, 0) > 3 || msg.parts.len() > 64 {
        return Err(reject("Nested multipart too deep"));
    }
    if msg.parts.iter().any(|p| p.is_encoding_problem) {
        return Err(reject("Malformed MIME"));
    }
    for part in &msg.parts {
        if conflicting_cte(&part.headers) {
            return Err(reject("Conflicting Content-Transfer-Encoding"));
        }
    }
    Ok(())
}

/// RFC 5322 headers are ASCII; a raw 8-bit byte that is not valid UTF-8 is hostile.
/// Valid UTF-8 (case 7 homoglyph subjects) is not this reject.
fn header_section_is_not_utf8(raw: &[u8]) -> bool {
    let end = double_nl(raw).unwrap_or(raw.len());
    std::str::from_utf8(&raw[..end]).is_err()
}

fn double_nl(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n"))
}

fn conflicting_cte(headers: &[mail_parser::Header<'_>]) -> bool {
    let ctes: Vec<&str> = headers
        .iter()
        .filter(|h| h.name == HeaderName::ContentTransferEncoding)
        .filter_map(|h| h.value.as_text())
        .collect();
    if ctes.len() > 1 {
        return true;
    }
    ctes.first()
        .is_some_and(|v| v.contains(',') || v.split_whitespace().count() > 1)
}

fn unterminated_multipart(raw: &[u8], msg: &Message<'_>) -> bool {
    let Some(ct) = msg.content_type() else {
        return false;
    };
    if !ct.ctype().eq_ignore_ascii_case("multipart") {
        return false;
    }
    let Some(boundary) = ct.attribute("boundary") else {
        return true;
    };
    let closer = format!("--{boundary}--");
    !find_bytes(raw, closer.as_bytes())
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn multipart_depth(msg: &Message<'_>, part_id: u32, depth: usize) -> usize {
    let Some(part) = msg.parts.get(part_id as usize) else {
        return depth;
    };
    match &part.body {
        PartType::Multipart(ids) => ids
            .iter()
            .map(|&id| multipart_depth(msg, id, depth + 1))
            .max()
            .unwrap_or(depth + 1),
        PartType::Message(inner) => multipart_depth(inner, 0, depth + 1),
        _ => depth,
    }
}

fn format_addresses(addr: Option<&mail_parser::Address<'_>>) -> String {
    address_list(addr).into_iter().next().unwrap_or_default()
}

fn address_list(addr: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    let Some(addr) = addr else {
        return Vec::new();
    };
    addr.iter()
        .filter_map(|a| {
            let formatted = match (a.name(), a.address()) {
                (Some(n), Some(ad)) if !n.is_empty() => format!("{n} <{ad}>"),
                (_, Some(ad)) => ad.to_string(),
                (Some(n), None) => n.to_string(),
                _ => return None,
            };
            Some(formatted)
        })
        .collect()
}

fn normalize_subject(subject: Option<&str>) -> Option<String> {
    let s = subject?.trim_end();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn has_crlf(s: &str) -> bool {
    s.bytes().any(|b| b == b'\r' || b == b'\n')
}

fn header_value_has_crlf(headers: &BTreeMap<String, String>, name: &str) -> bool {
    headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(name) && has_crlf(v))
}

/// Keep the parsed body byte-for-byte, including a trailing newline. Omit only `""`.
fn keep_body(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

/// Fixture 21: `extracted_text` is the body without the trailing newline; `text` keeps it.
fn extracted_from_body(text: Option<&str>) -> Option<String> {
    let t = text?;
    let stripped = t.strip_suffix('\n').unwrap_or(t);
    let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_owned())
    }
}

/// Live inbound `headers` is `In-Reply-To` when present. From/To/Subject/Message-ID/
/// Content-Type stay off this map — they have first-class fields.
fn inbound_header_map(raw: &BTreeMap<String, String>) -> Option<BTreeMap<String, String>> {
    raw.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("In-Reply-To"))
        .map(|(k, v)| {
            let mut map = BTreeMap::new();
            map.insert(k.clone(), v.clone());
            map
        })
}

fn nonempty_list(v: Vec<String>) -> Option<Vec<String>> {
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn structured_in_reply_to(msg: &Message<'_>) -> Option<MessageId> {
    let value = msg.in_reply_to();
    let id = value.as_text().or_else(|| {
        value
            .as_text_list()
            .and_then(|l| l.first().map(|s| s.as_ref()))
    })?;
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed == "<>" {
        return None;
    }
    Some(MessageId::bracketed(trimmed))
}

fn structured_references(msg: &Message<'_>) -> Option<Vec<MessageId>> {
    let value = msg.references();
    let ids: Vec<MessageId> = match value {
        mail_parser::HeaderValue::Text(s) => {
            let t = s.trim();
            if t.is_empty() || t == "<>" {
                Vec::new()
            } else {
                vec![MessageId::bracketed(t)]
            }
        }
        mail_parser::HeaderValue::TextList(list) => list
            .iter()
            .filter_map(|s| {
                let t = s.trim();
                if t.is_empty() || t == "<>" {
                    None
                } else {
                    Some(MessageId::bracketed(t))
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn raw_header_map(raw: &[u8], msg: &Message<'_>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for header in msg.headers() {
        let start = header.offset_start as usize;
        let end = header.offset_end as usize;
        let Some(slice) = raw.get(start..end) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(slice) else {
            continue;
        };
        let value = value.trim_end_matches(['\r', '\n']).trim_start();
        map.insert(header.name.as_str().to_string(), value.to_string());
    }
    map
}

/// Metadata plus the DECODED body bytes of each attachment, in the same order.
///
/// The body comes back alongside the metadata rather than from a second walk, because the pairing
/// is by position in `msg.attachments()` and two walks would have to re-derive it -- an off-by-one
/// there stores one attachment's bytes under another's id, which is a cross-attachment disclosure
/// inside a message rather than a crash. One walk makes the pairing correct by construction.
/// Metadata paired with its decoded body — the return shape of [`collect_attachments`].
type CollectedAttachments = Vec<(Attachment, Vec<u8>)>;

fn collect_attachments(msg: &Message<'_>) -> Result<Option<CollectedAttachments>, IngestError> {
    let mut out = Vec::new();
    for part in msg.attachments() {
        let filename = part.attachment_name().map(ToOwned::to_owned);
        if let Some(name) = filename.as_deref() {
            if name.contains('\0') || name.contains("..") {
                return Err(reject("Illegal attachment filename"));
            }
        }
        let size = part_len(part);
        let content_type = part.content_type().map(|ct| match ct.subtype() {
            Some(st) => format!("{}/{}", ct.ctype(), st),
            None => ct.ctype().to_string(),
        });
        let content_disposition = part.content_disposition().map(|cd| cd.ctype().to_string());
        let content_id = part.content_id().map(ToOwned::to_owned);
        out.push((
            Attachment {
                attachment_id: AttachmentId::new_random(),
                filename,
                size,
                content_type,
                content_disposition,
                content_id,
            },
            part_body(part),
        ));
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

/// The decoded bytes of a part -- what `part_len` measures, so the stored object's length always
/// equals the `size` the metadata advertises.
fn part_body(part: &mail_parser::MessagePart<'_>) -> Vec<u8> {
    match &part.body {
        PartType::Text(t) | PartType::Html(t) => t.as_bytes().to_vec(),
        PartType::Binary(b) | PartType::InlineBinary(b) => b.to_vec(),
        PartType::Message(m) => m.raw_message().to_vec(),
        PartType::Multipart(_) => Vec::new(),
    }
}

fn part_len(part: &mail_parser::MessagePart<'_>) -> u64 {
    match &part.body {
        PartType::Text(t) | PartType::Html(t) => t.len() as u64,
        PartType::Binary(b) | PartType::InlineBinary(b) => b.len() as u64,
        PartType::Message(m) => m.raw_message().len() as u64,
        PartType::Multipart(_) => 0,
    }
}
