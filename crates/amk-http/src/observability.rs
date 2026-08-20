//! Liveness, readiness, metrics, and the request-id span every log line hangs off.
//!
//! # Why these are here and not deferred to P4
//!
//! `docs/PLAN.md`:190 requires "logs structured to stdout, key-ids never key material" and :206
//! names a Prometheus `/metrics` surface. An audit on 2026-08-19 found NEITHER: zero `tracing::`
//! call sites workspace-wide, 26 `println!`s, and no probe endpoints at all -- so a Kubernetes
//! deployment could not health-check the thing it was running, and an incident would have been
//! answered by grepping unstructured stdout.
//!
//! It lands now rather than at P4 because `amk-events`, `amk-jobs`, `amk-dns` and `amk-mcp` do not
//! exist yet. Threading instrumentation through four crates costs a fraction of threading it
//! through eight, and every crate written after this one inherits the convention instead of being
//! retrofitted.
//!
//! # Why the metrics are hand-rolled
//!
//! `metrics` + `metrics-exporter-prometheus` is the obvious reach and would be two more
//! dependencies plus a global recorder. What is actually needed is a handful of monotonic counters
//! rendered as Prometheus text -- about sixty lines of `AtomicU64`. This follows the precedent
//! `docs/PLAN.md`:108 set when it chose a `cargo metadata` walk over cargo-deny for the
//! dependency-direction check: "zero extra tooling, exact graph". If histograms or labelled
//! cardinality are ever genuinely needed, that is the moment to take the dependency, not now.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use amk_types::ErrorCode;

use crate::error::AppError;
use crate::ratelimit::{RateLimiter, Subject};
use crate::AppState;

/// The counters `docs/PLAN.md`:206 names, restricted to the ones that can be TRUE today.
///
/// The plan's full list includes webhook delivery backlog, queue depth per job kind, and bounce
/// rate -- all of which belong to `amk-events` and `amk-jobs`, crates that do not exist. Exporting
/// them now as permanent zeroes would be worse than omitting them: a dashboard showing
/// `amk_webhook_failures_total 0` reads as "no failures", not as "no webhooks". Each is added by
/// the dispatch that makes it real.
#[derive(Debug, Default)]
pub struct Metrics {
    pub http_requests_total: AtomicU64,
    pub http_responses_5xx_total: AtomicU64,
    pub http_responses_4xx_total: AtomicU64,
    pub internal_errors_total: AtomicU64,
    pub throttled_total: AtomicU64,
    pub dkim_signing_failures_total: AtomicU64,
    pub ingest_accepted_total: AtomicU64,
    pub ingest_rejected_relay_denied_total: AtomicU64,
    pub ingest_rejected_unknown_recipient_total: AtomicU64,
    pub ingest_rejected_size_total: AtomicU64,
    pub ingest_parse_failures_total: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(c: &AtomicU64) -> u64 {
        c.load(Ordering::Relaxed)
    }

    /// Prometheus text exposition format (v0.0.4).
    ///
    /// Every counter carries HELP and TYPE. A scraper tolerates their absence; a human reading an
    /// unfamiliar dashboard at 3am does not.
    pub fn render(&self) -> String {
        // Length inferred, deliberately: an explicit `[...; N]` is a second record of the row
        // count, and it drifted the first time a counter was added.
        let rows: &[(&str, &str, u64)] = &[
            (
                "amk_http_requests_total",
                "HTTP requests received",
                Self::get(&self.http_requests_total),
            ),
            (
                "amk_http_responses_4xx_total",
                "HTTP responses with a 4xx status",
                Self::get(&self.http_responses_4xx_total),
            ),
            (
                "amk_http_responses_5xx_total",
                "HTTP responses with a 5xx status",
                Self::get(&self.http_responses_5xx_total),
            ),
            (
                "amk_internal_errors_total",
                "Errors mapped to the opaque internal-error response",
                Self::get(&self.internal_errors_total),
            ),
            (
                "amk_throttled_total",
                "Requests refused with 429 by the rate limiter",
                Self::get(&self.throttled_total),
            ),
            (
                "amk_dkim_signing_failures_total",
                "Outbound sends that could not be DKIM-signed",
                Self::get(&self.dkim_signing_failures_total),
            ),
            (
                "amk_ingest_accepted_total",
                "Inbound SMTP messages accepted and stored",
                Self::get(&self.ingest_accepted_total),
            ),
            (
                "amk_ingest_rejected_relay_denied_total",
                "Inbound RCPT rejected: not a local domain",
                Self::get(&self.ingest_rejected_relay_denied_total),
            ),
            (
                "amk_ingest_rejected_unknown_recipient_total",
                "Inbound RCPT rejected: no such inbox",
                Self::get(&self.ingest_rejected_unknown_recipient_total),
            ),
            (
                "amk_ingest_rejected_size_total",
                "Inbound messages rejected: over the size cap",
                Self::get(&self.ingest_rejected_size_total),
            ),
            (
                "amk_ingest_parse_failures_total",
                "Inbound messages that failed to parse",
                Self::get(&self.ingest_parse_failures_total),
            ),
        ];
        let mut out = String::with_capacity(2048);
        for &(name, help, value) in rows {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"));
        }
        out
    }
}

