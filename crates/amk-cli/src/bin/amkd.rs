//! `amkd` — the server process, `--role api|smtpd|worker|all`. See
//! `.claude/contracts/amk-bins.md`. Deliberately thin, matching `amk.rs`'s own shape: every real
//! decision lives in `amk_cli`'s library half.

use amk_cli::args::{self, AmkdCommand, AmkdRole};
use amk_cli::{config, exit, server};

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let command = match args::parse_amkd(&argv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(exit::USAGE);
        }
    };

    let role = match command {
        AmkdCommand::Help => {
            println!("{}", args::AMKD_HELP);
            return;
        }
        AmkdCommand::Serve(role) => role,
    };

    // FIRST. Everything below logs, and an event emitted before a subscriber exists is dropped
    // silently -- including the config failures that are the most useful thing a failed start can
    // tell an operator.
    amk_cli::logging::init();
    // DELIBERATE DEVIATION from "replace every println!". The `eprintln!`s below are fatal-exit
    // messages: the last thing an operator sees before the process dies. They stay on stderr,
    // unconditionally, because a `tracing::error!` is FILTERABLE -- `AMK_LOG=off` or a
    // misconfigured filter would swallow the one message explaining why the daemon refused to
    // start. Two of them also run before this line, during argument parsing, where no subscriber
    // exists yet. Lifecycle and request events are structured; the refusal-to-start path is not.

    if let Some(message) = server::not_yet_implemented(role) {
        eprintln!("{message}");
        std::process::exit(exit::FAILURE);
    }

    let url = match config::database_url() {
        Ok(url) => url,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(exit::FAILURE);
        }
    };
    let bind = config::bind_address();

    // Both of these are read BEFORE any listener is bound, and a set-but-unusable value is fatal
    // rather than silently absent. `AMK_SMTP_SMARTHOST` is validated here even though
    // `config::app_config()` reads it again, because `app_config` returns the value and cannot
    // return the error -- and a malformed smarthost that degraded to direct-to-MX would send mail
    // out of the wrong path without saying so.
    if let Err(e) = config::smtp_smarthost() {
        eprintln!("{e}");
        std::process::exit(exit::FAILURE);
    }
    // Before any listener exists. rustls cannot choose a provider on its own in this workspace
    // (both `ring` and `aws-lc-rs` are compiled in via feature unification), and without one it
    // panics inside the request path on the first outbound send rather than at boot.
    // `amk_outbound::smtp` also guards its own delivery path; this is the boot-time half, so the
    // failure -- if the chosen provider ever becomes unavailable -- surfaces here instead.
    amk_outbound::smtp::install_crypto_provider();

    let keyring = match config::dkim_keyring() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(exit::FAILURE);
        }
    };
    let app_config = config::app_config();

    let result = match role {
        AmkdRole::Api => server::serve_api(&url, &bind, app_config, keyring).await,
        AmkdRole::Smtpd => server::serve_smtpd(&url, &bind, app_config).await,
        AmkdRole::Worker | AmkdRole::All => {
            unreachable!("not_yet_implemented already rejected worker/all")
        }
    };
    if let Err(e) = result {
        eprintln!("amkd: {e}");
        std::process::exit(exit::FAILURE);
    }
}
