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

use amk_types::ids::{InboxId, MessageId, ThreadId};
use amk_types::page::Cursor;
use amk_types::Timestamp;
use chrono::{DateTime, SecondsFormat, Utc};

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
}