/// `GET /health` -- liveness. Answers 200 as long as the process can serve, and touches NOTHING
/// else.
///
/// Deliberately does not check the database. A liveness probe that fails on a dependency outage
/// tells Kubernetes to restart the pod, and restarting an API server does not fix Postgres -- it
/// turns a degraded service into a crash-loop while the database recovers. Dependency health is
/// `/ready`'s job, and the two are different questions with different consequences.
pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "ok\n")
}

/// `GET /ready` -- readiness. 200 when this instance can serve real traffic, 503 when it cannot.
///
/// Checks the database, because every endpoint but this one needs it. A 503 here removes the
/// instance from the load-balancer's rotation without killing it, which is the correct response to
/// "the dependency is down": stop sending it work, leave it alive to recover.
pub async fn ready(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "ready\n")
            .into_response(),
        Err(e) => {
            // The reason is logged, never returned: a readiness probe is unauthenticated, and a
            // connection string in its body would be a disclosure to anyone who can reach the
            // port. Same rule as `AppError::internal`.
            tracing::error!(error = %e, "readiness check failed: database unreachable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "not ready\n",
            )
                .into_response()
        }
    }
}

/// `GET /metrics` -- Prometheus scrape target.
///
/// Unauthenticated, like the probes, and that is a deliberate exposure decision rather than an
/// oversight: the values are aggregate counts with no per-tenant cardinality -- no inbox id, no
/// domain, no key id -- so scraping it reveals traffic volume and nothing about whose traffic it
/// is. `docs/PLAN.md`:206 has it scraped by the cluster's existing monitoring namespace, which
/// reaches it over the pod network; exposing it publicly is a deployment choice this code cannot
/// make. Adding a labelled metric later means revisiting this line.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        state.metrics.render(),
    )
}

/// The header a caller can supply to correlate its own logs with ours.
const REQUEST_ID: &str = "x-request-id";

/// Liveness. Skips the limiter outright -- it allocates nothing and reads nothing, so there is no
/// resource for a limit to protect, and it is the endpoint whose refusal restarts the pod.
const HEALTH_PATH: &str = "/health";
/// Every infrastructure endpoint, limited on their own [`Subject::Probe`] bucket rather than
/// sharing the anonymous one. `/health` appears here too so that its bucket is never the
/// application's, in case the exemption above is ever narrowed.
const PROBE_PATHS: [&str; 3] = [HEALTH_PATH, "/ready", "/metrics"];

