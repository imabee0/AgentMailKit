//! `amk` — the control-plane CLI: `init`, `migrate`, `doctor`. See
//! `.claude/contracts/amk-bins.md`. Deliberately thin: every real decision lives in
//! `amk_cli`'s library half, unit-tested there; this file only translates real argv/env into
//! calls on it and turns the result into an exit code.

use amk_cli::args::{self, AmkCommand};
use amk_cli::{commands, config, exit};

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let command = match args::parse_amk(&argv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(exit::USAGE);
        }
    };

    match command {
        AmkCommand::Help => println!("{}", args::AMK_HELP),
        AmkCommand::Init => run_init().await,
        AmkCommand::Migrate => run_migrate().await,
        AmkCommand::Doctor => run_doctor().await,
    }
}

fn database_url_or_exit() -> String {
    match config::database_url() {
        Ok(url) => url,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(exit::FAILURE);
        }
    }
}

async fn run_init() {
    let url = database_url_or_exit();
    let product_name = std::env::var(config::AMK_PRODUCT_NAME).ok();
    match commands::init::run(&url, product_name).await {
        Ok(outcome) => {
            // Printed exactly once, to stdout, and nowhere else -- never through `tracing`,
            // `eprintln!`, a file, or `{:?}` (which `CreateApiKeyResponse`'s hand-written `Debug`
            // redacts specifically so a stray one of those can't leak it; this prints the field
            // itself, explicitly, on purpose, the one time it is allowed to).
            println!("Initialized a new deployment.");
            println!("  organization_id: {}", outcome.organization_id);
            println!("  pod_id:          {}", outcome.pod_id);
            println!("  root api key:    {}", outcome.root_key.api_key);
            println!("This key will not be shown again -- store it now.");
        }
        Err(e) => {
            eprintln!("amk init: {e}");
            std::process::exit(exit::FAILURE);
        }
    }
}

async fn run_migrate() {
    let url = database_url_or_exit();
    match commands::migrate::run(&url).await {
        Ok(status) => println!("{}", commands::migrate::describe(status)),
        Err(e) => {
            eprintln!("amk migrate: {e}");
            std::process::exit(exit::FAILURE);
        }
    }
}

async fn run_doctor() {
    let inputs = commands::doctor::DoctorInputs {
        vars: config::var_presence(),
        database_url: std::env::var(config::AMK_DATABASE_URL).ok(),
    };
    let report = commands::doctor::run(inputs).await;
    println!("{}", report.to_text());
}
