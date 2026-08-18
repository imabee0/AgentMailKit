//! SMTP state machine. `smtp-proto` parses commands; this module owns the session.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::time::Duration;

use amk_types::InboxId;
use smtp_proto::{Error as SmtpParseError, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::accept::{Delivery, Envelope, Persist};
use crate::error::IngestError;
use crate::lookup::InboxLookup;

/// Test-injected session limits. Not product constants.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub hostname: String,
    local_domains: Vec<String>,
    pub max_message_bytes: usize,
    pub greet_pause: Duration,
}

impl IngestConfig {
    pub fn new(
        hostname: impl Into<String>,
        local_domains: &[&str],
        max_message_bytes: usize,
        greet_pause: Duration,
    ) -> Self {
        Self {
            hostname: hostname.into(),
            local_domains: local_domains
                .iter()
                .map(|d| d.to_ascii_lowercase())
                .collect(),
            max_message_bytes,
            greet_pause,
        }
    }

    pub fn local_domains(&self) -> &[String] {
        &self.local_domains
    }
}

/// Mutant 1 target: RCPT domain must be in `local_domains` even when lookup returns `Some`.
pub(crate) fn rcpt_domain_is_local(local_domains: &[String], rcpt: &str) -> bool {
    let Some((_, domain)) = rcpt.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_ascii_lowercase();
    local_domains.iter().any(|d| d == &domain)
}

/// One SMTP connection: greet-pause → banner → EHLO → MAIL → RCPT → DATA → persist → 250.
///
/// No AUTH, no STARTTLS. Never binds :25 itself — the caller chooses the listen address.
pub async fn serve_session<L, P>(
    mut stream: TcpStream,
    peer: SocketAddr,
    config: &IngestConfig,
    lookup: &L,
    persist: &P,
) -> Result<(), IngestError>
where
    L: InboxLookup,
    P: Persist,
{
    if talked_too_soon(&stream, config.greet_pause).await? {
        // Drain the premature command so the peer's write finishes, then 421.
        // Closing mid-write RSTs the socket and the client never sees the reply.
        let mut discard = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_millis(200), read_line(&mut stream, &mut discard))
                .await;
        write_reply(&mut stream, 421, 4, 7, 0, "You talk too soon").await?;
        return Ok(());
    }
    write_raw(&mut stream, &format!("220 {} ESMTP\r\n", config.hostname)).await?;

    let mut buf = Vec::new();
    let mut greeted = false;
    let mut mail_from: Option<String> = None;
    let mut rcpts: Vec<Delivery> = Vec::new();
    let mut ehlo_host = String::new();

    loop {
        let line = match read_line(&mut stream, &mut buf).await? {
            Some(l) => l,
            None => return Ok(()),
        };
        let mut iter = line.iter();
        let request = match Request::<Cow<str>>::parse(&mut iter) {
            Ok(r) => r,
            Err(SmtpParseError::NeedsMoreData { .. }) => {
                write_reply(&mut stream, 500, 5, 5, 2, "Syntax error").await?;
                continue;
            }
            Err(_) => {
                write_reply(&mut stream, 500, 5, 5, 2, "Syntax error").await?;
                continue;
            }
        };

        match request {
            Request::Ehlo { host } | Request::Helo { host } => {
                greeted = true;
                ehlo_host = host.into_owned();
                mail_from = None;
                rcpts.clear();
                write_raw(
                    &mut stream,
                    &format!(
                        "250-{} hello\r\n250-SIZE {}\r\n250 8BITMIME\r\n",
                        config.hostname, config.max_message_bytes
                    ),
                )
                .await?;
            }
            Request::Mail { from } => {
                if !greeted {
                    write_reply(&mut stream, 503, 5, 5, 1, "EHLO first").await?;
                    continue;
                }
                if from.size > 0 && from.size > config.max_message_bytes {
                    write_reply(&mut stream, 552, 5, 3, 4, "Message size exceeds limit").await?;
                    continue;
                }
                mail_from = Some(from.address.into_owned());
                rcpts.clear();
                write_reply(&mut stream, 250, 2, 1, 0, "OK").await?;
            }
            Request::Rcpt { to } => {
                if mail_from.is_none() {
                    write_reply(&mut stream, 503, 5, 5, 1, "MAIL first").await?;
                    continue;
                }
                let address = to.address.into_owned();
                // Local-domain check FIRST. Deleting this arm 250s a stubbed gmail.com RCPT.
                if !rcpt_domain_is_local(&config.local_domains, &address) {
                    write_reply(&mut stream, 550, 5, 7, 1, "Relay denied").await?;
                    continue;
                }
                let inbox_id = InboxId::new(address);
                match lookup.lookup(&inbox_id).await {
                    Some((organization_id, pod_id, resolved)) => {
                        rcpts.push(Delivery { organization_id, pod_id, inbox_id: resolved });
                        write_reply(&mut stream, 250, 2, 1, 5, "OK").await?;
                    }
                    None => {
                        write_reply(&mut stream, 550, 5, 1, 1, "User unknown").await?;
                    }
                }
            }
            Request::Data => {
                if mail_from.is_none() || rcpts.is_empty() {
                    write_reply(&mut stream, 503, 5, 5, 1, "Need MAIL and RCPT").await?;
                    continue;
                }
                write_reply(&mut stream, 354, 3, 0, 0, "Start mail input; end with <CRLF>.<CRLF>")
                    .await?;
                let (raw, oversize) =
                    read_data(&mut stream, &mut buf, config.max_message_bytes).await?;
                if oversize {
                    write_reply(&mut stream, 552, 5, 3, 4, "Message size exceeds limit").await?;
                    mail_from = None;
                    rcpts.clear();
                    continue;
                }
                let envelope = Envelope {
                    mail_from: mail_from.take().unwrap_or_default(),
                    client_ip: peer.ip(),
                    ehlo_host: ehlo_host.clone(),
                };
                let mut last_err: Option<IngestError> = None;
                for dest in rcpts.drain(..) {
                    match persist
                        .persist(&raw, &envelope, &dest, config.max_message_bytes)
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => last_err = Some(e),
                    }
                }
                match last_err {
                    Some(e) => {
                        write_reply(
                            &mut stream,
                            e.smtp_code(),
                            enhanced_first(e.smtp_code()),
                            0,
                            0,
                            &e.smtp_text(),
                        )
                        .await?;
                    }
                    None => {
                        write_reply(&mut stream, 250, 2, 0, 0, "OK").await?;
                    }
                }
            }
            Request::Rset => {
                mail_from = None;
                rcpts.clear();
                write_reply(&mut stream, 250, 2, 0, 0, "OK").await?;
            }
            Request::Noop { .. } => {
                write_reply(&mut stream, 250, 2, 0, 0, "OK").await?;
            }
            Request::Quit => {
                write_reply(&mut stream, 221, 2, 0, 0, "Bye").await?;
                return Ok(());
            }
            Request::StartTls | Request::Auth { .. } => {
                write_reply(&mut stream, 502, 5, 5, 1, "Command not implemented").await?;
            }
            _ => {
                write_reply(&mut stream, 502, 5, 5, 1, "Command not implemented").await?;
            }
        }
    }
}

