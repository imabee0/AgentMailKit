//! Process tests for `amkd --role smtpd`. Cases 2–5 of `.claude/contracts/amk-smtpd.md`.
//!
//! Cases 3–5 spawn the compiled binary (not `serve_session` / `accept` / `FixedInboxLookup`).
//! Restoring `not_yet_implemented(Smtpd)` never reaches 220. Deleting only the local-domain
//! RCPT check 250s `relay@gmail.com`.

mod support;

use amk_outbound::Keyring;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use amk_cli::commands::init;
use amk_http::{router, AppConfig, AppState};
use amk_store::inboxes::{self, NewInbox};
use amk_types::ids::{InboxId, MessageId};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const PRIMARY_DOMAIN: &str = "amk-smtpd-process.test";
const RELAY_GMAIL: &str = "relay@gmail.com";
const MAIL_FROM_NO_SPF: &str = "sender@amk-no-spf.invalid";

struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

fn wait_for_port(addr: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if Instant::now() > deadline {
            panic!("amkd --role smtpd never started listening on {addr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_smtpd(dsn: &str, bind: &str) -> ServerGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_amkd"))
        .args(["--role", "smtpd"])
        .env_clear()
        .env("AMK_DATABASE_URL", dsn)
        .env("AMK_BIND", bind)
        .env("AMK_PRIMARY_DOMAIN", PRIMARY_DOMAIN)
        .spawn()
        .expect("amkd must be spawnable");
    ServerGuard(child)
}

struct Smtp {
    stream: TcpStream,
}

impl Smtp {
    fn connect_after_banner(addr: &str) -> (Self, String) {
        let stream = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .expect("write timeout");
        let mut s = Self { stream };
        let banner = s.read_reply();
        (s, banner)
    }

    fn cmd(&mut self, line: &str) -> String {
        self.stream.write_all(line.as_bytes()).expect("write");
        if !line.ends_with("\r\n") {
            self.stream.write_all(b"\r\n").expect("crlf");
        }
        self.stream.flush().expect("flush");
        self.read_reply()
    }

    fn read_reply(&mut self) -> String {
        let mut out = String::new();
        let mut line = Vec::new();
        let mut buf = [0u8; 1];
        loop {
            let n = self.stream.read(&mut buf).expect("read");
            if n == 0 {
                break;
            }
            line.push(buf[0]);
            if buf[0] == b'\n' {
                let s = String::from_utf8_lossy(&line).into_owned();
                let cont = s.as_bytes().get(3) == Some(&b'-');
                out.push_str(&s);
                line.clear();
                if !cont {
                    break;
                }
            }
        }
        out
    }

    fn data(&mut self, raw: &[u8]) -> String {
        let intro = self.cmd("DATA");
        assert!(intro.starts_with("354"), "DATA intro must be 354, got {intro:?}");
        self.stream.write_all(raw).expect("data");
        if !raw.ends_with(b"\r\n") {
            self.stream.write_all(b"\r\n").expect("data crlf");
        }
        self.stream.write_all(b".\r\n").expect("dot");
        self.stream.flush().expect("flush");
        self.read_reply()
    }
}

