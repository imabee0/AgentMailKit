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
    let app_config = config::app_config();

    let result = match role {
        AmkdRole::Api => server::serve_api(&url, &bind, app_config).await,
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
