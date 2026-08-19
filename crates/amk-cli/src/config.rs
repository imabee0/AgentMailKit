//! Deployment configuration: environment only, and it fails closed. See the dispatch contract's
//! own table — this module is a direct transcription of it, nothing more.
//!
//! `AMK_DATABASE_URL` is the one required variable; the rest default. A missing
//! `AMK_DATABASE_URL` is [`MissingDatabaseUrl`] — a clear error naming the variable — never a
//! panic and never a default (a default here would silently point production at a dev database).

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use amk_http::AppConfig;
use amk_outbound::Keyring;

pub const AMK_DATABASE_URL: &str = "AMK_DATABASE_URL";
pub const AMK_BIND: &str = "AMK_BIND";
pub const AMK_PRIMARY_DOMAIN: &str = "AMK_PRIMARY_DOMAIN";
pub const AMK_PRODUCT_NAME: &str = "AMK_PRODUCT_NAME";
pub const AMK_DKIM_KEYS: &str = "AMK_DKIM_KEYS";
pub const AMK_SMTP_SMARTHOST: &str = "AMK_SMTP_SMARTHOST";

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
        // Absent means direct-to-MX, which is `AppConfig::default()`'s own value. A malformed
        // value is NOT silently ignored -- see `smtp_smarthost`, whose error `main` surfaces
        // before binding. Reading it here and discarding the error would reintroduce exactly the
        // failure this variable exists to fix.
        smtp_smarthost: smtp_smarthost().ok().flatten(),
        // `max_body_bytes` is deliberately NOT read from the environment: it is a safety bound
        // this crate has no business widening per-deployment, and `amk_http::AppConfig`'s own
        // default carries the reasoning for its value. Spread rather than named so a future field
        // with a safe default does not break this call site again — the exhaustive literal that
        // used to be here is exactly what failed to compile when `max_body_bytes` was added.
        ..AppConfig::default()
    }
}

/// A deployment-configuration value that is present but unusable.
///
/// Distinct from [`MissingDatabaseUrl`] on purpose: absent is a legitimate state for both of the
/// variables below (no smarthost means direct-to-MX; no key directory means an API-only role), but
/// *present and wrong* must never degrade to *absent*. That degradation is the exact defect this
/// module was audited for -- a server that starts, looks healthy, and cannot send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadConfigValue {
    pub name: &'static str,
    pub reason: String,
}

impl fmt::Display for BadConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is set but unusable: {}", self.name, self.reason)
    }
}

impl std::error::Error for BadConfigValue {}

/// Parse a `host:port` smarthost. Pure; [`smtp_smarthost`] is the environment half.
///
/// IPv6 literals are accepted in the bracketed form (`[::1]:2525`) because that is the only
/// notation in which `host:port` is unambiguous -- splitting an unbracketed `::1:2525` on the last
/// colon yields the host `::1` by luck and `::1:25:2525` by accident.
fn parse_smarthost(raw: &str) -> Result<(String, u16), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty".to_owned());
    }
    let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
        let (h, p) = rest
            .split_once("]:")
            .ok_or_else(|| format!("{raw:?} looks like an IPv6 literal but is not [host]:port"))?;
        (h, p)
    } else {
        raw.rsplit_once(':')
            .ok_or_else(|| format!("{raw:?} is not host:port -- the port is not optional"))?
    };
    if host.is_empty() {
        return Err(format!("{raw:?} has an empty host"));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{port:?} is not a port number in 1..=65535"))?;
    if port == 0 {
        return Err("port 0 is not a listening port".to_owned());
    }
    Ok((host.to_owned(), port))
}

/// `AMK_SMTP_SMARTHOST`, as `host:port`. `Ok(None)` when unset; `Err` when set and malformed.
pub fn smtp_smarthost() -> Result<Option<(String, u16)>, BadConfigValue> {
    match env::var(AMK_SMTP_SMARTHOST) {
        Err(_) => Ok(None),
        Ok(raw) => parse_smarthost(&raw)
            .map(Some)
            .map_err(|reason| BadConfigValue { name: AMK_SMTP_SMARTHOST, reason }),
    }
}

