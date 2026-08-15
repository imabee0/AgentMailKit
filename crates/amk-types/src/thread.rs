//! Thread resource.
//!
//! Threading rule (P-1 item 16, `reference/fixtures/16-threading-matrix/`): a message joins an
//! existing thread **only** via the RFC Message-ID reference chain (`In-Reply-To` / `References`),
//! scoped per inbox. Subject is **not** a grouping key — identical subjects, `Re:`/`Fwd:`/`AW:`/
//! `[list]` prefixes, trailing whitespace, duplicates and empty subjects each opened their own
//! thread (18 messages → 17 threads). Threads never span inboxes.

use crate::{ids::*, list_response, message::Attachment, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_id: Option<PodId>,
    pub inbox_id: InboxId,
    pub thread_id: ThreadId,
    pub labels: Vec<String>,
    pub timestamp: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_timestamp: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_timestamp: Option<Timestamp>,
    pub senders: Vec<String>,
    pub recipients: Vec<String>,
    /// Absent (not `""`) when the mail carried an empty subject — observed in threading case f.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    pub last_message_id: MessageId,
    pub message_count: u64,
    pub size: u64,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

/// A thread with its messages, ascending by timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    #[serde(flatten)]
    pub item: ThreadItem,
    pub messages: Vec<crate::message::Message>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateThreadRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateThreadResponse {
    pub thread_id: ThreadId,
    pub labels: Vec<String>,
}

/// Search hit highlights.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchHighlights {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
}

list_response!(ListThreadsResponse, threads, ThreadItem);

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ThreadItem {
        ThreadItem {
            organization_id: None,
            pod_id: None,
            inbox_id: InboxId::new("amk-probe@agentmail.to"),
            thread_id: ThreadId::new_random(),
            labels: vec!["received".into()],
            timestamp: Timestamp::now(),
            received_timestamp: None,
            sent_timestamp: None,
            senders: vec!["a@b.c".into()],
            recipients: vec!["amk-probe@agentmail.to".into()],
            subject: None,
            preview: None,
            attachments: None,
            last_message_id: MessageId::new("<x@y.z>"),
            message_count: 1,
            size: 10,
            updated_at: Timestamp::now(),
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn empty_subject_is_absent_not_empty_string() {
        // Threading case (f): mail with an empty Subject stores no `subject` field at all.
        let s = serde_json::to_string(&sample()).unwrap();
        assert!(!s.contains("subject"), "empty subject must be omitted: {s}");
    }

    #[test]
    fn thread_round_trips() {
        let t = sample();
        let back: ThreadItem = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn update_thread_omits_empty_label_arrays() {
        let s = serde_json::to_string(&UpdateThreadRequest::default()).unwrap();
        assert_eq!(s, "{}");
    }
}
