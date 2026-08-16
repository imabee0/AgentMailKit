//! Process-level (black-box) tests: the compiled `amk`/`amkd` binaries, run as real
//! subprocesses via `CARGO_BIN_EXE_*` with an explicitly controlled (`env_clear`-ed)
//! environment. This is deliberate for everything environment-shaped: process env is
//! process-global mutable state, and `cargo test`'s default thread-parallel execution makes
//! mutating it in-process across tests a race (see `crate::config`'s own note on this, and why
//! it keeps exactly one in-process test that does). A subprocess's environment is its own, so
//! there is nothing to race here, and it is also the literal thing the dispatch contract asks
//! for: "asserted on the captured output, not by reading the code."
//!
//! None of these tests need the dev database — they exercise argv/env plumbing, the fail-closed
//! missing-variable path, and the connection-failure redaction path (against a fast-refusing
//! address, never a real database), so they run unconditionally.

use std::process::{Command, Output};

fn run_amk(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amk"));
    cmd.args(args).env_clear();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("amk must be spawnable")
}

fn run_amkd(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amkd"));
    cmd.args(args).env_clear();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("amkd must be spawnable")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn did_not_panic(output: &Output) {
    let combined = text(output);
    assert!(
        !combined.contains("panicked at"),
        "process panicked instead of failing cleanly:\n{combined}"
    );
}

// ---- amk: the argument parser, as a real process --------------------------------------------

#[test]
fn amk_no_arguments_exits_nonzero_and_names_the_problem() {
    let out = run_amk(&[], &[]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("missing command"));
    did_not_panic(&out);
}

#[test]
fn amk_unknown_command_exits_nonzero_and_names_it() {
    let out = run_amk(&["frobnicate"], &[]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("\"frobnicate\""));
}

#[test]
fn amk_help_exits_zero() {
    let out = run_amk(&["--help"], &[]);
    assert!(out.status.success());
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out).contains("USAGE"));
}

// ---- amk: AMK_DATABASE_URL, fail-closed and non-panicking ------------------------------------

#[test]
fn amk_missing_database_url_is_a_clear_error_naming_the_variable_and_never_panics() {
    for cmd in ["init", "migrate", "doctor"] {
        let out = run_amk(&[cmd], &[]);
        // `doctor` is read-only and reports "unset" for a missing AMK_DATABASE_URL rather than
        // refusing outright (its whole job is to run even when configuration is incomplete), so
        // only `init`/`migrate` are asserted non-zero here.
        if cmd != "doctor" {
            assert!(!out.status.success(), "{cmd} must fail without AMK_DATABASE_URL");
            assert_eq!(out.status.code(), Some(1));
        }
        assert!(
            text(&out).contains("AMK_DATABASE_URL"),
            "{cmd}: message did not name the missing variable: {}",
            text(&out)
        );
        did_not_panic(&out);
    }
}

/// The variable name is exactly `AMK_DATABASE_URL`, not any looser match on it — an unprefixed
/// `DATABASE_URL` (a common convention elsewhere, and exactly the kind of "helpful" fallback a
/// widened guard would add) must NOT satisfy the requirement.
#[test]
fn amk_does_not_fall_back_to_an_unprefixed_database_url_variable() {
    let out = run_amk(&["migrate"], &[("DATABASE_URL", "postgres://amk:x@127.0.0.1:1/amk")]);
    assert!(
        !out.status.success(),
        "an unprefixed DATABASE_URL must not satisfy the requirement"
    );
    assert!(text(&out).contains("AMK_DATABASE_URL is not set"));
}

// ---- amk: the DSN password must never appear in a failed connection's output ----------------

#[test]
fn amk_migrate_never_leaks_the_dsn_password_on_a_failed_connection() {
    const SENTINEL: &str = "amk-cli-process-test-sentinel-password";
    // Port 1 on loopback: well-formed DSN, fast connection-refused rather than a 5s pool timeout.
    let dsn = format!("postgres://amk:{SENTINEL}@127.0.0.1:1/amk");
    let out = run_amk(&["migrate"], &[("AMK_DATABASE_URL", &dsn)]);
    assert!(!out.status.success());
    let combined = text(&out);
    assert!(!combined.contains(SENTINEL), "password leaked into process output:\n{combined}");
    assert!(combined.contains("AMK_DATABASE_URL"));
    did_not_panic(&out);
}

#[test]
fn amk_init_never_leaks_the_dsn_password_on_a_failed_connection() {
    const SENTINEL: &str = "amk-cli-process-test-init-sentinel-password";
    let dsn = format!("postgres://amk:{SENTINEL}@127.0.0.1:1/amk");
    let out = run_amk(&["init"], &[("AMK_DATABASE_URL", &dsn)]);
    assert!(!out.status.success());
    let combined = text(&out);
    assert!(!combined.contains(SENTINEL), "password leaked into process output:\n{combined}");
    did_not_panic(&out);
}