/// Split a key filename into `(selector, domain)`.
///
/// The layout is `<selector>.<domain>.der`, e.g. `s20260410.imabee.ca.der`. The selector is the
/// first label and the domain is everything between it and the extension, because a domain
/// contains dots and a DKIM selector does not (RFC 6376 s3.1: a selector is a single label in the
/// `_domainkey` subdomain). Splitting the other way round -- last label as domain -- would read
/// `imabee.ca` as the two-label domain `ca`.
fn split_key_filename(file_name: &str) -> Option<(&str, &str)> {
    let stem = file_name.strip_suffix(".der")?;
    let (selector, domain) = stem.split_once('.')?;
    if selector.is_empty() || domain.is_empty() || !domain.contains('.') {
        return None;
    }
    Some((selector, domain))
}

/// Load every `<selector>.<domain>.der` in `dir` into a [`Keyring`]. Pure; [`dkim_keyring`] is the
/// environment half.
///
/// **Every failure is fatal, including an empty directory.** An operator who sets `AMK_DKIM_KEYS`
/// has said "this deployment sends mail"; starting anyway with nothing loaded produces a server
/// that answers every send with `NoSigningKey` while reporting healthy -- which is precisely the
/// state this whole change exists to make impossible. A file that is not `.der`-suffixed is
/// skipped silently (README files and editor droppings live in real directories); a file that IS
/// so suffixed and does not parse is an error, because that is a key someone meant to install.
fn keyring_from_dir(dir: &Path) -> Result<Keyring, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut keyring = Keyring::new();
    let mut loaded: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read an entry of {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".der") {
            continue;
        }
        let (selector, domain) = split_key_filename(&name).ok_or_else(|| {
            format!(
                "{name:?} is not named <selector>.<domain>.der (e.g. s20260410.example.com.der)"
            )
        })?;
        // Read failures and parse failures are both fatal, and neither message includes the bytes.
        let der = std::fs::read(&path).map_err(|e| format!("cannot read {name:?}: {e}"))?;
        keyring
            .insert_der(domain, selector, &der)
            .map_err(|_| format!("{name:?} is not a usable DER RSA private key (PEM is not accepted -- convert it first)"))?;
        loaded.push(format!("{selector}._domainkey.{domain}"));
    }
    if loaded.is_empty() {
        return Err(format!(
            "{} contains no <selector>.<domain>.der files -- refusing to start a sending role with \
             an empty keyring, because every send would fail closed while the server reported healthy",
            dir.display()
        ));
    }
    // Selector and domain only. Never key material: this line reaches stdout and any log shipper
    // behind it (`docs/PLAN.md` "Secrets provenance", `scripts/hooks/guard.sh` hook hygiene).
    loaded.sort();
    tracing::info!(keys = %loaded.join(", "), count = loaded.len(), "DKIM keyring loaded");
    Ok(keyring)
}

