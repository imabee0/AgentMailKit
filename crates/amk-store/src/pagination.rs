//! Keyset pagination: the cursor shape observed in `reference/fixtures/04-pagination.http`, and
//! the scope check that stops one scope's token being replayed against another.
//!
//! # Why keyset, not OFFSET
//!
//! `amk_types::page::Cursor` already carries the `base64(JSON)` encode/decode; this module adds
//! the typed shape observed live — `{message_id, inbox_id, timestamp}` for messages, the same
//! structural shape with `thread_id` for threads — and the query-building half: a keyset
//! comparison never needs the referenced row to still exist, so replaying a token after its row
//! was deleted still resumes correctly. An OFFSET-based scheme would not have that property, and
//! neither would a scheme that re-fetched the cursor row to resume from it.
//!
//! # Why `WrongScope` is checked here, before any query runs
//!
//! The `WHERE` clause always uses the request's own [`amk_core::scope::ScopeFilter`] to pin
//! `inbox_id` — never the token's — so a foreign-scope token cannot smuggle a wider window into
//! the query. `WrongScope` is a distinct, cheap rejection so a client seeing a page reset does
//! not have to distinguish "malformed" from "the wrong page for this credential" from a database
//! error.

use amk_types::ids::{has_forbidden_byte, ApiKeyId, InboxId, MessageId, PodId, ThreadId};
use amk_types::page::Cursor;
use amk_types::Timestamp;
use chrono::{DateTime, SecondsFormat, Utc};

use crate::api_keys::{exact_api_key_uuid, KeyScope};
use crate::error::PageTokenError;

/// A fixed choice of ORDER BY / comparison direction.
///
/// Never interpolated into SQL text: each repository function holds one literal query string per
/// variant and matches on this to pick between them, so the direction can never be built by
/// formatting a runtime value into a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// One page of results, plus the cursor for the next one.
///
/// `next` is `None` on the last page — never `Some(String::new())` — matching
/// `reference/fixtures/03-id-formats.http` and `04-pagination.http`: the wire's
/// `next_page_token` is omitted, not emitted empty, once the scan is exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

fn encode_timestamp(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn decode_timestamp(raw: &str, field: &'static str) -> Result<DateTime<Utc>, PageTokenError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| PageTokenError::WrongType(field))
}

/// Reject a token whose own `inbox_id` disagrees with a pinned request scope. `pinned` is `None`
/// for an org- or pod-level mount that spans multiple inboxes, in which case every token is
/// in-scope by definition (the query itself still pins organization_id/pod_id).
fn check_inbox_scope(
    token_inbox: &InboxId,
    pinned: Option<&InboxId>,
) -> Result<(), PageTokenError> {
    match pinned {
        Some(p) if !token_inbox.eq_normalized(p) => Err(PageTokenError::WrongScope),
        _ => Ok(()),
    }
}

/// The keyset cursor for a messages page: `{message_id, inbox_id, timestamp}`, verbatim from
/// fixture 04.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCursor {
    pub message_id: MessageId,
    pub inbox_id: InboxId,
    pub timestamp: DateTime<Utc>,
}

impl MessageCursor {
    pub fn encode(&self) -> String {
        Cursor::new()
            .with("message_id", self.message_id.as_str())
            .with("inbox_id", self.inbox_id.as_str())
            .with("timestamp", encode_timestamp(self.timestamp))
            .encode()
    }

    /// Decode and validate against the request's pinned inbox, if any.
    pub fn decode(token: &str, pinned_inbox: Option<&InboxId>) -> Result<Self, PageTokenError> {
        let cursor = Cursor::decode(token)?;
        let message_id = cursor
            .get_str("message_id")
            .ok_or(PageTokenError::MissingField("message_id"))?;
        let inbox_id_raw = cursor
            .get_str("inbox_id")
            .ok_or(PageTokenError::MissingField("inbox_id"))?;
        let ts_raw = cursor
            .get_str("timestamp")
            .ok_or(PageTokenError::MissingField("timestamp"))?;
        // A NUL byte percent-decodes (or, here, JSON-decodes) to a perfectly valid UTF-8 string,
        // so it survives `Cursor::decode`'s JSON parse untouched. Reject it here, before either
        // raw field is used to build an id or reaches a query — the same rule
        // `from_path_segment` applies to a URL path segment, applied to this token's own two
        // wire-reachable string fields. `has_forbidden_byte` is `amk-types`' one definition of
        // the rule; this crate does not keep a second copy of it.
        if has_forbidden_byte(message_id) {
            return Err(PageTokenError::ForbiddenByte("message_id"));
        }
        if has_forbidden_byte(inbox_id_raw) {
            return Err(PageTokenError::ForbiddenByte("inbox_id"));
        }
        let timestamp = decode_timestamp(ts_raw, "timestamp")?;
        let inbox_id = InboxId::new(inbox_id_raw).normalized();
        check_inbox_scope(&inbox_id, pinned_inbox)?;
        Ok(Self { message_id: MessageId::new(message_id), inbox_id, timestamp })
    }
}

