//! Hand-written argument parsing for `amk` and `amkd`.
//!
//! **No `clap`, no argument-parsing dependency of any kind** — see the dispatch contract's own
//! reasoning: the whole surface is four subcommands and one flag, which is a few dozen lines of
//! plain matching, fully unit-testable without spawning a process. Every function here operates
//! on an explicit `&[String]` (never `std::env::args()` itself) precisely so it stays that way —
//! `main.rs` is the only place that touches the real argv.
//!
//! A parser that silently accepts garbage is how a deployment ends up running the wrong role, so
//! every rejection here names what was wrong and carries the usage text, never a bare exit code.

use std::fmt;

/// A parsing failure. Always paired with a non-zero exit by the caller (`--help` is the only
/// zero-exit outcome, and it is not an error at all — see [`AmkCommand::Help`]/[`AmkdCommand::Help`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

pub const AMK_HELP: &str = "\
amk -- AgentMailKit control-plane CLI

USAGE:
    amk <COMMAND>

COMMANDS:
    init       Initialise a fresh deployment: default organization, default pod, root API key.
    migrate    Apply pending database migrations and report the resulting state.
    doctor     Read-only deployment diagnostics. Safe to paste: never prints a configuration value.

    -h, --help  Print this message and exit.

Every command reads AMK_DATABASE_URL from the environment (required, no default).";

/// A parsed `amk` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmkCommand {
    Init,
    Migrate,
    Doctor,
    Help,
}

/// Parse `amk`'s argv, excluding the program name (`std::env::args().skip(1)`).
pub fn parse_amk(args: &[String]) -> Result<AmkCommand, UsageError> {
    match args {
        [] => Err(UsageError(format!("amk: missing command\n\n{AMK_HELP}"))),
        [one] => match one.as_str() {
            "-h" | "--help" => Ok(AmkCommand::Help),
            "init" => Ok(AmkCommand::Init),
            "migrate" => Ok(AmkCommand::Migrate),
            "doctor" => Ok(AmkCommand::Doctor),
            other => Err(UsageError(format!("amk: unknown command {other:?}\n\n{AMK_HELP}"))),
        },
        [first, ..] => Err(UsageError(format!(
            "amk: unexpected extra argument(s) after {first:?}\n\n{AMK_HELP}"
        ))),
    }
}

pub const AMKD_HELP: &str = "\
amkd -- AgentMailKit server process

USAGE:
    amkd --role <ROLE>

ROLES:
    api      Serve the HTTP control-plane API.
    smtpd    Serve inbound SMTP ingest (amk-ingest).
    worker   Background job processing -- not implemented yet (amk-jobs).
    all      Every role above -- not implemented yet (requires all of the above).

    -h, --help  Print this message and exit.

`--role api` reads AMK_DATABASE_URL (required), AMK_BIND (default 127.0.0.1:8080),
AMK_PRIMARY_DOMAIN and AMK_PRODUCT_NAME (both optional) from the environment.
`--role smtpd` reads AMK_DATABASE_URL (required), AMK_BIND (SMTP listen address;
default 127.0.0.1:8080), and AMK_PRIMARY_DOMAIN (required; there is no default).";

/// The role named by a well-formed `--role <ROLE>` — every value this parser recognises, whether
/// or not that role is actually runnable yet. Whether it is runnable is `crate::server`'s call,
/// not the parser's: a role the parser doesn't recognise at all is a *usage* error (this module),
/// but a role it recognises and rejects as not-yet-implemented is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmkdRole {
    Api,
    Smtpd,
    Worker,
    All,
}

/// A parsed `amkd` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmkdCommand {
    Serve(AmkdRole),
    Help,
}

