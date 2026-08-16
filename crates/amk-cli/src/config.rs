//! Deployment configuration: environment only, and it fails closed. See the dispatch contract's
//! own table — this module is a direct transcription of it, nothing more.
//!
//! `AMK_DATABASE_URL` is the one required variable; the rest default. A missing
//! `AMK_DATABASE_URL` is [`MissingDatabaseUrl`] — a clear error naming the variable — never a
//! panic and never a default (a default here would silently point production at a dev database).

use std::env;
use std::fmt;

use amk_http::AppConfig;

pub const AMK_DATABASE_URL: &str = "AMK_DATABASE_URL";
pub const AMK_BIND: &str = "AMK_BIND";
pub const AMK_PRIMARY_DOMAIN: &str = "AMK_PRIMARY_DOMAIN";
pub const AMK_PRODUCT_NAME: &str = "AMK_PRODUCT_NAME";

pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// `AMK_DATABASE_URL` was not set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDatabaseUrl;

impl fmt::Display for MissingDatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{AMK_DATABASE_URL} is not set. This must be the Postgres connection string to use \
             for this deployment; there is no default, so a missing value never silently falls \
             back to a development database."
        )
    }
}

impl std::error::Error for MissingDatabaseUrl {}

/// `AMK_DATABASE_URL`, required, no default.
pub fn database_url() -> Result<String, MissingDatabaseUrl> {
    env::var(AMK_DATABASE_URL).map_err(|_| MissingDatabaseUrl)
}

/// `AMK_BIND`, defaulting to [`DEFAULT_BIND`].
pub fn bind_address() -> String {
    env::var(AMK_BIND).unwrap_or_else(|_| DEFAULT_BIND.to_owned())
}

/// `amk_http::AppConfig` built from `AMK_PRIMARY_DOMAIN`/`AMK_PRODUCT_NAME` — both absent by
/// default, per `amk_http::config`'s own fail-closed rule. This crate only reads the environment
/// and passes the values through; it never invents a default domain or product name.
pub fn app_config() -> AppConfig {
    AppConfig {
        primary_domain: env::var(AMK_PRIMARY_DOMAIN).ok(),
        product_name: env::var(AMK_PRODUCT_NAME).ok(),
    }
}

/// Whether each configuration variable is present, for `amk doctor` — never the value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarPresence {
    pub name: &'static str,
    pub set: bool,
}

/// One line per documented variable, in the table's own order.
pub fn var_presence() -> Vec<VarPresence> {
    [
        AMK_DATABASE_URL,
        AMK_BIND,
        AMK_PRIMARY_DOMAIN,
        AMK_PRODUCT_NAME,
    ]
    .into_iter()
    .map(|name| VarPresence { name, set: env::var(name).is_ok() })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ONLY test in this crate that mutates process environment directly — every other test
    /// either drives a compiled binary as a subprocess (its own independent environment, so no
    /// race is possible) or takes configuration as an explicit function argument. Keeping this
    /// the sole exception is what makes it safe under `cargo test`'s default thread-parallel
    /// execution: two tests mutating the same process-global variable concurrently would race.
    #[test]
    fn database_url_reads_the_documented_variable_name() {
        const SENTINEL: &str = "amk-cli-test-database-url-sentinel";
        // The literal string, NOT the `AMK_DATABASE_URL` constant: this test exists to catch a
        // typo/rename of the constant's own VALUE, and reading it back through that same constant
        // would make the two drift together, so the test could never fail no matter what the
        // constant's content was renamed to (`tests/process.rs`'s black-box tests are what were
        // actually catching that class of mutation before this fix).
        // SAFETY: no other test in this binary touches AMK_DATABASE_URL — see the doc above.
        unsafe { env::set_var("AMK_DATABASE_URL", SENTINEL) };
        assert_eq!(database_url().as_deref(), Ok(SENTINEL));
        unsafe { env::remove_var("AMK_DATABASE_URL") };
        assert!(database_url().is_err(), "removing the variable must be observed as unset");
    }

    #[test]
    fn default_bind_is_a_loopback_address_with_a_port() {
        assert!(DEFAULT_BIND.starts_with("127.0.0.1:"));
    }
}