impl From<(&MessageId, &InboxId, Timestamp)> for MessageCursor {
    fn from((message_id, inbox_id, timestamp): (&MessageId, &InboxId, Timestamp)) -> Self {
        Self {
            message_id: message_id.clone(),
            inbox_id: inbox_id.clone(),
            timestamp: timestamp.into_inner(),
        }
    }
}

/// The keyset cursor for a threads page: same structural shape as [`MessageCursor`], tiebreaking
/// on the thread's own id instead of a message id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCursor {
    pub thread_id: ThreadId,
    pub inbox_id: InboxId,
    pub timestamp: DateTime<Utc>,
}

impl ThreadCursor {
    pub fn encode(&self) -> String {
        Cursor::new()
            .with("thread_id", self.thread_id.to_string())
            .with("inbox_id", self.inbox_id.as_str())
            .with("timestamp", encode_timestamp(self.timestamp))
            .encode()
    }

    pub fn decode(token: &str, pinned_inbox: Option<&InboxId>) -> Result<Self, PageTokenError> {
        let cursor = Cursor::decode(token)?;
        let thread_id_raw = cursor
            .get_str("thread_id")
            .ok_or(PageTokenError::MissingField("thread_id"))?;
        let inbox_id_raw = cursor
            .get_str("inbox_id")
            .ok_or(PageTokenError::MissingField("inbox_id"))?;
        let ts_raw = cursor
            .get_str("timestamp")
            .ok_or(PageTokenError::MissingField("timestamp"))?;
        // See the identical check in `MessageCursor::decode`: `inbox_id` is the one field here
        // that is a free-text string rather than a UUID, so it is the one that can carry a NUL
        // through JSON decoding undetected. `thread_id` needs no matching check: any NUL in it
        // fails `Uuid`'s own parse below as `WrongType`, before it could reach a query.
        if has_forbidden_byte(inbox_id_raw) {
            return Err(PageTokenError::ForbiddenByte("inbox_id"));
        }
        let timestamp = decode_timestamp(ts_raw, "timestamp")?;
        let thread_id = thread_id_raw
            .parse::<uuid::Uuid>()
            .map(ThreadId::from)
            .map_err(|_| PageTokenError::WrongType("thread_id"))?;
        let inbox_id = InboxId::new(inbox_id_raw).normalized();
        check_inbox_scope(&inbox_id, pinned_inbox)?;
        Ok(Self { thread_id, inbox_id, timestamp })
    }
}

impl From<(ThreadId, &InboxId, Timestamp)> for ThreadCursor {
    fn from((thread_id, inbox_id, timestamp): (ThreadId, &InboxId, Timestamp)) -> Self {
        Self { thread_id, inbox_id: inbox_id.clone(), timestamp: timestamp.into_inner() }
    }
}

/// Reject a token whose own `pod_id` disagrees with a pinned request scope. Sibling of
/// [`check_inbox_scope`], same shape, one level up: `pinned` is `None` for the organization mount
/// (every token is in-scope by definition — the query itself still pins `organization_id`).
fn check_pod_scope(token_pod: PodId, pinned: Option<PodId>) -> Result<(), PageTokenError> {
    match pinned {
        Some(p) if token_pod != p => Err(PageTokenError::WrongScope),
        _ => Ok(()),
    }
}

/// The keyset cursor for a pods page: `{created_at, pod_id}`. `pods::list` has exactly one mount
/// (`GET /v0/pods`), so there is no scope to pin and [`PodCursor::decode`] takes no pinned
/// argument — unlike [`InboxCursor`]/[`ApiKeyCursor`]. It also carries no free-text field: `pod_id`
/// is a UUID column, so a NUL byte in it fails the `Uuid` parse below as [`PageTokenError::WrongType`]
/// rather than needing its own [`has_forbidden_byte`] check. Both absences are decisions, not
/// omissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PodCursor {
    pub created_at: DateTime<Utc>,
    pub pod_id: PodId,
}

