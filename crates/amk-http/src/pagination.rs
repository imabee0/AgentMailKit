//! Pagination parameter parsing, shared by the six list operations in this dispatch.
//!
//! `[SPEC:contract]`: these six take `limit` and `page_token` only, plus `ascending` on four of
//! them (`/v0/pods`, `/v0/inboxes`, `/v0/pods/{pod_id}/inboxes`, `/v0/api-keys`) — the other two
//! (`/v0/pods/{pod_id}/api-keys`, `/v0/inboxes/{inbox_id}/api-keys`) do not carry `ascending`, so
//! their query struct omits the field entirely rather than accepting-and-ignoring it.

use amk_store::SortDirection;
use amk_types::ValidationIssue;
use serde::Deserialize;

use crate::body::validation_error;
use crate::error::AppError;

/// `limit`: default 100 when the caller omits it, `[ASSUMED]` — no fixture observed an omitted
/// `limit`. There is deliberately **no maximum**: see [`parse_limit`].
pub const DEFAULT_LIMIT: u64 = 100;

/// The query parameters for the four list endpoints that carry `ascending`.
///
/// `limit` is `Option<String>`, not `Option<u64>`, and that is load-bearing rather than sloppy —
/// [`parse_limit`] carries the whole reason.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub page_token: Option<String>,
    pub ascending: Option<bool>,
}

/// The query parameters for the two api-key list endpoints that do not carry `ascending`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQueryNoDirection {
    pub limit: Option<String>,
    pub page_token: Option<String>,
}

/// A parsed query, resolved to a concrete limit and sort direction.
pub struct Resolved {
    pub limit: u64,
    /// `Some` only when the caller supplied `limit`, and then it is the caller's own value,
    /// **verbatim and uncapped**.
    ///
    /// This used to be the *clamped* value, on the reasoning that the envelope's `limit` should
    /// describe the page actually returned. `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
    /// §1 settles it the other way: `GET /v0/pods?limit=101` is 200 and the response echoes
    /// `"limit":101`, with the fixture noting in as many words that no upper cap is enforced. The
    /// old note conceded the point in advance — "which value an over-limit request echoes is
    /// itself unobserved" — and the probe then observed it. Sixth time a live capture has beaten a
    /// reasoned reading.
    pub echo_limit: Option<u64>,
    pub direction: SortDirection,
    pub page_token: Option<String>,
}

/// Classifies a raw `limit` query value into the exact issue
/// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1 observed for it.
///
/// # Why this is hand-written instead of `Option<u64>` and serde
///
/// The fixture requires four inputs to split into two *different* bodies:
///
/// | input        | required outcome                                                   |
/// |--------------|--------------------------------------------------------------------|
/// | `?limit=abc` | `invalid_type`, `expected:"number"`, `received:"NaN"`               |
/// | `?limit=-1`  | `too_small`, `origin:"number"`, `minimum:0`, `inclusive:false`      |
/// | `?limit=`    | `too_small` — the fixture records "identical body to limit=-1"      |
/// | `?limit=0`   | `too_small` — likewise identical                                    |
///
/// `Option<u64>` cannot express that split, which is why the first dispatch against this
/// contract could not satisfy it and pinned the divergence in a test instead. `u64::from_str`
/// fails on `"-1"`, `""` and `"abc"` with the *same* `ParseIntError` kind rendering, so all three
/// would have to collapse to one answer; and `"0"` parses fine, so it would never reach a
/// validator at all. Taking the value as a string and classifying it here is the only way to
/// reproduce the observed behaviour — and it deletes the `limit` half of the serde-message string
/// matching in [`crate::body`] rather than adding to it, so an upstream reword of
/// `ParseIntError`'s `Display` can no longer change what this endpoint returns.
///
/// # Why there is no upper bound
///
/// The fixture accepts `101` and explicitly records that no cap is enforced; `MAX_LIMIT` (and the
/// clamp that made `?limit=101` answer `100`) are gone with it. `limit` reaches Postgres as a
/// `LIMIT` bound, so a pathological value returns at most the caller's own scoped rows — it is not
/// the unbounded *buffer* that `AppConfig::max_body_bytes` exists to prevent, and the two are not
/// the same risk. Reintroducing a ceiling is a plan decision needing a fixture, not a quiet edit.
///
/// # Why the error type is `AppError` and not `ValidationIssue`
///
/// `ValidationIssue` is 216 bytes and would be the size every `?` in this module propagates —
/// clippy's `result_large_err`, denied by `./scripts/check.sh`. `AppError` already exists for
/// exactly this reason (`crate::error::AppError`'s own doc: it boxes `ErrorEnvelope` because
/// "every fallible handler in this crate returns `Result<_, AppError>`, so its size is the size
/// every `?` propagates"). Returning it here reuses that decision instead of taking a second one,
/// and it has the side benefit that the unit tests below assert against the envelope actually put
/// on the wire rather than an intermediate value.
fn parse_limit(raw: &str) -> Result<u64, AppError> {
    // `[SPEC:…27…]` §1: an empty value is `too_small`, NOT a parse failure — the fixture records
    // `?limit=` returning a body identical to `?limit=-1`, i.e. the reference coerces it to 0 and
    // then applies the >0 rule. Checked first because "" parses as neither an integer nor a NaN.
    if raw.is_empty() {
        return Err(too_small_limit());
    }
    match raw.parse::<u64>() {
        // The only accepting branch. Verbatim, uncapped.
        Ok(v) if v > 0 => Ok(v),
        // `?limit=0`, and any `u64` that is zero.
        Ok(_) => Err(too_small_limit()),
        Err(_) => {
            // Not a non-negative integer. Split the negatives back out: they are a *number* that
            // is too small, which is a different issue from "not a number at all". `i128` rather
            // than `i64` so a value below `i64::MIN` is still classified as the negative it
            // plainly is rather than falling through to NaN.
            // `[INFERRED]`: no fixture probes a fractional (`?limit=1.5`) or an over-`u64` value.
            // Both land on NaN here, which is the answer the fixture gives for every non-integer
            // it did probe; flagged rather than silently assumed.
            match raw.parse::<i128>() {
                Ok(_) => Err(too_small_limit()),
                Err(_) => Err(validation_error(ValidationIssue::invalid_type(
                    "number",
                    Some("NaN"),
                    Some("limit"),
                    "Invalid input: expected number, received NaN",
                ))),
            }
        }
    }
}

