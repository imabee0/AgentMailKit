//! Shared ingest-test scaffolding: Postgres skip/require, seed helpers, SMTP client, MIME builder.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use amk_ingest::{
    serve_session, Accepted, Delivery, Envelope, FixedInboxLookup, IngestConfig, IngestError,
    Persist,
};
use amk_store::inboxes::{self, NewInbox};
use amk_store::organizations::{self, NewOrganization};
use amk_store::pods::{self, NewPod};
use amk_types::ids::{InboxId, OrganizationId, PodId};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const DATABASE_URL: &str = "postgres://amk:amk-dev-local@127.0.0.1:55432/amk";

pub async fn pool() -> Option<PgPool> {
    match amk_store::connect(DATABASE_URL).await {
        Ok(p) => Some(p),
        Err(e @ sqlx::Error::Migrate(_)) => {
            let msg = format!(
                "the dev database is reachable but its migration history disagrees with this \
                 checkout's migrations/ directory ({e})"
            );
            if std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1") {
                panic!("{msg}");
            }
            eprintln!("skipping: {msg}");
            None
        }
        Err(e) => {
            if std::env::var("AMK_REQUIRE_DB").as_deref() == Ok("1") {
                panic!(
                    "AMK_REQUIRE_DB=1 but the dev database is unreachable ({e}). \
                     Run `./scripts/dev-db.sh up`."
                );
            }
            eprintln!("skipping: dev database unreachable ({e})");
            None
        }
    }
}

pub fn unique_suffix() -> String {
    Uuid::new_v4().simple().to_string()
}

pub async fn seed_org(pool: &PgPool) -> OrganizationId {
    let id = OrganizationId::new(format!("org-{}", unique_suffix()));
    organizations::create(
        pool,
        NewOrganization {
            organization_id: id.clone(),
            name: None,
            inbox_limit: None,
            domain_limit: None,
        },
    )
    .await
    .expect("seed organization");
    id
}

pub async fn seed_pod(pool: &PgPool, org: &OrganizationId) -> PodId {
    let pod_id = PodId::new_random();
    pods::create(
        pool,
        NewPod { organization_id: org.clone(), pod_id, client_id: None, name: "test-pod".into() },
    )
    .await
    .expect("seed pod");
    pod_id
}

pub async fn seed_inbox_at(
    pool: &PgPool,
    org: &OrganizationId,
    pod: PodId,
    address: &str,
) -> InboxId {
    let inbox = inboxes::create(
        pool,
        NewInbox {
            inbox_id: InboxId::new(address),
            organization_id: org.clone(),
            pod_id: pod,
            client_id: None,
            display_name: None,
            metadata: None,
        },
    )
    .await
    .expect("seed inbox");
    inbox.inbox_id
}

pub async fn seed_org_pod_inbox(pool: &PgPool) -> (OrganizationId, PodId, InboxId) {
    let org = seed_org(pool).await;
    let pod = seed_pod(pool, &org).await;
    let addr = format!("inbox-{}@local.test", unique_suffix());
    let inbox = seed_inbox_at(pool, &org, pod, &addr).await;
    (org, pod, inbox)
}

#[derive(Clone, Default)]
pub struct CountingPersist {
    pub calls: Arc<AtomicUsize>,
}

impl Persist for CountingPersist {
    async fn persist(
        &self,
        _raw: &[u8],
        _envelope: &Envelope,
        _dest: &Delivery,
        _max_message_bytes: usize,
    ) -> Result<Accepted, IngestError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(IngestError::rejected(554, "persist must not run"))
    }
}

pub fn short_pause_config(local_domains: &[&str], max_message_bytes: usize) -> IngestConfig {
    IngestConfig::new("mx.test", local_domains, max_message_bytes, Duration::from_millis(20))
}