impl PodCursor {
    pub fn encode(&self) -> String {
        Cursor::new()
            .with("created_at", encode_timestamp(self.created_at))
            .with("pod_id", self.pod_id.to_string())
            .encode()
    }

    pub fn decode(token: &str) -> Result<Self, PageTokenError> {
        let cursor = Cursor::decode(token)?;
        let created_at_raw = cursor
            .get_str("created_at")
            .ok_or(PageTokenError::MissingField("created_at"))?;
        let pod_id_raw = cursor
            .get_str("pod_id")
            .ok_or(PageTokenError::MissingField("pod_id"))?;
        let created_at = decode_timestamp(created_at_raw, "created_at")?;
        let pod_id = pod_id_raw
            .parse::<uuid::Uuid>()
            .map(PodId::from)
            .map_err(|_| PageTokenError::WrongType("pod_id"))?;
        Ok(Self { created_at, pod_id })
    }
}

/// The keyset cursor for an inboxes page: `{created_at, inbox_id, pod_id}`. Two mounts —
/// `GET /v0/inboxes` (organization-wide) and `GET /v0/pods/{pod_id}/inboxes` (pod-pinned) — so
/// [`InboxCursor::decode`] takes `pinned: Option<PodId>`: `None` for the organization mount
/// (accepts any token), `Some(p)` for the pod mount, requiring `cursor.pod_id == p`.
/// `inboxes.pod_id` is `NOT NULL` (migration 0003), so the coordinate is always present on a real
/// row. This is [`check_inbox_scope`]'s exact shape, one level up, via [`check_pod_scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxCursor {
    pub created_at: DateTime<Utc>,
    pub inbox_id: InboxId,
    pub pod_id: PodId,
}

impl InboxCursor {
    pub fn encode(&self) -> String {
        Cursor::new()
            .with("created_at", encode_timestamp(self.created_at))
            .with("inbox_id", self.inbox_id.as_str())
            .with("pod_id", self.pod_id.to_string())
            .encode()
    }

    /// Decode and validate against the request's pinned pod, if any (see the struct doc for what
    /// `pinned` means at each of the two mounts).
    pub fn decode(token: &str, pinned: Option<PodId>) -> Result<Self, PageTokenError> {
        let cursor = Cursor::decode(token)?;
        let created_at_raw = cursor
            .get_str("created_at")
            .ok_or(PageTokenError::MissingField("created_at"))?;
        let inbox_id_raw = cursor
            .get_str("inbox_id")
            .ok_or(PageTokenError::MissingField("inbox_id"))?;
        let pod_id_raw = cursor
            .get_str("pod_id")
            .ok_or(PageTokenError::MissingField("pod_id"))?;
        // `inbox_id` is this cursor's one free-text field — see the identical check (and the same
        // reasoning: a NUL survives the JSON decode as valid UTF-8, then fails at Postgres
        // parameter encoding, SQLSTATE 22021) in `MessageCursor::decode`.
        if has_forbidden_byte(inbox_id_raw) {
            return Err(PageTokenError::ForbiddenByte("inbox_id"));
        }
        let created_at = decode_timestamp(created_at_raw, "created_at")?;
        let pod_id = pod_id_raw
            .parse::<uuid::Uuid>()
            .map(PodId::from)
            .map_err(|_| PageTokenError::WrongType("pod_id"))?;
        let inbox_id = InboxId::new(inbox_id_raw).normalized();
        check_pod_scope(pod_id, pinned)?;
        Ok(Self { created_at, inbox_id, pod_id })
    }
}

