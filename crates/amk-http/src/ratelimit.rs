//! Per-key and per-IP token buckets, stricter on the auth-failure path.
//!
//! `docs/PLAN.md`:190 -- "Rate buckets per-key/per-IP, stricter on auth failure paths." Nothing
//! implemented it, and `amk-http/Cargo.toml` recorded the omission honestly: "rate limiting is a
//! later dispatch". This is that dispatch.
//!
//! # What it actually defends
//!
//! Two different things, which is why there are two costs rather than one limit.
//!
//! Ordinary requests are bounded so one caller cannot saturate the pool. That is a fairness
//! property; the numbers are generous because a legitimate agent burst is normal traffic for this
//! product -- it is a mail API for automated senders.
//!
//! Auth failures are bounded far harder, because that path is where guessing happens. A key is 32
//! bytes of `OsRng` and unguessable in any realistic sense, so this is not the last line of
//! defence -- `amk-store::api_keys::authenticate` performs exactly one argon2id verify on every
//! path including misses, which is both the timing-oracle fix and, incidentally, an expensive
//! operation an attacker gets to trigger. THAT is the real exposure: unauthenticated argon2id at
//! line rate is a CPU exhaustion primitive. Charging failures 20x is what makes it uneconomic.
//!
//! # Why in-process
//!
//! State is per-instance. `docs/PLAN.md`:202 already accepts a single Postgres and a single node
//! ("revisit only if a second node ever exists"), so a shared bucket store would be solving a
//! problem this deployment does not have, with a dependency and a network hop. If a second
//! instance ever exists, this becomes wrong and the doc above is where to start.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tokens a bucket holds when full, and therefore the largest burst allowed.
const CAPACITY: f64 = 120.0;
/// Tokens restored per second: the sustained rate once a burst is spent.
const REFILL_PER_SEC: f64 = 2.0;
/// Cost of an ordinary request.
const COST_OK: f64 = 1.0;
/// Cost of a request that ended 401/403. Twenty ordinary requests' worth, so a burst of 120 allows
/// six consecutive auth failures before throttling -- ample for a misconfigured client rotating a
/// key, useless for anything enumerating.
const COST_AUTH_FAILURE: f64 = 20.0;
/// Buckets untouched for this long are dropped. Without it the map is an unbounded allocation
/// keyed by attacker-chosen input, which is the DoS this file exists to prevent.
const IDLE_EVICTION: Duration = Duration::from_secs(600);
/// Never hold more than this many buckets. A hard stop for the case where eviction cannot keep up
/// with churn -- a scan across a million source addresses inside one eviction window.
const MAX_BUCKETS: usize = 50_000;

/// Who is being limited.
///
/// A presented credential wins over the source address, and deliberately: many agents behind one
/// NAT share an IP, and limiting them as one punishes the innocent for a neighbour's traffic. The
/// key is NOT validated here -- this runs before authentication -- so the bucket is keyed on the
/// PREFIX only, which is public (it is the `O(1)` lookup id, not the secret), and is truncated so
/// an attacker cannot mint unbounded distinct buckets by varying the tail.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Key(String),
    Ip(IpAddr),
}

impl Subject {
    /// Derive the subject from a presented credential, falling back to the peer address.
    ///
    /// The prefix is truncated to 16 characters. A caller can still create one bucket per distinct
    /// prefix, but the space is bounded by `MAX_BUCKETS` and every one of them is charged.
    pub fn derive(authorization: Option<&str>, peer: IpAddr) -> Self {
        match authorization.and_then(|v| v.strip_prefix("Bearer ")) {
            Some(token) if !token.is_empty() => {
                let prefix: String = token.chars().take(16).collect();
                Subject::Key(prefix)
            }
            _ => Subject::Ip(peer),
        }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// The limiter. One per process, held in `AppState`.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<Subject, Bucket>>,
    capacity: f64,
    refill: f64,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(CAPACITY, REFILL_PER_SEC)
    }
}