/// Per-request span, request id, and the response counters.
///
/// Hand-written rather than `tower_http::trace::TraceLayer` because tower-http is not a sanctioned
/// dependency of this crate (see `Cargo.toml`), and this is what it would have been reached for.
/// `axum::middleware::from_fn` costs no new crate.
///
/// An inbound `x-request-id` is TRUSTED AND ECHOED, and that is safe here only because the value
/// never reaches a query, a path, or a log field that is parsed -- it is recorded as an opaque
/// span field and returned verbatim. It is length-capped regardless, because an unbounded
/// attacker-controlled string in every log line is a cheap way to fill a disk.
pub async fn trace_requests(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    const MAX_ID: usize = 64;
    let incoming = request
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= MAX_ID && v.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_owned);
    let request_id = incoming.unwrap_or_else(|| Uuid::new_v4().to_string());

    // The peer address, when the server was built with `into_make_service_with_connect_info`.
    // Absent in the in-process test harness (`oneshot` has no socket), where UNSPECIFIED means
    // every test request shares one bucket -- correct, because a test that silently got its own
    // bucket per request would never observe a limit at all.
    let peer: IpAddr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    // Infrastructure endpoints are classified BEFORE the subject is derived, because the whole
    // point is that they do not share a bucket with application traffic. See `Subject::Probe`.
    let path_now = request.uri().path();
    let exempt = path_now == HEALTH_PATH;
    let subject = if PROBE_PATHS.contains(&path_now) {
        Subject::Probe(peer)
    } else {
        Subject::derive(
            request
                .headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            peer,
        )
    };

    // Checked BEFORE the handler runs, and charged at the ordinary cost. The auth-failure
    // surcharge is applied after, once the status is known -- the expensive thing an attacker
    // triggers is `authenticate`'s argon2id verify, which has already happened by then, so the
    // surcharge is what makes the NEXT attempt uneconomic rather than this one.
    if !exempt
        && !state
            .limiter
            .check(&subject, RateLimiter::cost_for_status(200))
    {
        state
            .metrics
            .http_requests_total
            .fetch_add(1, Ordering::Relaxed);
        state
            .metrics
            .throttled_total
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(?subject, "rate limited");
        // Full envelope, not the 401/403 bare body: clients branch on `code`, and
        // `ErrorCode::RateLimitExceeded` is `rate_limit_exceeded` at 429. Schemathesis's
        // `error_shape_is_one_of_the_two` (and docs.agentmail.to/errors) reject a third shape.
        let mut resp =
            AppError::new(ErrorCode::RateLimitExceeded, "Too Many Requests").into_response();
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return resp;
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let span = tracing::info_span!("http", %request_id, %method, path = %path);
    let _enter = span.enter();

    state
        .metrics
        .http_requests_total
        .fetch_add(1, Ordering::Relaxed);

    // The guard is dropped before the await: a `tracing::Span` entered across an await point
    // records time the task was not running, and holds a non-Send guard across a yield.
    drop(_enter);
    let span_for_body = span.clone();
    let mut response = {
        let _e = span_for_body.enter();
        next.run(request)
    }
    .await;

    let status = response.status();
    // The surcharge. `cost_for_status` is 20 for 401/403 and 1 otherwise, and the ordinary 1 was
    // already charged above, so a failure costs 21 in total. Deliberate: the point is that the
    // NEXT attempt is throttled.
    let extra = RateLimiter::cost_for_status(status.as_u16());
    // No `Subject::Probe` exclusion here, deliberately. One was written and then removed: it
    // survived its own mutation run because it is unreachable -- no probe path can answer 401 or
    // 403, and the only non-200 any of them emits is `/ready`'s 503, which `cost_for_status`
    // charges the ordinary cost (pinned in `ratelimit::tests`). An unpinnable guard against a
    // status that cannot occur is not defence in depth; it is a claim no test can falsify. If a
    // probe endpoint ever gains a credential, this is the line that has to change WITH a test.
    if extra > 1.0 {
        // `penalise`, not `check`: the argon2id verify has already happened, so this charge lands
        // whether or not the bucket can afford it. Using `check` here meant the surcharge stopped
        // applying the moment it was most needed.
        state.limiter.penalise(&subject, extra);
    }
    if status.is_server_error() {
        state
            .metrics
            .http_responses_5xx_total
            .fetch_add(1, Ordering::Relaxed);
    } else if status.is_client_error() {
        state
            .metrics
            .http_responses_4xx_total
            .fetch_add(1, Ordering::Relaxed);
    }
    tracing::info!(parent: &span, status = status.as_u16(), "request completed");

    // Echoed so a caller can find this request in our logs from its own.
    if let Ok(v) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID, v);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_valid_prometheus_text_with_help_and_type_for_every_counter() {
        let m = Metrics::new();
        let out = m.render();
        let names: Vec<&str> = out
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert!(!names.is_empty(), "no samples rendered");
        for n in &names {
            assert!(out.contains(&format!("# HELP {n} ")), "{n} has no HELP line");
            assert!(out.contains(&format!("# TYPE {n} counter")), "{n} has no TYPE line");
        }
        // Every line is either a comment or `name value`. A scraper rejects anything else, and a
        // rejected scrape is silent -- the dashboard just stops updating.
        for l in out.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let parts: Vec<&str> = l.split_whitespace().collect();
            assert_eq!(parts.len(), 2, "malformed sample line: {l:?}");
            parts[1].parse::<u64>().expect("value is not a number");
        }
    }

    #[test]
    fn counters_start_at_zero_and_increment() {
        let m = Metrics::new();
        assert!(m.render().contains("amk_http_requests_total 0"));
        m.http_requests_total.fetch_add(3, Ordering::Relaxed);
        assert!(m.render().contains("amk_http_requests_total 3"));
    }

    #[test]
    fn no_counter_that_cannot_be_true_yet_is_exported() {
        // Exporting a permanent zero for a subsystem that does not exist is worse than omitting
        // it: `amk_webhook_failures_total 0` reads as "no failures", not "no webhooks".
        let out = Metrics::new().render();
        for absent in ["webhook", "queue_depth", "bounce", "job"] {
            assert!(!out.contains(absent), "exported a counter for an unbuilt subsystem: {absent}");
        }
    }
}