/// Reject a token whose own mount coordinates disagree with the `KeyScope` this request is pinned
/// to. Unlike [`check_inbox_scope`]/[`check_pod_scope`], the pinned side here is not a plain
/// `Option<T>` but a [`KeyScope`] — because the *mount* a key list was walked at is not the same
/// thing as an individual key's own scope (`KeyScope::Organization` lists pod- and inbox-scoped
/// keys too; see `KeyScope`'s own doc). `pinned` is collapsed to the same `(Option<PodId>,
/// Option<InboxId>)` pair [`crate::api_keys::scope_params`] already reduces a `KeyScope` to, and
/// the token's own pair must equal it exactly — `Organization` is `(None, None)`, a real
/// checkable value, not "no coordinate".
fn check_key_scope(
    token_pod: Option<PodId>,
    token_inbox: Option<&InboxId>,
    pinned: &KeyScope,
) -> Result<(), PageTokenError> {
    let (pinned_pod, pinned_inbox): (Option<PodId>, Option<&InboxId>) = match pinned {
        KeyScope::Organization => (None, None),
        KeyScope::Pod(p) => (Some(*p), None),
        KeyScope::Inbox(i) => (None, Some(i)),
    };
    // The inbox half is compared with `eq_normalized`, never `==` — fixture 18: two differently
    // cased renderings of the same address must agree.
    let inbox_matches = match (token_inbox, pinned_inbox) {
        (Some(a), Some(b)) => a.eq_normalized(b),
        (None, None) => true,
        _ => false,
    };
    if token_pod == pinned_pod && inbox_matches {
        Ok(())
    } else {
        Err(PageTokenError::WrongScope)
    }
}

/// The keyset cursor for an api-keys page: `{created_at, api_key_id, pod_id?, inbox_id?}`. Unlike
/// [`MessageCursor`]/[`ThreadCursor`]/[`InboxCursor`], `pod_id`/`inbox_id` are each optional on
/// the cursor itself and are omitted from the encoded token when absent (this crate's own
/// optionals-are-omitted convention, not merely the wire's) — together they record the **mount**
/// a page was walked at (`Organization` is `(None, None)`), not an extra tiebreak coordinate:
/// `(created_at, api_key_id)` is already a total order, because `api_key_id` is the table's own
/// primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCursor {
    pub created_at: DateTime<Utc>,
    pub api_key_id: ApiKeyId,
    pub pod_id: Option<PodId>,
    pub inbox_id: Option<InboxId>,
}

impl ApiKeyCursor {
    pub fn encode(&self) -> String {
        let mut cursor = Cursor::new()
            .with("created_at", encode_timestamp(self.created_at))
            .with("api_key_id", self.api_key_id.as_str());
        if let Some(p) = self.pod_id {
            cursor = cursor.with("pod_id", p.to_string());
        }
        if let Some(i) = &self.inbox_id {
            cursor = cursor.with("inbox_id", i.as_str());
        }
        cursor.encode()
    }

