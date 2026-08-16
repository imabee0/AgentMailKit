//! `amkd`'s role dispatch. `--role api` serves `amk_http::router` over plain HTTP on `AMK_BIND`;
//! every other role the parser recognises is rejected, naming what will implement it — never
//! silently accepted, never treated as unknown (that distinction belongs to `crate::args`, which
//! rejects a genuinely unrecognised `--role` value before this module ever runs). TLS is P6
//! (cert-manager terminating via rustls), not here.

use amk_http::{AppConfig, AppState};

use crate::args::AmkdRole;
use crate::redact::describe_connect_failure;

/// `None` for the one role this dispatch implements; `Some(message)` naming what will, for every
/// other role the parser recognises. A role that parses and does nothing would be a server that
/// looks like it is running and is not — so `main` must check this before doing anything else.
pub fn not_yet_implemented(role: AmkdRole) -> Option<&'static str> {
    match role {
        AmkdRole::Api => None,
        AmkdRole::Smtpd => Some(
            "amkd --role smtpd is not implemented yet -- mail ingest/outbound is amk-ingest and \
             amk-outbound (plan phase P2).",
        ),
        AmkdRole::Worker => Some(
            "amkd --role worker is not implemented yet -- background job processing is amk-jobs.",
        ),
        AmkdRole::All => {
            Some("amkd --role all is not implemented yet -- it requires every role above.")
        }
    }
}

/// Connect, build the router, bind `bind`, and serve forever (or until the listener errors).
pub async fn serve_api(database_url: &str, bind: &str, config: AppConfig) -> Result<(), String> {
    let pool = amk_store::connect(database_url).await.map_err(|e| {
        format!("could not connect using AMK_DATABASE_URL: {}", describe_connect_failure(&e))
    })?;
    let app = amk_http::router(AppState::new(pool, config));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("could not bind AMK_BIND {bind:?}: {e}"))?;
    println!("amkd: serving --role api on {bind}");
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_is_the_one_role_this_dispatch_implements() {
        assert_eq!(not_yet_implemented(AmkdRole::Api), None);
    }

    #[test]
    fn every_other_role_is_rejected_and_names_itself() {
        let smtpd = not_yet_implemented(AmkdRole::Smtpd).expect("smtpd must be rejected");
        assert!(smtpd.contains("smtpd"));
        assert!(smtpd.contains("amk-ingest") || smtpd.contains("amk-outbound"));

        let worker = not_yet_implemented(AmkdRole::Worker).expect("worker must be rejected");
        assert!(worker.contains("worker"));
        assert!(worker.contains("amk-jobs"));

        let all = not_yet_implemented(AmkdRole::All).expect("all must be rejected");
        assert!(all.contains("all"));
    }
}
