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
//!
//! `max_body_bytes` is a THIRD piece of configuration this crate needs but must not guess past a
//! safe default: see its own doc.

#[derive(Clone)]
pub struct AppConfig {
    /// The domain a `POST /v0/inboxes` with no `domain` field is created under. `None` means "not
    /// configured" — creation without an explicit `domain` then fails closed.
    pub primary_domain: Option<String>,
    /// The `display_name` a `POST /v0/inboxes` with no `display_name` field gets. `None` means
    /// "not configured" — same fail-closed rule as `primary_domain`.
    pub product_name: Option<String>,
    /// The maximum request body size `crate::body::JsonBody` will buffer before rejecting, via
    /// `axum::extract::DefaultBodyLimit` installed on the router (`crate::router`). See
    /// [`DEFAULT_MAX_BODY_BYTES`] for why 8 MiB and why it is `[INFERRED]`.
    pub max_body_bytes: usize,
    /// When set, outbound SMTP is relayed through this hop. When `None`, deliver direct-to-MX.
    /// No environment variable: tests inject a [`amk_outbound::RecordingTransport`]; production
    /// leaves this `None` (and the keyring empty) so a send fails closed until keys are injected.
    pub smtp_smarthost: Option<(String, u16)>,
    /// HMAC secret behind signed download URLs. `None` when blobs are not configured; `amkd`
    /// refuses to start with a blob root and no key, so the two are always set together.
    ///
    /// NOT in `Debug`: see the hand-written impl below.
    pub master_key: Option<Vec<u8>>,
    /// How this deployment is reached from outside, e.g. `https://mail.example.com`. Only used to
    /// build absolute `download_url`s -- the reference returns an absolute CDN URL and a relative
    /// one would be a shape divergence, not just a cosmetic one.
    pub public_base_url: String,
}
impl std::fmt::Debug for AppConfig {
    /// Hand-written the moment this struct started holding a secret.
    ///
    /// A derived `Debug` prints `master_key` into any log line, panic message or error chain that
    /// formats an `AppConfig` -- and `AppState` is formatted in exactly those places. The same
    /// reasoning as `amk-outbound::signing::SigningKey` and `amk-ingest::IngestConfig`: the leak
    /// is invisible in the diff that introduces it, because nothing in it looks like a leak.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("primary_domain", &self.primary_domain)
            .field("product_name", &self.product_name)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("smtp_smarthost", &self.smtp_smarthost)
            .field("public_base_url", &self.public_base_url)
            .field(
                "master_key",
                &self
                    .master_key
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("none"),
            )
            .finish()
    }
}

/// `[INFERRED]` — `reference/fixtures/27-malformed-request-handling.txt` §5 observed that the
/// reference accepts and parses a 3 MB body (answering the ordinary JSON-syntax error, not a
/// size-specific one) against axum's own unconditional 2 MB default, but its own true ceiling was
/// deliberately not probed: finding it means firing progressively larger payloads at someone
/// else's production API, which is not a reasonable thing to do for a number that can instead be
/// chosen safely. 8 MiB is chosen because it comfortably clears the one size this project has
/// actually measured — the ~5.95 MB inline attachment threshold `[SPEC:repo agentmail-toolkit]`,
/// which P2 request bodies must clear — while still bounding the buffer: unbounded body buffering
/// is a denial-of-service primitive on a public endpoint, and matching the reference exactly is
/// not worth that.
pub const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            primary_domain: None,
            product_name: None,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            smtp_smarthost: None,
            master_key: None,
            public_base_url: "http://127.0.0.1:8080".to_owned(),
        }
    }
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_master_key() {
        // The whole reason the derive was removed. This fails the moment somebody adds it back.
        let c = AppConfig {
            master_key: Some(b"super-secret-master-key-material-01234567".to_vec()),
            ..AppConfig::default()
        };
        let rendered = format!("{c:?}");
        assert!(!rendered.contains("super-secret"), "the key leaked into Debug: {rendered}");
        assert!(!rendered.contains("115"), "the key leaked as bytes: {rendered}");
        assert!(rendered.contains("<redacted>"));
        // Absent is distinguishable from present-but-hidden, which is what an operator needs.
        assert!(format!("{:?}", AppConfig::default()).contains("none"));
    }
}