    /// Decode and validate against the mount this request is pinned to. `pinned` is the
    /// [`KeyScope`] this call is mounted at — not the individual key's own scope, see
    /// [`check_key_scope`].
    pub fn decode(token: &str, pinned: &KeyScope) -> Result<Self, PageTokenError> {
        let cursor = Cursor::decode(token)?;
        let created_at_raw = cursor
            .get_str("created_at")
            .ok_or(PageTokenError::MissingField("created_at"))?;
        let api_key_id_raw = cursor
            .get_str("api_key_id")
            .ok_or(PageTokenError::MissingField("api_key_id"))?;
        let created_at = decode_timestamp(created_at_raw, "created_at")?;
        let api_key_id = ApiKeyId::new(api_key_id_raw);
        // `api_keys.api_key_id` is a `uuid` column, not `text` — unlike `message_id`/`thread_id`'s
        // own columns — so the value this crate binds into the keyset comparison has to be a
        // `Uuid`, and only the *canonical* rendering of one (see `exact_api_key_uuid`'s own long
        // comment in api_keys.rs for why an alternate rendering must not resolve). Validating that
        // here, at decode, rejects a malformed value uniformly as `WrongType` rather than leaving
        // `api_keys::list` to reason about a cursor that silently binds SQL NULL into its keyset
        // predicate. A NUL byte fails this the same way it fails `ThreadCursor`'s `thread_id`
        // parse — no separate `has_forbidden_byte` check is needed for this field.
        if exact_api_key_uuid(&api_key_id).is_none() {
            return Err(PageTokenError::WrongType("api_key_id"));
        }
        let pod_id = match cursor.get_str("pod_id") {
            Some(raw) => Some(
                raw.parse::<uuid::Uuid>()
                    .map(PodId::from)
                    .map_err(|_| PageTokenError::WrongType("pod_id"))?,
            ),
            None => None,
        };
        let inbox_id = match cursor.get_str("inbox_id") {
            Some(raw) => {
                // Sibling of the identical check in `InboxCursor::decode`: the only free-text
                // field either cursor carries.
                if has_forbidden_byte(raw) {
                    return Err(PageTokenError::ForbiddenByte("inbox_id"));
                }
                Some(InboxId::new(raw).normalized())
            }
            None => None,
        };
        check_key_scope(pod_id, inbox_id.as_ref(), pinned)?;
        Ok(Self { created_at, api_key_id, pod_id, inbox_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-15T05:44:16.768Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn message_cursor_round_trips() {
        let c = MessageCursor {
            message_id: MessageId::new("<a@b.c>"),
            inbox_id: InboxId::new("amk-probe@agentmail.to"),
            timestamp: ts(),
        };
        let token = c.encode();
        let back = MessageCursor::decode(&token, None).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn message_cursor_matches_the_live_fixture_04_shape() {
        // Verbatim token from reference/fixtures/04-pagination.http.
        const LIVE_TOKEN: &str = "eyJtZXNzYWdlX2lkIjoiPDAxMDAwMWEwMDNmMzEyYWMtM2M4MDM0OWUtMGE5Ny00OTE1LTlkZWEtMGQyNzY0ZDc3MjlhLTAwMDAwMEBlbWFpbC5hbWF6b25zZXMuY29tPiIsImluYm94X2lkIjoiYW1rLXByb2JlQGFnZW50bWFpbC50byIsInRpbWVzdGFtcCI6IjIwMjYtMDgtMTVUMDU6NDQ6MTYuNzY4WiJ9";
        let c = MessageCursor::decode(LIVE_TOKEN, None).unwrap();
        assert_eq!(c.inbox_id.as_str(), "amk-probe@agentmail.to");
        assert!(c.message_id.as_str().starts_with('<'));
        assert_eq!(c.timestamp, ts());
    }

    #[test]
    fn message_cursor_accepts_a_matching_pinned_inbox() {
        let c = MessageCursor {
            message_id: MessageId::new("<a@b.c>"),
            inbox_id: InboxId::new("Amk-Probe@agentmail.to"),
            timestamp: ts(),
        };
        let token = c.encode();
        let pinned = InboxId::new("amk-probe@agentmail.to");
        let decoded = MessageCursor::decode(&token, Some(&pinned)).unwrap();
        assert!(decoded.inbox_id.eq_normalized(&pinned));
    }

    #[test]
    fn message_cursor_rejects_a_foreign_inbox_scope() {
        let c = MessageCursor {
            message_id: MessageId::new("<a@b.c>"),
            inbox_id: InboxId::new("mine@agentmail.to"),
            timestamp: ts(),
        };
        let token = c.encode();
        let pinned = InboxId::new("theirs@agentmail.to");
        assert_eq!(MessageCursor::decode(&token, Some(&pinned)), Err(PageTokenError::WrongScope));
    }

    #[test]
    fn message_cursor_rejects_tampered_truncated_and_bad_base64() {
        let c = MessageCursor {
            message_id: MessageId::new("<a@b.c>"),
            inbox_id: InboxId::new("mine@agentmail.to"),
            timestamp: ts(),
        };
        let token = c.encode();

        // Truncated.
        assert!(MessageCursor::decode(&token[..token.len() - 5], None).is_err());
        // Tampered: flip a character but keep it valid base64 length-wise.
        let mut bytes = token.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        // Either it fails to decode, or it decodes to something structurally different — either
        // way it must never silently produce the original cursor.
        if let Ok(decoded) = MessageCursor::decode(&tampered, None) {
            assert_ne!(decoded, c);
        }
        // Not base64 at all.
        assert_eq!(MessageCursor::decode("!!!not base64!!!", None), Err(PageTokenError::Base64));
    }

    #[test]
    fn message_cursor_rejects_missing_fields() {
        let token = Cursor::new()
            .with("inbox_id", "a@b.c")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(
            MessageCursor::decode(&token, None),
            Err(PageTokenError::MissingField("message_id"))
        );
    }

    #[test]
    fn message_cursor_rejects_malformed_timestamp() {
        let token = Cursor::new()
            .with("message_id", "<a@b.c>")
            .with("inbox_id", "a@b.c")
            .with("timestamp", "not-a-timestamp")
            .encode();
        assert_eq!(
            MessageCursor::decode(&token, None),
            Err(PageTokenError::WrongType("timestamp"))
        );
    }

    /// The second of the two wire-reachable entry points into `InboxId`/`MessageId`: a tampered
    /// token whose `inbox_id` carries a NUL. `%00` inside a JSON string decodes to a perfectly
    /// valid UTF-8 `\0` character, so this reaches `decode` as ordinary-looking JSON — the defect
    /// this dispatch closes is that, unguarded, it then reaches a bound Postgres `text` parameter
    /// and fails at *encoding* (`SQLSTATE 22021`) rather than here. Asserted on the error type,
    /// not merely `is_err()`: a `Base64`/`Json`/`WrongType` failure would also make this pass
    /// while leaving the real hole open.
    #[test]
    fn message_cursor_rejects_a_nul_byte_in_inbox_id() {
        let token = Cursor::new()
            .with("message_id", "<a@b.c>")
            .with("inbox_id", "abc\0def")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(
            MessageCursor::decode(&token, None),
            Err(PageTokenError::ForbiddenByte("inbox_id"))
        );
    }

    /// Sibling of [`message_cursor_rejects_a_nul_byte_in_inbox_id`] for the other free-text field.
    #[test]
    fn message_cursor_rejects_a_nul_byte_in_message_id() {
        let token = Cursor::new()
            .with("message_id", "<a\0b@c>")
            .with("inbox_id", "a@b.c")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(
            MessageCursor::decode(&token, None),
            Err(PageTokenError::ForbiddenByte("message_id"))
        );
    }

    /// `decode` normalizes `inbox_id` itself rather than leaving it to the caller: today
    /// [`check_inbox_scope`] compares via `eq_normalized`, so a raw, un-normalized `inbox_id` on
    /// the returned struct would be harmless — but a decoded cursor is meant to be a trustworthy
    /// value once past validation, and the day a caller reaches for `==` instead of
    /// `eq_normalized` (as [`MessageCursor::inbox_id`] and [`ThreadCursor::inbox_id`] are ordinary
    /// public fields, nothing stops that), a non-normalized value silently reopens fixture 18's
    /// case-fold bug one layer up. Kept and tested rather than deleted as "redundant": the
    /// redundancy is exactly the point — defense in depth for a value that crossed a trust
    /// boundary (an attacker-controlled base64 token).
    #[test]
    fn message_cursor_decode_normalizes_the_inbox_id() {
        let token = Cursor::new()
            .with("message_id", "<a@b.c>")
            .with("inbox_id", "MiXeD-Case@Example.Test")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        let decoded = MessageCursor::decode(&token, None).unwrap();
        assert_eq!(
            decoded.inbox_id.as_str(),
            "mixed-case@example.test",
            "decode must normalize inbox_id itself, not rely on every future caller remembering \
             eq_normalized"
        );
    }

    #[test]
    fn thread_cursor_decode_normalizes_the_inbox_id() {
        let token = Cursor::new()
            .with("thread_id", ThreadId::new_random().to_string())
            .with("inbox_id", "MiXeD-Case@Example.Test")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        let decoded = ThreadCursor::decode(&token, None).unwrap();
        assert_eq!(decoded.inbox_id.as_str(), "mixed-case@example.test");
    }

    /// A malformed (non-UUID) `thread_id` field must be rejected outright, never silently coerced
    /// to the nil UUID or any other default — a token is attacker-controlled input.
    #[test]
    fn thread_cursor_decode_rejects_a_non_uuid_thread_id() {
        let token = Cursor::new()
            .with("thread_id", "not-a-uuid")
            .with("inbox_id", "a@b.c")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(ThreadCursor::decode(&token, None), Err(PageTokenError::WrongType("thread_id")));
    }

    /// Sibling of `message_cursor_rejects_a_nul_byte_in_inbox_id` for [`ThreadCursor`]. Unlike
    /// `MessageCursor`, `ThreadCursor` has only one free-text field (`inbox_id`) — `thread_id` is
    /// a UUID and is covered instead by
    /// [`thread_cursor_decode_rejects_a_non_uuid_thread_id`]'s `WrongType`.
    #[test]
    fn thread_cursor_rejects_a_nul_byte_in_inbox_id() {
        let token = Cursor::new()
            .with("thread_id", ThreadId::new_random().to_string())
            .with("inbox_id", "abc\0def")
            .with("timestamp", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(
            ThreadCursor::decode(&token, None),
            Err(PageTokenError::ForbiddenByte("inbox_id"))
        );
    }

    #[test]
    fn thread_cursor_round_trips_and_checks_scope() {
        let thread_id = ThreadId::new_random();
        let c = ThreadCursor {
            thread_id,
            inbox_id: InboxId::new("amk-probe@agentmail.to"),
            timestamp: ts(),
        };
        let token = c.encode();
        assert_eq!(ThreadCursor::decode(&token, None).unwrap(), c);

        let other = InboxId::new("someone-else@agentmail.to");
        assert_eq!(ThreadCursor::decode(&token, Some(&other)), Err(PageTokenError::WrongScope));
    }

    // ---- PodCursor -----------------------------------------------------------------------

    #[test]
    fn pod_cursor_round_trips() {
        let c = PodCursor { created_at: ts(), pod_id: PodId::new_random() };
        let token = c.encode();
        assert_eq!(PodCursor::decode(&token).unwrap(), c);
    }

    #[test]
    fn pod_cursor_rejects_a_non_uuid_pod_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("pod_id", "not-a-uuid")
            .encode();
        assert_eq!(PodCursor::decode(&token), Err(PageTokenError::WrongType("pod_id")));
    }

    /// A NUL byte in `pod_id` has no dedicated `ForbiddenByte` check — see the struct doc — so
    /// this pins that it is rejected as `WrongType` (via `Uuid`'s own parse) instead, not silently
    /// accepted or reaching a query.
    #[test]
    fn pod_cursor_rejects_a_nul_byte_in_pod_id_as_wrong_type_not_forbidden_byte() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("pod_id", "abc\0def")
            .encode();
        assert_eq!(PodCursor::decode(&token), Err(PageTokenError::WrongType("pod_id")));
    }

    #[test]
    fn pod_cursor_rejects_missing_fields() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .encode();
        assert_eq!(PodCursor::decode(&token), Err(PageTokenError::MissingField("pod_id")));
    }

    // ---- InboxCursor ----------------------------------------------------------------------

    #[test]
    fn inbox_cursor_round_trips_and_accepts_a_matching_pinned_pod() {
        let pod_id = PodId::new_random();
        let c = InboxCursor {
            created_at: ts(),
            inbox_id: InboxId::new("amk-probe@agentmail.to"),
            pod_id,
        };
        let token = c.encode();
        assert_eq!(InboxCursor::decode(&token, None).unwrap(), c);
        assert_eq!(InboxCursor::decode(&token, Some(pod_id)).unwrap(), c);
    }

    #[test]
    fn inbox_cursor_rejects_a_foreign_pod_scope() {
        let c = InboxCursor {
            created_at: ts(),
            inbox_id: InboxId::new("mine@agentmail.to"),
            pod_id: PodId::new_random(),
        };
        let token = c.encode();
        let other_pod = PodId::new_random();
        assert_eq!(InboxCursor::decode(&token, Some(other_pod)), Err(PageTokenError::WrongScope));
    }

    #[test]
    fn inbox_cursor_rejects_a_nul_byte_in_inbox_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("inbox_id", "abc\0def")
            .with("pod_id", PodId::new_random().to_string())
            .encode();
        assert_eq!(
            InboxCursor::decode(&token, None),
            Err(PageTokenError::ForbiddenByte("inbox_id"))
        );
    }

    #[test]
    fn inbox_cursor_decode_normalizes_the_inbox_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("inbox_id", "MiXeD-Case@Example.Test")
            .with("pod_id", PodId::new_random().to_string())
            .encode();
        let decoded = InboxCursor::decode(&token, None).unwrap();
        assert_eq!(decoded.inbox_id.as_str(), "mixed-case@example.test");
    }