/// The one `too_small` body, shared so the three inputs the fixture calls "identical" cannot drift
/// apart. `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1, whole object.
fn too_small_limit() -> AppError {
    validation_error(ValidationIssue::too_small(
        "number",
        0,
        false,
        Some("limit"),
        "Too small: expected number to be >0",
    ))
}

/// Default direction when `ascending` is omitted: **descending (newest first)**.
///
/// `[TESTED]` `reference/fixtures/22-org-mount-and-delete-semantics.txt`: `GET /v0/pods` with no
/// `ascending` parameter returned the pod created at `05:39:29` before the one created at
/// `04:16:23` — newest first. Fixture 04's "ascending default: temporal" describes the same
/// observation from the other side (timestamp is the tiebreak key); fixture 22 is the one that
/// pins the direction.
fn direction_for(ascending: Option<bool>) -> SortDirection {
    match ascending {
        Some(true) => SortDirection::Ascending,
        Some(false) | None => SortDirection::Descending,
    }
}

fn resolve(
    limit: Option<&str>,
    page_token: Option<String>,
    ascending: Option<bool>,
) -> Result<Resolved, AppError> {
    let parsed = match limit {
        Some(raw) => Some(parse_limit(raw)?),
        None => None,
    };
    Ok(Resolved {
        limit: parsed.unwrap_or(DEFAULT_LIMIT),
        // The caller's own value, uncapped — see the field's own doc.
        echo_limit: parsed,
        direction: direction_for(ascending),
        page_token,
    })
}

impl ListQuery {
    pub fn resolve(&self) -> Result<Resolved, AppError> {
        resolve(self.limit.as_deref(), self.page_token.clone(), self.ascending)
    }
}

