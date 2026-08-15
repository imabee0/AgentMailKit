//! Pagination envelope and cursor.
//!
//! P-1 item 4 (`reference/fixtures/04-pagination.http`) observed:
//! * envelope `{count, limit?, next_page_token?, <resource>: [...]}` — the array key is named
//!   after the resource, which is why each list response is its own struct;
//! * `next_page_token` is **absent** (not `null`, not `""`) on the last page;
//! * the token is `base64(JSON)` of a **keyset cursor** — for messages,
//!   `{"message_id":…,"inbox_id":…,"timestamp":…}`, i.e. the last item's sort key.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// An opaque-to-clients page token that we encode as base64(JSON keyset), matching the
/// upstream scheme. Clients must not parse it; we do, to resume a scan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor(pub Map<String, Value>);

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("page token is not valid base64")]
    Base64,
    #[error("page token is not valid JSON")]
    Json,
    #[error("page token is not a JSON object")]
    NotAnObject,
}

impl Cursor {
    pub fn new() -> Self {
        Self(Map::new())
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0.insert(key.to_owned(), value.into());
        self
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    pub fn encode(&self) -> String {
        STANDARD.encode(serde_json::to_vec(&self.0).expect("Map<String, Value> always serializes"))
    }

    pub fn decode(token: &str) -> Result<Self, CursorError> {
        let bytes = STANDARD.decode(token).map_err(|_| CursorError::Base64)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| CursorError::Json)?;
        match value {
            Value::Object(map) => Ok(Self(map)),
            _ => Err(CursorError::NotAnObject),
        }
    }
}

/// Declares a list response: `{count, limit?, next_page_token?, <field>: Vec<T>}`.
///
/// `limit` and `next_page_token` are skipped when absent so the last page omits the token
/// entirely, as observed.
#[macro_export]
macro_rules! list_response {
    ($(#[$m:meta])* $name:ident, $field:ident, $item:ty) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct $name {
            pub count: u64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub limit: Option<u64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub next_page_token: Option<String>,
            pub $field: Vec<$item>,
        }

        impl $name {
            pub fn new(items: Vec<$item>, limit: Option<u64>, next: Option<String>) -> Self {
                Self { count: items.len() as u64, limit, next_page_token: next, $field: items }
            }
        }
    };
}

/// Common list query parameters shared by the collection endpoints.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ascending: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<crate::Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<crate::Timestamp>,
    /// Restricted-label visibility flags. All default false: restricted mail is hidden
    /// from list results unless explicitly requested (and the credential holds the
    /// matching label-read permission).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_spam: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_blocked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_unauthenticated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_trash: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim token from reference/fixtures/04-pagination.http.
    const LIVE_TOKEN: &str = "eyJtZXNzYWdlX2lkIjoiPDAxMDAwMWEwMDNmMzEyYWMtM2M4MDM0OWUtMGE5Ny00OTE1LTlkZWEtMGQyNzY0ZDc3MjlhLTAwMDAwMEBlbWFpbC5hbWF6b25zZXMuY29tPiIsImluYm94X2lkIjoiYW1rLXByb2JlQGFnZW50bWFpbC50byIsInRpbWVzdGFtcCI6IjIwMjYtMDgtMTVUMDU6NDQ6MTYuNzY4WiJ9";

    #[test]
    fn decodes_the_live_keyset_cursor() {
        let c = Cursor::decode(LIVE_TOKEN).unwrap();
        assert_eq!(c.get_str("inbox_id"), Some("amk-probe@agentmail.to"));
        assert_eq!(c.get_str("timestamp"), Some("2026-08-15T05:44:16.768Z"));
        assert!(c.get_str("message_id").unwrap().starts_with('<'));
    }

    #[test]
    fn cursor_round_trips() {
        let c = Cursor::new()
            .with("inbox_id", "a@b.c")
            .with("timestamp", "2026-08-15T05:44:16.768Z");
        assert_eq!(Cursor::decode(&c.encode()).unwrap(), c);
    }

    #[test]
    fn malformed_tokens_are_rejected_not_panicking() {
        assert!(matches!(Cursor::decode("!!!not base64!!!"), Err(CursorError::Base64)));
        assert!(matches!(Cursor::decode(&STANDARD.encode("not json")), Err(CursorError::Json)));
        assert!(matches!(
            Cursor::decode(&STANDARD.encode("[1,2]")),
            Err(CursorError::NotAnObject)
        ));
        // A truncated real token must fail cleanly rather than half-decode.
        assert!(Cursor::decode(&LIVE_TOKEN[..LIVE_TOKEN.len() - 10]).is_err());
    }

    list_response!(TestList, items, String);

    #[test]
    fn last_page_omits_the_token_entirely() {
        let page = TestList::new(vec!["a".into()], Some(1), None);
        let s = serde_json::to_string(&page).unwrap();
        assert_eq!(s, r#"{"count":1,"limit":1,"items":["a"]}"#);
        assert!(!s.contains("next_page_token"));
    }

    #[test]
    fn non_last_page_carries_the_token() {
        let page = TestList::new(vec!["a".into()], Some(1), Some("tok".into()));
        let s = serde_json::to_string(&page).unwrap();
        assert_eq!(s, r#"{"count":1,"limit":1,"next_page_token":"tok","items":["a"]}"#);
    }
}
