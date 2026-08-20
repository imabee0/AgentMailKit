//! Where log events go. The composition root decides; no library does.
//!
//! `docs/PLAN.md`:190 -- "Logs structured to stdout, key-ids never key material". Structured means
//! JSON by default: this is a daemon whose output is scraped, and a log shipper parsing
//! `amkd: serving --role api on 127.0.0.1:8080` with a regex is the state this replaces.
//!
//! A TTY gets the human-readable formatter instead, because a developer running `amkd` in a
//! terminal is not a log shipper, and making them read JSON to see a startup line is a tax with no
//! payer. The switch is on `is_terminal()`, not on a flag somebody has to remember.

use std::io::IsTerminal;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Install the global subscriber. Call once, before anything that logs.
///
/// `AMK_LOG` (falling back to `RUST_LOG`) sets the filter, so an operator can raise a level on a
/// running deployment's next restart without a rebuild. The default is `info` for our crates and
/// `warn` for everything else -- sqlx at `info` logs every statement, which at any real volume
/// buries the lines that matter and can echo bound parameters.
pub fn init() {
    let filter = EnvFilter::try_from_env("AMK_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| {
            EnvFilter::new(
                "warn,amk_http=info,amk_cli=info,amk_store=info,amk_ingest=info,amk_outbound=info",
            )
        });

    let registry = tracing_subscriber::registry().with(filter);
    if std::io::stdout().is_terminal() {
        registry
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .init();
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .init();
    }
}