impl ListQueryNoDirection {
    pub fn resolve(&self) -> Result<Resolved, AppError> {
        resolve(self.limit.as_deref(), self.page_token.clone(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(limit: &str) -> ListQuery {
        ListQuery { limit: Some(limit.to_owned()), ..Default::default() }
    }

    #[test]
    fn an_omitted_limit_defaults_to_100_and_is_not_echoed() {
        let r = ListQuery::default()
            .resolve()
            .expect("an omitted limit is not a rejection");
        assert_eq!(r.limit, 100);
        assert_eq!(r.echo_limit, None, "an internal default must never be echoed as if observed");
    }

    #[test]
    fn a_supplied_limit_is_echoed_verbatim() {
        let r = q("3").resolve().expect("3 is a valid limit");
        assert_eq!(r.limit, 3);
        assert_eq!(r.echo_limit, Some(3));
    }

    /// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` §1: `?limit=101` is 200 and
    /// the response echoes `"limit":101`. This is the assertion that the old clamp inverted — it
    /// answered `100` to both questions.
    #[test]
    fn a_limit_above_one_hundred_is_neither_clamped_nor_rejected() {
        let r = q("101")
            .resolve()
            .expect("fixture 27 §1: 101 is accepted, not rejected");
        assert_eq!(r.limit, 101, "the caller's value reaches the query uncapped");
        assert_eq!(r.echo_limit, Some(101), "fixture 27 §1 echoes 101, not the old clamped 100");

        let big = q("9999999").resolve().expect("no upper cap is enforced");
        assert_eq!(big.limit, 9_999_999);
        assert_eq!(big.echo_limit, Some(9_999_999));
    }

    /// The single `errors[]` entry the envelope actually carries — every assertion below reads
    /// what goes on the wire, not an intermediate.
    fn only_issue(err: &AppError) -> &ValidationIssue {
        assert_eq!(err.0.errors.len(), 1, "exactly one errors[] entry: {:?}", err.0.errors);
        &err.0.errors[0]
    }

    /// The three inputs fixture 27 §1 records as producing *identical* bodies. Asserting the whole
    /// issue, not just its code: `minimum`/`inclusive`/`origin` are exactly the extras that a
    /// `ValidationIssue`-models-only-{code,path,message} bug would have dropped silently.
    #[test]
    fn empty_negative_and_zero_limits_are_all_the_same_too_small_issue() {
        for raw in ["", "-1", "0"] {
            let err = parse_limit(raw).expect_err("fixture 27 §1: all three are rejected");
            let issue = only_issue(&err);
            assert_eq!(issue.code, "too_small", "limit={raw:?}");
            assert_eq!(issue.origin.as_deref(), Some("number"), "limit={raw:?}");
            assert_eq!(issue.minimum, Some(0), "limit={raw:?}");
            assert_eq!(issue.inclusive, Some(false), "limit={raw:?}");
            assert_eq!(issue.path, vec![serde_json::json!("limit")], "limit={raw:?}");
            assert_eq!(issue.message, "Too small: expected number to be >0", "limit={raw:?}");
            assert_eq!(issue.expected, None, "too_small carries origin, never expected");
        }
    }

    /// The other half of the split, and the one the old `Option<u64>` collapsed together with the
    /// negatives. `received:"NaN"` is present here and absent from `too_small` above.
    #[test]
    fn a_non_numeric_limit_is_invalid_type_nan_not_too_small() {
        let err = parse_limit("abc").expect_err("fixture 27 §1: abc is rejected");
        let issue = only_issue(&err);
        assert_eq!(issue.code, "invalid_type");
        assert_eq!(issue.expected.as_deref(), Some("number"));
        assert_eq!(issue.received.as_deref(), Some("NaN"));
        assert_eq!(issue.path, vec![serde_json::json!("limit")]);
        assert_eq!(issue.message, "Invalid input: expected number, received NaN");
        assert_eq!(issue.minimum, None, "NaN is not a number that is too small");
    }

    /// A negative below `i64::MIN` is still plainly a negative number, not a NaN — the reason
    /// `parse_limit` reaches for `i128`. Falsified by narrowing it to `i64`, which flips this to
    /// `invalid_type`.
    #[test]
    fn a_negative_below_i64_min_is_still_too_small_not_nan() {
        let err = parse_limit("-99999999999999999999").expect_err("negative is rejected");
        assert_eq!(only_issue(&err).code, "too_small");
    }

    #[test]
    fn ascending_true_is_ascending_and_everything_else_is_descending() {
        assert_eq!(direction_for(Some(true)), SortDirection::Ascending);
        assert_eq!(direction_for(Some(false)), SortDirection::Descending);
        assert_eq!(direction_for(None), SortDirection::Descending, "fixture 22: newest first");
    }

    #[test]
    fn no_direction_variant_always_resolves_descending_regardless_of_field_absence() {
        let r = ListQueryNoDirection { limit: Some("5".to_owned()), page_token: None }
            .resolve()
            .expect("5 is a valid limit");
        assert_eq!(r.direction, SortDirection::Descending);
    }
}