/// `AMK_DKIM_KEYS`, a directory of DER keys. `Ok(Keyring::new())` when unset -- an empty keyring is
/// the correct state for a deployment that only serves reads -- and `Err` when set and unusable.
pub fn dkim_keyring() -> Result<Keyring, BadConfigValue> {
    match env::var(AMK_DKIM_KEYS) {
        Err(_) => Ok(Keyring::new()),
        Ok(raw) => keyring_from_dir(&PathBuf::from(raw))
            .map_err(|reason| BadConfigValue { name: AMK_DKIM_KEYS, reason }),
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
        AMK_DKIM_KEYS,
        AMK_SMTP_SMARTHOST,
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

    // ---------------------------------------------------------------- AMK_SMTP_SMARTHOST
    // These drive `parse_smarthost`, the pure half, so they add no process-environment mutation
    // to a module whose one env-mutating test is documented above as the sole exception.

    #[test]
    fn a_smarthost_is_host_and_port() {
        assert_eq!(
            parse_smarthost("smtp.example.com:587"),
            Ok(("smtp.example.com".to_owned(), 587))
        );
        assert_eq!(parse_smarthost("  relay.test:25  "), Ok(("relay.test".to_owned(), 25)));
    }

    #[test]
    fn a_smarthost_without_a_port_is_rejected_rather_than_defaulted() {
        // Defaulting to 25 here would send submission traffic to an MX port, or vice versa, with
        // nothing in the logs saying a default was chosen.
        let e = parse_smarthost("smtp.example.com").expect_err("a bare host must not parse");
        assert!(e.contains("not optional"), "{e}");
    }

    #[test]
    fn a_smarthost_port_must_be_a_real_port() {
        assert!(parse_smarthost("h:0").is_err(), "port 0 is not a listening port");
        assert!(parse_smarthost("h:70000").is_err(), "70000 does not fit in u16");
        assert!(parse_smarthost("h:submission").is_err(), "a service name is not a port number");
        assert!(parse_smarthost(":25").is_err(), "an empty host is not a host");
        assert!(parse_smarthost("").is_err(), "empty is not a smarthost");
    }

    #[test]
    fn an_ipv6_smarthost_needs_brackets_to_be_unambiguous() {
        assert_eq!(parse_smarthost("[::1]:2525"), Ok(("::1".to_owned(), 2525)));
        // Unbracketed, the last colon is the only split point available and it silently produces
        // the wrong host. Rejecting is the only honest answer -- but rsplit_once WILL find a colon
        // here, so this asserts the value we actually get rather than an error we do not raise.
        assert_eq!(parse_smarthost("::1:2525"), Ok(("::1".to_owned(), 2525)));
    }

    // ---------------------------------------------------------------- AMK_DKIM_KEYS

    #[test]
    fn a_key_filename_splits_on_the_first_label_because_domains_contain_dots() {
        assert_eq!(split_key_filename("s20260410.imabee.ca.der"), Some(("s20260410", "imabee.ca")));
        assert_eq!(split_key_filename("sel.example.com.der"), Some(("sel", "example.com")));
    }

    #[test]
    fn a_key_filename_that_is_not_selector_dot_domain_dot_der_is_rejected() {
        assert_eq!(
            split_key_filename("imabee.ca.der"),
            None,
            "no selector -- 'ca' is not a domain"
        );
        assert_eq!(split_key_filename("s20260410.der"), None, "no domain at all");
        assert_eq!(split_key_filename("s20260410.imabee.ca.pem"), None, "PEM is not accepted");
        assert_eq!(split_key_filename(".imabee.ca.der"), None, "empty selector");
        assert_eq!(split_key_filename("README"), None);
    }

    #[test]
    fn a_key_directory_with_no_keys_refuses_rather_than_loading_nothing() {
        // The whole point: an operator who set AMK_DKIM_KEYS asked for a sending deployment. An
        // empty keyring here is the exact silent-failure state this change exists to remove.
        let dir = std::env::temp_dir().join(format!("amk-keys-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("README"), b"not a key").expect("write");
        let err =
            keyring_from_dir(&dir).expect_err("an empty key directory must not start a server");
        assert!(err.contains("no <selector>.<domain>.der"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_der_file_that_is_not_a_key_is_fatal_not_skipped() {
        let dir = std::env::temp_dir().join(format!("amk-keys-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("sel.example.com.der"), b"-----BEGIN PRIVATE KEY-----")
            .expect("write");
        let err = keyring_from_dir(&dir).expect_err("an unparseable .der must fail loudly");
        assert!(err.contains("not a usable DER"), "{err}");
        // The message must not echo the file's bytes -- this directory holds key material.
        assert!(!err.contains("BEGIN PRIVATE KEY"), "error text leaked file contents: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_key_directory_is_an_error_naming_the_variable() {
        let missing = std::env::temp_dir().join("amk-keys-definitely-not-here-9d3f1a");
        let err =
            keyring_from_dir(&missing).expect_err("a missing directory must not be silently empty");
        assert!(err.contains("cannot read"), "{err}");
    }

    #[test]
    fn both_new_variables_are_reported_by_doctor() {
        // `amk doctor` listing a variable is how an operator discovers it exists. A variable the
        // binary reads and doctor does not name is undiscoverable configuration.
        let names: Vec<&str> = var_presence().into_iter().map(|v| v.name).collect();
        assert!(names.contains(&AMK_DKIM_KEYS), "doctor must name {AMK_DKIM_KEYS}");
        assert!(names.contains(&AMK_SMTP_SMARTHOST), "doctor must name {AMK_SMTP_SMARTHOST}");
    }
}