fn reply_code(reply: &str) -> u16 {
    reply.get(..3).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn rfc822(
    from: &str,
    to: &str,
    subject: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    body: &str,
) -> Vec<u8> {
    let mut s =
        format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: {message_id}\r\n");
    if let Some(irt) = in_reply_to {
        s.push_str(&format!("In-Reply-To: {irt}\r\n"));
    }
    s.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
    s.push_str(body);
    if !body.ends_with('\n') {
        s.push_str("\r\n");
    }
    s.into_bytes()
}

async fn get_message(pool: &sqlx::PgPool, key: &str, inbox: &InboxId, message_id: &str) -> Value {
    let app = router(AppState::new(pool.clone(), AppConfig::default(), Keyring::new()));
    let uri = format!(
        "/v0/inboxes/{}/messages/{}",
        inbox.to_path_segment(),
        MessageId::new(message_id).to_path_segment(),
    );
    let request = Request::builder()
        .method("GET")
        .uri(&uri)
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .expect("valid request");
    let response = app.oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(
        status,
        StatusCode::OK,
        "GET-by-id {uri} failed ({status}): {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("json")
}

fn wait_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("amkd --role smtpd did not exit within {timeout:?}");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Case 2: DB is set, `AMK_PRIMARY_DOMAIN` is not → exit 1 naming that variable. Store empty.
#[tokio::test]
async fn amkd_role_smtpd_without_primary_domain_names_the_variable() {
    let Some(db) = support::FreshDb::create("amkd_smtpd_no_primary_domain").await else {
        return;
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_amkd"))
        .args(["--role", "smtpd"])
        .env_clear()
        .env("AMK_DATABASE_URL", &db.dsn)
        .env("AMK_BIND", format!("127.0.0.1:{}", pick_port()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("amkd must be spawnable");

    let status = wait_exit(&mut child, Duration::from_secs(10));
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }
    let combined = format!("{stdout}{stderr}");
    assert!(!status.success(), "must refuse to start: {combined}");
    assert_eq!(status.code(), Some(1), "{combined}");
    assert!(
        combined.contains("AMK_PRIMARY_DOMAIN"),
        "must name AMK_PRIMARY_DOMAIN: {combined}"
    );
    assert!(
        !combined.contains("not implemented"),
        "must not use the old rejection: {combined}"
    );

    db.drop_it().await;
}

/// Case 3: seed `relay@gmail.com` (lookup would be Some). RCPT that address → 550.
/// Deleting only the local-domain check becomes 250. Restoring not-implemented never reaches 220.
#[tokio::test]
async fn amkd_role_smtpd_rcpt_of_seeded_gmail_is_550() {
    let Some(db) = support::FreshDb::create("amkd_smtpd_gmail_relay").await else {
        return;
    };
    let outcome = init::run_with_pool(&db.pool, None).await.expect("init");
    inboxes::create(
        &db.pool,
        NewInbox {
            inbox_id: InboxId::new(RELAY_GMAIL),
            organization_id: outcome.organization_id,
            pod_id: outcome.pod_id,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .expect("seed relay@gmail.com");

    let addr = format!("127.0.0.1:{}", pick_port());
    assert!(!addr.ends_with(":25"), "tests must never listen on :25");
    let _guard = spawn_smtpd(&db.dsn, &addr);
    wait_for_port(&addr, Duration::from_secs(10));

    let (mut c, banner) = Smtp::connect_after_banner(&addr);
    assert!(banner.starts_with("220"), "banner must be 220, got {banner:?}");
    assert_eq!(reply_code(&c.cmd("EHLO client.test")), 250);
    assert_eq!(reply_code(&c.cmd(&format!("MAIL FROM:<{MAIL_FROM_NO_SPF}>"))), 250);
    let rcpt = c.cmd(&format!("RCPT TO:<{RELAY_GMAIL}>"));
    assert_eq!(reply_code(&rcpt), 550, "gmail.com is not local; got {rcpt:?}");

    db.drop_it().await;
}

/// Cases 4–5: live Authenticator, MAIL FROM a no-SPF domain. DATA persists (`received`);
/// a second DATA with In-Reply-To of the first shares `thread_id`. Same listening smtpd.
#[tokio::test]
async fn amkd_role_smtpd_data_persists_and_reply_joins_thread() {
    let Some(db) = support::FreshDb::create("amkd_smtpd_inject").await else {
        return;
    };
    let outcome = init::run_with_pool(&db.pool, None).await.expect("init");
    let inbox = InboxId::new(format!("user@{PRIMARY_DOMAIN}"));
    inboxes::create(
        &db.pool,
        NewInbox {
            inbox_id: inbox.clone(),
            organization_id: outcome.organization_id,
            pod_id: outcome.pod_id,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .expect("seed local inbox");

    let addr = format!("127.0.0.1:{}", pick_port());
    assert!(!addr.ends_with(":25"), "tests must never listen on :25");
    let _guard = spawn_smtpd(&db.dsn, &addr);
    wait_for_port(&addr, Duration::from_secs(10));

    let root_mid = format!("<root-{}@amk-no-spf.invalid>", uuid::Uuid::new_v4().simple());
    let reply_mid = format!("<reply-{}@amk-no-spf.invalid>", uuid::Uuid::new_v4().simple());
    let raw_root = rfc822(MAIL_FROM_NO_SPF, inbox.as_str(), "hello", &root_mid, None, "root body");

    let (mut c, banner) = Smtp::connect_after_banner(&addr);
    assert!(banner.starts_with("220"), "banner must be 220, got {banner:?}");
    assert_eq!(reply_code(&c.cmd("EHLO client.test")), 250);
    assert_eq!(reply_code(&c.cmd(&format!("MAIL FROM:<{MAIL_FROM_NO_SPF}>"))), 250);
    assert_eq!(reply_code(&c.cmd(&format!("RCPT TO:<{}>", inbox.as_str()))), 250);
    let data = c.data(&raw_root);
    assert_eq!(reply_code(&data), 250, "root DATA must be 250, got {data:?}");
    let _ = c.cmd("QUIT");

    let key = outcome.root_key.api_key.as_str();
    let root = get_message(&db.pool, key, &inbox, &root_mid).await;
    let labels = root["labels"].as_array().expect("labels");
    assert!(
        labels.iter().any(|v| v.as_str() == Some("received")),
        "GET-by-id labels must contain received: {root}"
    );
    let root_thread = root["thread_id"].as_str().expect("thread_id").to_owned();

    // Bare In-Reply-To (fixture 21): the session must still join.
    let raw_reply = rfc822(
        MAIL_FROM_NO_SPF,
        inbox.as_str(),
        "Re: hello",
        &reply_mid,
        Some(root_mid.trim_start_matches('<').trim_end_matches('>')),
        "reply body",
    );
    let (mut c2, _) = Smtp::connect_after_banner(&addr);
    assert_eq!(reply_code(&c2.cmd("EHLO client.test")), 250);
    assert_eq!(reply_code(&c2.cmd(&format!("MAIL FROM:<{MAIL_FROM_NO_SPF}>"))), 250);
    assert_eq!(reply_code(&c2.cmd(&format!("RCPT TO:<{}>", inbox.as_str()))), 250);
    let data2 = c2.data(&raw_reply);
    assert_eq!(reply_code(&data2), 250, "reply DATA must be 250, got {data2:?}");

    let reply = get_message(&db.pool, key, &inbox, &reply_mid).await;
    assert_eq!(
        reply["thread_id"].as_str(),
        Some(root_thread.as_str()),
        "reply must join the injected inbound thread: root={root} reply={reply}"
    );

    db.drop_it().await;
}
