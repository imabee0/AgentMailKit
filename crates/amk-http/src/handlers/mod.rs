//! The P0/P1 handlers, grouped the way the dispatch contract's scope table groups them:
//! identity + organization, pods, inboxes, api-keys.

pub mod api_keys;
pub mod attachments;
pub mod identity;
pub mod inboxes;
pub mod messages;
pub mod pods;
pub mod threads;