    #[test]
    fn inbox_cursor_rejects_a_non_uuid_pod_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("inbox_id", "a@b.c")
            .with("pod_id", "not-a-uuid")
            .encode();
        assert_eq!(InboxCursor::decode(&token, None), Err(PageTokenError::WrongType("pod_id")));
    }

    // ---- ApiKeyCursor ---------------------------------------------------------------------

    fn valid_api_key_id() -> ApiKeyId {
        ApiKeyId::new(uuid::Uuid::new_v4().to_string())
    }

    #[test]
    fn api_key_cursor_round_trips_at_the_organization_mount() {
        let c = ApiKeyCursor {
            created_at: ts(),
            api_key_id: valid_api_key_id(),
            pod_id: None,
            inbox_id: None,
        };
        let token = c.encode();
        assert_eq!(ApiKeyCursor::decode(&token, &KeyScope::Organization).unwrap(), c);
    }

    #[test]
    fn api_key_cursor_round_trips_at_the_pod_mount() {
        let pod_id = PodId::new_random();
        let c = ApiKeyCursor {
            created_at: ts(),
            api_key_id: valid_api_key_id(),
            pod_id: Some(pod_id),
            inbox_id: None,
        };
        let token = c.encode();
        assert_eq!(ApiKeyCursor::decode(&token, &KeyScope::Pod(pod_id)).unwrap(), c);
    }

    #[test]
    fn api_key_cursor_round_trips_at_the_inbox_mount_and_normalizes_it() {
        let inbox_id = InboxId::new("Mixed-Case@Example.Test");
        let c = ApiKeyCursor {
            created_at: ts(),
            api_key_id: valid_api_key_id(),
            pod_id: None,
            inbox_id: Some(inbox_id.clone()),
        };
        let token = c.encode();
        let decoded = ApiKeyCursor::decode(&token, &KeyScope::Inbox(inbox_id)).unwrap();
        assert_eq!(decoded.inbox_id.as_ref().unwrap().as_str(), "mixed-case@example.test");
    }

    /// The mount is not the key's own scope (see `check_key_scope`'s doc): a token minted at the
    /// organization mount — `(None, None)` — must not resolve against a pod-pinned request, even
    /// though `Organization` also *lists* pod-scoped keys.
    #[test]
    fn api_key_cursor_rejects_a_mismatched_mount() {
        let c = ApiKeyCursor {
            created_at: ts(),
            api_key_id: valid_api_key_id(),
            pod_id: None,
            inbox_id: None,
        };
        let token = c.encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Pod(PodId::new_random())),
            Err(PageTokenError::WrongScope)
        );
    }

    #[test]
    fn api_key_cursor_rejects_a_pod_token_replayed_against_a_different_pod() {
        let pod_a = PodId::new_random();
        let pod_b = PodId::new_random();
        let c = ApiKeyCursor {
            created_at: ts(),
            api_key_id: valid_api_key_id(),
            pod_id: Some(pod_a),
            inbox_id: None,
        };
        let token = c.encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Pod(pod_b)),
            Err(PageTokenError::WrongScope)
        );
    }

    #[test]
    fn api_key_cursor_rejects_a_nul_byte_in_inbox_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("api_key_id", valid_api_key_id().as_str())
            .with("inbox_id", "abc\0def")
            .encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Organization),
            Err(PageTokenError::ForbiddenByte("inbox_id"))
        );
    }

    /// `api_key_id` binds into a `uuid` column (see the struct's own doc), so a non-UUID or
    /// non-canonical rendering — including a NUL byte, which cannot appear in valid UUID text at
    /// all — must be `WrongType`, never silently accepted only to bind `NULL` at query time.
    #[test]
    fn api_key_cursor_rejects_a_non_uuid_api_key_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("api_key_id", "not-a-uuid")
            .encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Organization),
            Err(PageTokenError::WrongType("api_key_id"))
        );
    }

    #[test]
    fn api_key_cursor_rejects_a_non_canonical_rendering_of_api_key_id() {
        let id = uuid::Uuid::new_v4();
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            // Uppercase is a valid UUID parse but not the canonical rendering this crate ever
            // issues — see `exact_api_key_uuid`'s own doc for why only the canonical form resolves.
            .with("api_key_id", id.to_string().to_uppercase())
            .encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Organization),
            Err(PageTokenError::WrongType("api_key_id"))
        );
    }

    #[test]
    fn api_key_cursor_rejects_a_non_uuid_pod_id() {
        let token = Cursor::new()
            .with("created_at", "2026-08-15T05:44:16.768Z")
            .with("api_key_id", valid_api_key_id().as_str())
            .with("pod_id", "not-a-uuid")
            .encode();
        assert_eq!(
            ApiKeyCursor::decode(&token, &KeyScope::Organization),
            Err(PageTokenError::WrongType("pod_id"))
        );
    }
}
