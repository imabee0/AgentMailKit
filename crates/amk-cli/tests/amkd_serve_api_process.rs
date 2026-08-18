//! Process-level test for `crate::server::serve_api` — the one code path nothing else in this
//! crate's suite reaches. `tests/process.rs`'s `amkd` tests deliberately withhold
//! `AMK_DATABASE_URL` so they never get past the connect step, and `tests/init_and_serve.rs`
//! builds its own `AppState` directly, bypassing `serve_api` entirely. So a regression that
//! silently discards the operator's configuration — `AppState::new(pool, config)` mutated to
//! `AppState::new(pool, AppConfig::default())` — compiles and passes every other test in this
//! crate.
//!
//! That is not cosmetic: `amk_http`'s own rule is that `AppConfig::primary_domain: None` makes
//! `POST /v0/inboxes` without an explicit `domain` fail closed (internal error), so the mutant's
//! real-world signature is an operator who sets `AMK_PRIMARY_DOMAIN` correctly and still gets a
//! failure on every inbox creation that omits a domain. The only way to observe that is to spawn
//! the real `amkd --role api` binary, point it at a real (throwaway) database, and make a real
//! HTTP request through the router it actually served — so this test does exactly that, with no
//! new dependency: a hand-rolled `Connection: close` HTTP/1.1 request over `std::net::TcpStream`
//! (mirroring `reference/fixtures/24-p0-gate-sdk-authme.txt`'s own raw-probe step) rather than an
//! HTTP client crate.

mod support;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use amk_cli::commands::init;

const PRIMARY_DOMAIN: &str = "amk-cli-process-test.example";
const PRODUCT_NAME: &str = "AmkCliProcessTestProduct";

struct RawResponse {
    status: u16,
    body: String,
}

/// A trivial hand-rolled HTTP/1.1 client — see the module doc for why this is not a new
/// dependency. `Connection: close` makes the server close the socket once it has answered, so
/// reading to EOF is enough; no keep-alive/chunked-body handling is needed for this one request.
fn http_request(addr: &str, method: &str, path: &str, bearer: &str, body: &str) -> RawResponse {
    let mut stream =
        TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to amkd at {addr}: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("set_read_timeout");
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {bearer}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(request.as_bytes()).expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_owned();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    RawResponse { status, body }
}

fn wait_for_port(addr: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if Instant::now() > deadline {
            panic!("amkd never started listening on {addr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An unused-but-bound listener's port, released immediately before the subprocess binds it —
/// the standard "pick a free ephemeral port" trick. A test-to-test collision on the freed port
/// is possible in principle but not in practice at this crate's test-suite size.
fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The survivor this test exists to kill: `serve_api` must build `AppState` from the config it
/// was actually given, not `AppConfig::default()`. `AMK_PRIMARY_DOMAIN`/`AMK_PRODUCT_NAME` are set
/// to values that cannot be confused with "unset" (unlike `reference/fixtures/
/// 24-p0-gate-sdk-authme.txt`'s capture, which ran with both variables unset and so could not
/// have told the mutant apart from the real behaviour), and the assertion is on the router's own
/// observable response, not on anything this test could have gotten right by construction.
#[tokio::test]
async fn amkd_role_api_serves_the_configured_app_config_not_the_default() {
    let Some(db) = support::FreshDb::create("amkd_role_api_serves_configured_app_config").await
    else {
        return;
    };

    let outcome = init::run_with_pool(&db.pool, None)
        .await
        .expect("init must succeed against a fresh db");

    let addr = format!("127.0.0.1:{}", pick_port());
    let child = Command::new(env!("CARGO_BIN_EXE_amkd"))
        .args(["--role", "api"])
        .env_clear()
        .env("AMK_DATABASE_URL", &db.dsn)
        .env("AMK_BIND", &addr)
        .env("AMK_PRIMARY_DOMAIN", PRIMARY_DOMAIN)
        .env("AMK_PRODUCT_NAME", PRODUCT_NAME)
        .spawn()
        .expect("amkd must be spawnable");
    let _guard = ServerGuard(child);

    wait_for_port(&addr, Duration::from_secs(5));

    // POST /v0/inboxes with an empty body: no `domain`, no `display_name`. Against the real
    // `serve_api`, both are filled in from the AppConfig this process was actually given. Against
    // the mutant (`AppConfig::default()`), `primary_domain: None` makes this fail closed with an
    // internal error instead.
    let resp = http_request(&addr, "POST", "/v0/inboxes", &outcome.root_key.api_key, "{}");
    assert_eq!(resp.status, 200, "unexpected status; body: {}", resp.body);
    assert!(
        resp.body.contains(&format!("@{PRIMARY_DOMAIN}")),
        "the operator's AMK_PRIMARY_DOMAIN never reached the served router: {}",
        resp.body
    );
    assert!(
        resp.body.contains(PRODUCT_NAME),
        "the operator's AMK_PRODUCT_NAME never reached the served router: {}",
        resp.body
    );

    db.drop_it().await;
}