/// Parse `amkd`'s argv, excluding the program name.
pub fn parse_amkd(args: &[String]) -> Result<AmkdCommand, UsageError> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(AmkdCommand::Help);
    }
    match args {
        [] => Err(UsageError(format!("amkd: missing --role\n\n{AMKD_HELP}"))),
        [flag] if flag == "--role" => {
            Err(UsageError(format!("amkd: --role requires a value\n\n{AMKD_HELP}")))
        }
        [flag, value] if flag == "--role" => match value.as_str() {
            "api" => Ok(AmkdCommand::Serve(AmkdRole::Api)),
            "smtpd" => Ok(AmkdCommand::Serve(AmkdRole::Smtpd)),
            "worker" => Ok(AmkdCommand::Serve(AmkdRole::Worker)),
            "all" => Ok(AmkdCommand::Serve(AmkdRole::All)),
            other => Err(UsageError(format!("amkd: unknown --role {other:?}\n\n{AMKD_HELP}"))),
        },
        _ => Err(UsageError(format!("amkd: expected exactly `--role <ROLE>`\n\n{AMKD_HELP}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    // ---- amk: clean paths ---------------------------------------------------------------

    #[test]
    fn amk_recognises_each_real_command() {
        assert_eq!(parse_amk(&v(&["init"])), Ok(AmkCommand::Init));
        assert_eq!(parse_amk(&v(&["migrate"])), Ok(AmkCommand::Migrate));
        assert_eq!(parse_amk(&v(&["doctor"])), Ok(AmkCommand::Doctor));
    }

    #[test]
    fn amk_help_short_and_long_both_exit_clean() {
        assert_eq!(parse_amk(&v(&["--help"])), Ok(AmkCommand::Help));
        assert_eq!(parse_amk(&v(&["-h"])), Ok(AmkCommand::Help));
    }

    // ---- amk: assigned edge cases --------------------------------------------------------

    #[test]
    fn amk_no_arguments_is_a_named_usage_error() {
        let err = parse_amk(&v(&[])).unwrap_err();
        assert!(err.0.contains("missing command"), "message did not name the problem: {err}");
    }

    #[test]
    fn amk_unknown_subcommand_names_it() {
        // `import` in particular: P6, not this dispatch, and deliberately not a recognised
        // subcommand — the correct behaviour today is the ordinary "unknown command" error, not a
        // stub. See the dispatch contract's own "amk import — does not exist yet" section.
        for bad in ["import", "frobnicate", "INIT"] {
            let err = parse_amk(&v(&[bad])).unwrap_err();
            assert!(
                err.0.contains(&format!("{bad:?}")),
                "error for {bad:?} did not name the unrecognised command: {err}"
            );
        }
    }

    #[test]
    fn amk_extra_arguments_are_rejected_not_silently_ignored() {
        let err = parse_amk(&v(&["init", "extra"])).unwrap_err();
        assert!(
            err.0.contains("unexpected extra argument"),
            "message did not name the problem: {err}"
        );
        assert!(err.0.contains("\"init\""), "message did not name the leading argument: {err}");
    }

    // ---- amkd: clean paths -----------------------------------------------------------------

    #[test]
    fn amkd_recognises_every_role_string() {
        assert_eq!(parse_amkd(&v(&["--role", "api"])), Ok(AmkdCommand::Serve(AmkdRole::Api)));
        assert_eq!(parse_amkd(&v(&["--role", "smtpd"])), Ok(AmkdCommand::Serve(AmkdRole::Smtpd)));
        assert_eq!(parse_amkd(&v(&["--role", "worker"])), Ok(AmkdCommand::Serve(AmkdRole::Worker)));
        assert_eq!(parse_amkd(&v(&["--role", "all"])), Ok(AmkdCommand::Serve(AmkdRole::All)));
    }

    #[test]
    fn amkd_help_short_and_long_both_exit_clean() {
        assert_eq!(parse_amkd(&v(&["--help"])), Ok(AmkdCommand::Help));
        assert_eq!(parse_amkd(&v(&["-h"])), Ok(AmkdCommand::Help));
        // --help wins even alongside other (otherwise invalid) arguments.
        assert_eq!(parse_amkd(&v(&["--role", "--help"])), Ok(AmkdCommand::Help));
    }

    // ---- amkd: assigned edge cases -----------------------------------------------------------

    #[test]
    fn amkd_no_arguments_is_a_named_usage_error() {
        let err = parse_amkd(&v(&[])).unwrap_err();
        assert!(err.0.contains("--role"), "message did not name what was missing: {err}");
    }

    #[test]
    fn amkd_missing_role_value_is_a_named_usage_error() {
        let err = parse_amkd(&v(&["--role"])).unwrap_err();
        assert!(
            err.0.contains("requires a value"),
            "message did not name the missing value: {err}"
        );
    }

    #[test]
    fn amkd_unknown_role_names_it() {
        let err = parse_amkd(&v(&["--role", "bogus"])).unwrap_err();
        assert!(err.0.contains("\"bogus\""), "message did not name the unknown role: {err}");
    }

    #[test]
    fn amkd_role_matching_is_exact_not_prefix_or_case_insensitive() {
        // A future "helpful" widening (prefix match, case folding) would silently accept a typo
        // like "API" or "ap" as the real "api" role — reject both.
        for bad in ["API", "ap", " api", "api "] {
            let err = parse_amkd(&v(&["--role", bad])).unwrap_err();
            assert!(err.0.contains("unknown"), "{bad:?} was accepted as a role: {err}");
        }
    }
}