// ---- amk doctor: never a configuration VALUE, only set/unset --------------------------------

#[test]
fn amk_doctor_never_prints_a_configured_value_only_set_or_unset() {
    const DSN_SENTINEL: &str = "amk-cli-doctor-process-dsn-sentinel";
    const BIND_SENTINEL: &str = "amk-cli-doctor-process-bind-sentinel";
    const DOMAIN_SENTINEL: &str = "amk-cli-doctor-process-domain-sentinel";
    const PRODUCT_SENTINEL: &str = "amk-cli-doctor-process-product-sentinel";

    let dsn = format!("postgres://amk:{DSN_SENTINEL}@127.0.0.1:1/amk");
    let out = run_amk(
        &["doctor"],
        &[
            ("AMK_DATABASE_URL", dsn.as_str()),
            ("AMK_BIND", BIND_SENTINEL),
            ("AMK_PRIMARY_DOMAIN", DOMAIN_SENTINEL),
            ("AMK_PRODUCT_NAME", PRODUCT_SENTINEL),
        ],
    );
    assert!(out.status.success(), "doctor is read-only and must not fail: {}", text(&out));
    let combined = text(&out);
    assert!(!combined.contains(DSN_SENTINEL), "leaked the DSN password:\n{combined}");
    assert!(!combined.contains(BIND_SENTINEL), "leaked AMK_BIND's value:\n{combined}");
    assert!(
        !combined.contains(DOMAIN_SENTINEL),
        "leaked AMK_PRIMARY_DOMAIN's value:\n{combined}"
    );
    assert!(
        !combined.contains(PRODUCT_SENTINEL),
        "leaked AMK_PRODUCT_NAME's value:\n{combined}"
    );
    // It must still say something useful: every variable reported present, and the DSN's
    // reachability outcome named (not merely silent).
    assert!(combined.contains("AMK_DATABASE_URL: set"));
    assert!(combined.contains("AMK_BIND: set"));
    assert!(combined.contains("AMK_PRIMARY_DOMAIN: set"));
    assert!(combined.contains("AMK_PRODUCT_NAME: set"));
    did_not_panic(&out);
}

#[test]
fn amk_doctor_reports_unset_variables_as_unset_with_no_value() {
    let out = run_amk(&["doctor"], &[]);
    assert!(out.status.success());
    let combined = text(&out);
    assert!(combined.contains("AMK_DATABASE_URL: unset"));
    assert!(combined.contains("AMK_BIND: unset"));
    assert!(combined.contains("AMK_PRIMARY_DOMAIN: unset"));
    assert!(combined.contains("AMK_PRODUCT_NAME: unset"));
}

// ---- amkd: the argument parser, as a real process --------------------------------------------

#[test]
fn amkd_no_arguments_exits_nonzero_and_names_the_problem() {
    let out = run_amkd(&[], &[]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("--role"));
}

#[test]
fn amkd_missing_role_value_exits_nonzero_and_names_it() {
    let out = run_amkd(&["--role"], &[]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("requires a value"));
}

#[test]
fn amkd_unknown_role_exits_nonzero_and_names_it() {
    let out = run_amkd(&["--role", "bogus"], &[]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("\"bogus\""));
}

#[test]
fn amkd_help_exits_zero() {
    let out = run_amkd(&["--help"], &[]);
    assert!(out.status.success());
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out).contains("USAGE"));
}

// ---- amkd: unimplemented roles are recognised and rejected, never silently accepted ----------

#[test]
fn amkd_role_smtpd_worker_all_are_each_rejected_naming_the_phase_and_never_reach_the_database() {
    for (role, needle) in [
        ("smtpd", "amk-ingest"),
        ("worker", "amk-jobs"),
        ("all", "every role"),
    ] {
        // No AMK_DATABASE_URL at all: if the role were silently accepted and reached the connect
        // step, the failure message would be about AMK_DATABASE_URL, not about the role. Passing
        // no database url is exactly what makes that distinguishable and keeps this test DB-free.
        let out = run_amkd(&["--role", role], &[]);
        assert!(!out.status.success(), "--role {role} must exit non-zero");
        assert_eq!(out.status.code(), Some(1));
        let combined = text(&out);
        assert!(
            combined.contains(role),
            "--role {role}'s rejection message did not name the role: {combined}"
        );
        assert!(
            combined.contains(needle),
            "--role {role}'s rejection message did not name what implements it: {combined}"
        );
        assert!(
            !combined.contains("AMK_DATABASE_URL"),
            "--role {role} reached the connect step instead of being rejected first: {combined}"
        );
        did_not_panic(&out);
    }
}
