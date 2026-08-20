//! Infrastructure probes must not share a rate-limit bucket with application traffic.
//!
//! `scripts/binary-smoke.sh` found this the hard way. Its Gate 7 fires five deliberately-forged
//! download tokens to prove that a bad token is refused; each is a 403, and each was charged the
//! 20x auth-failure surcharge against the anonymous bucket for 127.0.0.1. The very next request in
//! the script -- `GET /health` -- answered **429**.
//!
//! In a cluster that is a pod restart. Liveness failing turns "someone is guessing credentials"
//! into "the API server is being killed and restarted", which is both an outage and the worst
//! possible response to the input. The fix keeps a limit on `/ready` and `/metrics` (they do real
//! work) but on a separate `Subject::Probe` bucket, and exempts `/health` entirely because it
//! touches nothing.
//!
//! These are DB-backed because `/ready` queries the pool -- a probe test that stubbed that out
//! would not be testing the endpoint a kubelet calls.
mod support;

use axum::http::StatusCode;

/// Requests with NO Authorization header land on the anonymous `Subject::Ip` bucket -- the exact
/// one Gate 7's forged tokens drained. Every one of these is a 401.
async fn drain_the_anonymous_bucket(app: &axum::Router) {
    let mut throttled = false;
    for _ in 0..40 {
        let r = support::send(app, "GET", "/v0/inboxes", None, None).await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            throttled = true;
            break;
        }
    }
    assert!(
        throttled,
        "40 unauthenticated requests did not throttle the anonymous bucket -- the surcharge is \
         not being applied, and the rest of this test would pass vacuously"
    );
}

#[tokio::test]
async fn a_drained_application_bucket_does_not_throttle_liveness() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let app = support::test_router(pool);
    drain_the_anonymous_bucket(&app).await;

    // The assertion that would have caught the defect. Ten in a row, because `/health` is exempt
    // outright: not one of them may be refused, however far into debt the application bucket is.
    for i in 0..10 {
        let r = support::send(&app, "GET", "/health", None, None).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "/health answered {} on probe {i} while the application bucket was throttled -- a \
             kubelet reads that as a dead process and restarts the pod",
            r.status
        );
    }
}

#[tokio::test]
async fn a_drained_application_bucket_does_not_throttle_readiness_or_metrics() {
    let Some(pool) = support::pool().await else {
        return;
    };
    let app = support::test_router(pool);
    drain_the_anonymous_bucket(&app).await;

    for path in ["/ready", "/metrics"] {
        let r = support::send(&app, "GET", path, None, None).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{path} answered {} -- it is sharing a bucket with application traffic",
            r.status
        );
    }
}

#[tokio::test]
async fn the_probe_bucket_is_still_a_bucket() {
    // The other direction, and the reason `/ready` and `/metrics` are isolated rather than
    // exempted: both do real work -- `/ready` takes a pooled connection -- so an unauthenticated
    // endpoint with no limit at all is a pool-starvation primitive. Widening the exemption from
    // `/health` to all three must fail here.
    let Some(pool) = support::pool().await else {
        return;
    };
    let app = support::test_router(pool);

    let mut throttled_at = None;
    for i in 0..200 {
        let r = support::send(&app, "GET", "/ready", None, None).await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            throttled_at = Some(i);
            break;
        }
    }
    assert!(
        throttled_at.is_some(),
        "200 consecutive /ready requests were never throttled -- the probe bucket is not charged, \
         so an unauthenticated caller can hold the connection pool open indefinitely"
    );

    // And liveness survives even that: the endpoint whose refusal restarts the pod is exempt from
    // every bucket, including the probe one it would otherwise share.
    let r = support::send(&app, "GET", "/health", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "/health was throttled by the probe bucket");
}
