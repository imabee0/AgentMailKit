//! Deployment configuration this crate needs but must never guess.
//!
//! `POST /v0/inboxes` with an empty body mints a default `username`, `domain` and `display_name`
//! (`reference/fixtures/23-inbox-defaults-and-key-shape.txt`). The generated username's *shape*
//! is reproduced (adjective + noun + 3 digits, lowercase, no separator — `crate::words`); the
//! domain and product name are not: AgentMail's own defaults (`agentmail.to`, `"AgentMail"`) name
//! *their* deployment, not this one, and reproducing them here would be shipping AgentMail's
//! product name in every self-hosted inbox we create.
//!
//! **Both are `[ASSUMED]` configuration, and both fail closed rather than guess** — a deployment
//! with no configured primary domain (or no configured product name) refuses inbox creation with
//! an internal error instead of inventing `agentmail.to`/`"AgentMail"` on its behalf. See
//! `crate::handlers::inboxes::create` for where that refusal happens.

#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    /// The domain a `POST /v0/inboxes` with no `domain` field is created under. `None` means "not
    /// configured" — creation without an explicit `domain` then fails closed.
    pub primary_domain: Option<String>,
    /// The `display_name` a `POST /v0/inboxes` with no `display_name` field gets. `None` means
    /// "not configured" — same fail-closed rule as `primary_domain`.
    pub product_name: Option<String>,
}