/// Mutant 2 target: first-byte before the pause is 421, not 250.
async fn talked_too_soon(stream: &TcpStream, pause: Duration) -> Result<bool, IngestError> {
    if pause.is_zero() {
        return Ok(false);
    }
    let mut peek = [0u8; 1];
    match tokio::time::timeout(pause, stream.peek(&mut peek)).await {
        Ok(Ok(n)) if n > 0 => Ok(true),
        Ok(Ok(_)) => Ok(false),
        Ok(Err(e)) => Err(IngestError::Io(e.to_string())),
        Err(_elapsed) => Ok(false),
    }
}

fn enhanced_first(code: u16) -> u8 {
    (code / 100) as u8
}

async fn write_reply(
    stream: &mut TcpStream,
    code: u16,
    e0: u8,
    e1: u8,
    e2: u8,
    message: &str,
) -> Result<(), IngestError> {
    write_raw(stream, &format!("{code} {e0}.{e1}.{e2} {message}\r\n")).await
}

async fn write_raw(stream: &mut TcpStream, s: &str) -> Result<(), IngestError> {
    stream
        .write_all(s.as_bytes())
        .await
        .map_err(|e| IngestError::Io(e.to_string()))?;
    stream
        .flush()
        .await
        .map_err(|e| IngestError::Io(e.to_string()))
}

async fn read_line(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>, IngestError> {
    loop {
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            return Ok(Some(buf.drain(..=pos).collect()));
        }
        let mut tmp = [0u8; 1024];
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| IngestError::Io(e.to_string()))?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 16 * 1024 {
            return Err(IngestError::rejected(500, "Line too long"));
        }
    }
}

/// Collect DATA until `<CRLF>.<CRLF>`. `oversize` is true when the unstuffed body exceeds `cap`.
async fn read_data(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    cap: usize,
) -> Result<(Vec<u8>, bool), IngestError> {
    let mut data = Vec::new();
    let mut oversize = false;
    loop {
        let line = match read_line(stream, buf).await? {
            Some(l) => l,
            None => return Err(IngestError::Io("connection closed during DATA".into())),
        };
        if is_data_end(&line) {
            break;
        }
        if oversize {
            continue;
        }
        let unstuffed = destuff(&line);
        if data.len().saturating_add(unstuffed.len()) > cap {
            oversize = true;
            data.clear();
            continue;
        }
        data.extend_from_slice(unstuffed);
    }
    Ok((data, oversize))
}

fn is_data_end(line: &[u8]) -> bool {
    matches!(line, b".\r\n" | b".\n")
}

fn destuff(line: &[u8]) -> &[u8] {
    if line.first() == Some(&b'.') && line.get(1) == Some(&b'.') {
        &line[1..]
    } else {
        line
    }
}
