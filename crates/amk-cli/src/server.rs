//! `amkd`'s role dispatch. `--role api` serves `amk_http::router` over plain HTTP on `AMK_BIND`;
//! `--role smtpd` serves inbound SMTP on the same bind variable. `worker` and `all` are rejected,
//! naming what will implement them. A genuinely unrecognised `--role` is `crate::args`'s job.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use amk_http::{config::DEFAULT_MAX_BODY_BYTES, AppConfig, AppState};
use amk_ingest::lookup::StoreInboxLookup;
use amk_ingest::{serve_session, Authenticator, IngestConfig, StorePersist};
use amk_outbound::Keyring;

use crate::args::AmkdRole;
use crate::config::AMK_PRIMARY_DOMAIN;
use crate::config::{smtp_max_connections, smtp_session_timeout};
use crate::redact::describe_connect_failure;

/// `None` for a role this dispatch implements; `Some(message)` naming what will, for every other
/// role the parser recognises. A role that parses and does nothing would be a server that looks
/// like it is running and is not — so `main` must check this before doing anything else.
pub fn not_yet_implemented(role: AmkdRole) -> Option<&'static str> {
    match role {
        AmkdRole::Api | AmkdRole::Smtpd => None,
        AmkdRole::Worker => Some(
            "amkd --role worker is not implemented yet -- background job processing is amk-jobs.",
        ),
        AmkdRole::All => {
            Some("amkd --role all is not implemented yet -- it requires every role above.")
        }
    }
}

/// Connect, build the router, bind `bind`, and serve forever (or until the listener errors).
///
/// `keyring` is the operator's DKIM material, from `AMK_DKIM_KEYS`. It is a parameter rather than
/// something this function loads, so that a failure to load it is reported by `main` BEFORE a
/// listener exists -- a server that has bound its port is a server something is already talking
/// to, and discovering there that it cannot sign is too late.
pub async fn serve_api(
    database_url: &str,
    bind: &str,
    config: AppConfig,
    keyring: Keyring,
) -> Result<(), String> {
    let pool = amk_store::connect(database_url).await.map_err(|e| {
        format!("could not connect using AMK_DATABASE_URL: {}", describe_connect_failure(&e))
    })?;
    let app = amk_http::router(AppState::new(pool, config, keyring));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("could not bind AMK_BIND {bind:?}: {e}"))?;
    tracing::info!(role = "api", %bind, "serving");
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"))
}

/// Connect, require `AMK_PRIMARY_DOMAIN`, bind `bind` as SMTP, accept loop → `serve_session`.
pub async fn serve_smtpd(database_url: &str, bind: &str, config: AppConfig) -> Result<(), String> {
    let primary_domain = config
        .primary_domain
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "{AMK_PRIMARY_DOMAIN} is not set. smtpd refuses to start without a local domain; \
                 there is no default."
            )
        })?;

    let pool = amk_store::connect(database_url).await.map_err(|e| {
        format!("could not connect using AMK_DATABASE_URL: {}", describe_connect_failure(&e))
    })?;
    let auth = Authenticator::live().map_err(|e| format!("could not start authenticator: {e}"))?;
    let ingest = IngestConfig::new(
        primary_domain.clone(),
        &[primary_domain.as_str()],
        DEFAULT_MAX_BODY_BYTES,
        Duration::from_millis(250),
    );
    let lookup = StoreInboxLookup { pool: pool.clone() };
    let persist = StorePersist { pool, auth };

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("could not bind AMK_BIND {bind:?}: {e}"))?;
    tracing::info!(role = "smtpd", %bind, %primary_domain, "serving");

    // TWO BOUNDS, both of which were missing, and together they are what closes a slow-loris.
    //
    // The accept loop used to `tokio::spawn` unconditionally, so concurrent sessions were limited
    // only by file descriptors -- and `serve_session` has no deadline of its own: after the 250ms
    // greet-pause, `read_line` awaits the next byte forever. A few hundred sockets each trickling
    // one byte a minute cost an attacker nothing and pinned a task apiece.
    //
    // The SEMAPHORE bounds how many sessions exist at once. The DEADLINE bounds how long any one
    // of them can hold its permit -- without it the semaphore just changes the resource being
    // exhausted from tasks to permits, and the server stops answering anyone.
    let permits = smtp_max_connections();
    let session_timeout = smtp_session_timeout();
    let limiter = Arc::new(Semaphore::new(permits));
    tracing::info!(
        max_connections = permits,
        session_timeout_s = session_timeout.as_secs(),
        "smtpd limits"
    );

    loop {
        let (mut stream, peer) = listener
            .accept()
            .await
            .map_err(|e| format!("accept error: {e}"))?;

        // `try_acquire_owned`, not `acquire_owned`: at capacity we must ANSWER, not queue. Queuing
        // accepted-but-unserved sockets is the same unbounded growth one layer down, and a sender
        // that gets no banner cannot tell an overloaded server from a broken one. 421 is the
        // RFC 5321 §3.8 code for "service not available, closing channel" and every real MTA
        // retries on it -- so mail is deferred, never lost.
        let permit = match Arc::clone(&limiter).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(%peer, max_connections = permits, "smtpd at capacity, deferring");
                let _ = stream
                    .write_all(b"421 4.7.0 Too many concurrent connections, try again later\r\n")
                    .await;
                let _ = stream.shutdown().await;
                continue;
            }
        };

        let ingest = ingest.clone();
        let lookup = lookup.clone();
        let persist = persist.clone();
        tokio::spawn(async move {
            // Held for exactly the session's lifetime, released on every path including timeout.
            let _permit = permit;
            match tokio::time::timeout(
                session_timeout,
                serve_session(stream, peer, &ingest, &lookup, &persist),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::debug!(%peer, error = %e, "smtp session ended with an error")
                }
                Err(_) => tracing::warn!(
                    %peer,
                    timeout_s = session_timeout.as_secs(),
                    "smtp session exceeded its deadline; closing"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_and_smtpd_are_the_roles_this_dispatch_implements() {
        assert_eq!(not_yet_implemented(AmkdRole::Api), None);
        assert_eq!(not_yet_implemented(AmkdRole::Smtpd), None);
    }

    #[test]
    fn worker_and_all_are_rejected_and_name_themselves() {
        let worker = not_yet_implemented(AmkdRole::Worker).expect("worker must be rejected");
        assert!(worker.contains("worker"));
        assert!(worker.contains("amk-jobs"));

        let all = not_yet_implemented(AmkdRole::All).expect("all must be rejected");
        assert!(all.contains("all"));
    }
}