pub async fn spawn_smtp(
    config: IngestConfig,
    lookup: FixedInboxLookup,
    persist: impl Persist + Clone + 'static,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    assert_ne!(addr.port(), 25, "tests must never listen on :25");
    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let config = config.clone();
            let lookup = lookup.clone();
            let persist = persist.clone();
            tokio::spawn(async move {
                let _ = serve_session(stream, peer, &config, &lookup, &persist).await;
            });
        }
    });
    addr
}

pub struct SmtpClient {
    stream: TcpStream,
}

impl SmtpClient {
    pub async fn connect_after_banner(addr: SocketAddr) -> (Self, String) {
        let stream = TcpStream::connect(addr).await.expect("connect");
        let mut c = Self { stream };
        let banner = c.read_reply().await;
        (c, banner)
    }

    pub async fn connect_raw(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("connect");
        Self { stream }
    }

    pub async fn cmd(&mut self, line: &str) -> String {
        self.try_cmd(line).await.expect("write")
    }

    pub async fn try_cmd(&mut self, line: &str) -> Result<String, std::io::Error> {
        self.stream.write_all(line.as_bytes()).await?;
        if !line.ends_with("\r\n") {
            self.stream.write_all(b"\r\n").await?;
        }
        self.stream.flush().await?;
        Ok(self.read_reply().await)
    }

    pub async fn read_reply(&mut self) -> String {
        let mut out = String::new();
        let mut buf = [0u8; 1];
        let mut line = Vec::new();
        loop {
            let n = self.stream.read(&mut buf).await.expect("read");
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

    pub async fn data(&mut self, raw: &[u8]) -> String {
        let intro = self.cmd("DATA").await;
        assert!(intro.starts_with("354"), "DATA intro must be 354, got {intro:?}");
        self.stream.write_all(raw).await.expect("data");
        if !raw.ends_with(b"\r\n") {
            self.stream.write_all(b"\r\n").await.expect("data crlf");
        }
        self.stream.write_all(b".\r\n").await.expect("dot");
        self.stream.flush().await.expect("flush");
        self.read_reply().await
    }
}

pub fn reply_code(reply: &str) -> u16 {
    reply.get(..3).and_then(|s| s.parse().ok()).unwrap_or(0)
}

pub struct MimeSpec {
    pub from: String,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub extra_headers: Vec<String>,
    pub body: String,
    pub content_type: Option<String>,
}

impl MimeSpec {
    pub fn simple(from: &str, to: &str, subject: &str, message_id: &str, body: &str) -> Self {
        Self {
            from: from.into(),
            to: Some(to.into()),
            subject: Some(subject.into()),
            message_id: Some(message_id.into()),
            in_reply_to: None,
            references: None,
            extra_headers: Vec::new(),
            body: body.into(),
            content_type: Some("text/plain; charset=utf-8".into()),
        }
    }

    pub fn render(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("From: {}\r\n", self.from));
        if let Some(to) = &self.to {
            s.push_str(&format!("To: {to}\r\n"));
        }
        if let Some(subject) = &self.subject {
            s.push_str(&format!("Subject: {subject}\r\n"));
        }
        if let Some(mid) = &self.message_id {
            s.push_str(&format!("Message-ID: {mid}\r\n"));
        }
        if let Some(irt) = &self.in_reply_to {
            s.push_str(&format!("In-Reply-To: {irt}\r\n"));
        }
        if let Some(refs) = &self.references {
            s.push_str(&format!("References: {refs}\r\n"));
        }
        for h in &self.extra_headers {
            s.push_str(h);
            if !h.ends_with("\r\n") {
                s.push_str("\r\n");
            }
        }
        if let Some(ct) = &self.content_type {
            s.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        s.push_str("\r\n");
        s.push_str(&self.body);
        if !self.body.ends_with('\n') {
            s.push_str("\r\n");
        }
        s.into_bytes()
    }
}

pub fn mid(tag: &str) -> String {
    format!("<{tag}-{}@probe.test>", unique_suffix())
}