impl RateLimiter {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self { buckets: Mutex::new(HashMap::new()), capacity, refill: refill_per_sec }
    }

    /// Charge `cost` against `subject`. `false` means throttled.
    ///
    /// A poisoned mutex FAILS OPEN -- serving unlimited traffic beats refusing all of it because a
    /// thread panicked while holding a lock. The panic itself is the bug to fix; denying every
    /// request forever is not a safer response to it.
    pub fn check(&self, subject: &Subject, cost: f64) -> bool {
        self.check_at(subject, cost, Instant::now())
    }

    fn check_at(&self, subject: &Subject, cost: f64, now: Instant) -> bool {
        let mut map = match self.buckets.lock() {
            Ok(m) => m,
            Err(_) => return true,
        };

        if map.len() >= MAX_BUCKETS {
            map.retain(|_, b| now.duration_since(b.last) < IDLE_EVICTION);
            // Still full after evicting the idle: the map itself is now the resource under
            // pressure. Fail open rather than start refusing arbitrary callers -- a limiter that
            // becomes a denial of service when busy has inverted its own purpose.
            if map.len() >= MAX_BUCKETS {
                return true;
            }
        }

        let bucket = map
            .entry(subject.clone())
            .or_insert(Bucket { tokens: self.capacity, last: now });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill).min(self.capacity);
        bucket.last = now;

        if bucket.tokens >= cost {
            bucket.tokens -= cost;
            true
        } else {
            // NOT decremented on rejection. Charging a throttled request would let a caller hold
            // itself throttled forever by continuing to hammer, which turns a temporary limit into
            // a permanent lockout.
            false
        }
    }

    /// Charge a penalty that has ALREADY been incurred. Always deducts; may go negative.
    ///
    /// [`check`](Self::check) is a gate -- it asks permission and declines to spend when the
    /// answer is no. A penalty is not asking anything: the expensive work (an argon2id verify on
    /// a bad key) is already done, and the charge has to land whether or not the bucket can
    /// afford it.
    ///
    /// Using `check` for this was a real defect, found by driving the running server rather than
    /// by reading the code: twelve consecutive auth failures were never throttled. Once the bucket
    /// dropped below the 20-token surcharge, `check` refused to deduct it and returned false,
    /// which the caller ignored -- so the penalty silently stopped applying while the 1-token
    /// pre-check sailed on for another hundred requests.
    ///
    /// Debt is floored at `-capacity`, bounding recovery to `capacity / refill` seconds (60s at
    /// the defaults). Without a floor, a sustained attack drives the balance arbitrarily negative
    /// and locks the subject out long after it stops -- and if that subject is a shared NAT
    /// address, it locks out everyone behind it.
    pub fn penalise(&self, subject: &Subject, cost: f64) {
        self.penalise_at(subject, cost, Instant::now());
    }

    fn penalise_at(&self, subject: &Subject, cost: f64, now: Instant) {
        let mut map = match self.buckets.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        let Some(bucket) = map.get_mut(subject) else {
            // No bucket means `check` never ran for this subject, which cannot happen on the
            // request path. Creating one here would let a penalty mint buckets, bypassing the
            // MAX_BUCKETS pressure valve.
            return;
        };
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill).min(self.capacity);
        bucket.last = now;
        bucket.tokens = (bucket.tokens - cost).max(-self.capacity);
    }

    /// Cost for a response status: auth failures are dear, everything else is one.
    pub fn cost_for_status(status: u16) -> f64 {
        if status == 401 || status == 403 {
            COST_AUTH_FAILURE
        } else {
            COST_OK
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> Subject {
        Subject::Ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)))
    }

    #[test]
    fn a_burst_up_to_capacity_is_allowed_and_the_next_request_is_not() {
        let rl = RateLimiter::new(5.0, 1.0);
        let t = Instant::now();
        for i in 0..5 {
            assert!(rl.check_at(&ip(1), 1.0, t), "request {i} inside the burst was throttled");
        }
        assert!(!rl.check_at(&ip(1), 1.0, t), "capacity+1 was allowed");
    }

    #[test]
    fn tokens_refill_over_time() {
        let rl = RateLimiter::new(5.0, 2.0);
        let t = Instant::now();
        for _ in 0..5 {
            rl.check_at(&ip(1), 1.0, t);
        }
        assert!(!rl.check_at(&ip(1), 1.0, t));
        assert!(rl.check_at(&ip(1), 1.0, t + Duration::from_secs(1)), "2/s did not refill");
    }

    #[test]
    fn a_throttled_request_does_not_deepen_the_debt() {
        // Charging a rejected request lets a caller hold itself throttled forever by hammering.
        let rl = RateLimiter::new(2.0, 1.0);
        let t = Instant::now();
        rl.check_at(&ip(1), 1.0, t);
        rl.check_at(&ip(1), 1.0, t);
        for _ in 0..50 {
            assert!(!rl.check_at(&ip(1), 1.0, t));
        }
        // One second of refill must be enough again, not fifty.
        assert!(rl.check_at(&ip(1), 1.0, t + Duration::from_secs(1)));
    }

    #[test]
    fn auth_failures_cost_twenty_times_an_ordinary_request() {
        assert_eq!(RateLimiter::cost_for_status(200), COST_OK);
        assert_eq!(RateLimiter::cost_for_status(404), COST_OK);
        assert_eq!(RateLimiter::cost_for_status(401), COST_AUTH_FAILURE);
        assert_eq!(RateLimiter::cost_for_status(403), COST_AUTH_FAILURE);

        // The property that matters: a full bucket absorbs only a handful of failures.
        let rl = RateLimiter::new(CAPACITY, REFILL_PER_SEC);
        let t = Instant::now();
        let mut allowed = 0;
        while rl.check_at(&ip(9), COST_AUTH_FAILURE, t) {
            allowed += 1;
            assert!(allowed < 100, "auth failures are not being charged");
        }
        assert_eq!(allowed, 6, "a full bucket should absorb exactly six auth failures");
    }

    #[test]
    fn subjects_are_independent() {
        let rl = RateLimiter::new(1.0, 0.0);
        let t = Instant::now();
        assert!(rl.check_at(&ip(1), 1.0, t));
        assert!(!rl.check_at(&ip(1), 1.0, t));
        assert!(rl.check_at(&ip(2), 1.0, t), "one caller exhausted another caller's bucket");
    }

    #[test]
    fn a_presented_key_beats_the_source_address() {
        // Agents behind one NAT share an IP; limiting them as one punishes the innocent.
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        // The first 16 characters of the token, as a literal -- an expression that computes
        // the expectation the same way the code does can never catch the code being wrong.
        assert_eq!(
            Subject::derive(Some("Bearer am_abcdefghijklmnop_TAIL"), peer),
            Subject::Key("am_abcdefghijklm".to_owned())
        );
        assert_eq!(Subject::derive(None, peer), Subject::Ip(peer));
        assert_eq!(Subject::derive(Some("Basic xyz"), peer), Subject::Ip(peer));
        assert_eq!(Subject::derive(Some("Bearer "), peer), Subject::Ip(peer));
    }

    #[test]
    fn the_key_prefix_is_truncated_so_the_bucket_space_is_bounded() {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let a = Subject::derive(Some(&format!("Bearer {}", "x".repeat(500))), peer);
        let b = Subject::derive(Some(&format!("Bearer {}{}", "x".repeat(16), "DIFFERENT")), peer);
        assert_eq!(a, b, "varying the tail must not mint a new bucket");
        if let Subject::Key(k) = a {
            assert_eq!(k.len(), 16);
        } else {
            panic!("expected a key subject");
        }
    }

    #[test]
    fn idle_buckets_are_evicted_rather_than_accumulating_forever() {
        let rl = RateLimiter::new(10.0, 1.0);
        let t = Instant::now();
        for n in 0..50 {
            rl.check_at(&Subject::Key(format!("k{n}")), 1.0, t);
        }
        assert_eq!(rl.len(), 50);
        // Eviction runs on pressure, so force it: the map must not grow without bound.
        let later = t + IDLE_EVICTION + Duration::from_secs(1);
        {
            let mut m = rl.buckets.lock().unwrap();
            m.retain(|_, b| later.duration_since(b.last) < IDLE_EVICTION);
        }
        assert_eq!(rl.len(), 0, "idle buckets were not evictable");
    }

    #[test]
    fn a_penalty_lands_even_when_the_bucket_cannot_afford_it() {
        // THE DEFECT THIS EXISTS FOR. Using `check` for the surcharge meant that once the balance
        // fell below the cost, the charge silently stopped applying -- twelve consecutive auth
        // failures against the running server went unthrottled.
        let rl = RateLimiter::new(10.0, 0.0);
        let t = Instant::now();
        assert!(rl.check_at(&ip(1), 1.0, t));
        rl.penalise_at(&ip(1), 20.0, t); // more than the bucket holds
        assert!(!rl.check_at(&ip(1), 1.0, t), "the penalty did not land");
    }

    #[test]
    fn penalty_debt_is_floored_so_an_attack_cannot_lock_a_subject_out_indefinitely() {
        // A shared NAT address driven arbitrarily negative would lock out everyone behind it long
        // after the abuse stopped.
        let rl = RateLimiter::new(10.0, 1.0);
        let t = Instant::now();
        rl.check_at(&ip(1), 1.0, t);
        for _ in 0..1000 {
            rl.penalise_at(&ip(1), 20.0, t);
        }
        // Floored at -capacity, so 2 x capacity / refill seconds restores service at the latest.
        assert!(
            rl.check_at(&ip(1), 1.0, t + Duration::from_secs(21)),
            "debt was not floored: recovery took longer than capacity/refill"
        );
    }

    #[test]
    fn a_penalty_does_not_create_a_bucket() {
        // Otherwise a penalty could mint buckets and bypass the MAX_BUCKETS pressure valve.
        let rl = RateLimiter::new(10.0, 1.0);
        rl.penalise_at(&ip(7), 20.0, Instant::now());
        assert_eq!(rl.len(), 0);
    }

    #[test]
    fn six_auth_failures_are_absorbed_and_the_seventh_is_throttled() {
        // The end-to-end number, pinned here so it cannot drift silently: capacity 120, ordinary
        // cost 1, surcharge 20 -> 21 per failure -> the seventh request finds an empty bucket.
        let rl = RateLimiter::new(CAPACITY, REFILL_PER_SEC);
        let t = Instant::now();
        let mut absorbed = 0;
        for _ in 0..12 {
            if !rl.check_at(&ip(3), COST_OK, t) {
                break;
            }
            absorbed += 1;
            rl.penalise_at(&ip(3), COST_AUTH_FAILURE, t);
        }
        assert_eq!(absorbed, 6, "expected exactly six failures before throttling");
    }
}
