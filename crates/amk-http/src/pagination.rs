//! Pagination parameter parsing, shared by the six list operations in this dispatch.
//!
//! `[SPEC:contract]`: these six take `limit` and `page_token` only, plus `ascending` on four of
//! them (`/v0/pods`, `/v0/inboxes`, `/v0/pods/{pod_id}/inboxes`, `/v0/api-keys`) — the other two
//! (`/v0/pods/{pod_id}/api-keys`, `/v0/inboxes/{inbox_id}/api-keys`) do not carry `ascending`, so
//! their query struct omits the field entirely rather than accepting-and-ignoring it.

use amk_store::SortDirection;
use serde::Deserialize;

/// `limit`: default 100, maximum 100, `[ASSUMED]` — no fixture observed an omitted `limit`; see
/// the dispatch contract for the reasoning. A `limit` above the maximum is clamped, never
/// rejected — no fixture shows a `validation_error` for it.
pub const DEFAULT_LIMIT: u64 = 100;
pub const MAX_LIMIT: u64 = 100;

/// The query parameters for the four list endpoints that carry `ascending`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<u64>,
    pub page_token: Option<String>,
    pub ascending: Option<bool>,
}

/// The query parameters for the two api-key list endpoints that do not carry `ascending`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQueryNoDirection {
    pub limit: Option<u64>,
    pub page_token: Option<String>,
}

/// A parsed query, resolved to a concrete limit and sort direction.
pub struct Resolved {
    pub limit: u64,
    /// `Some` only when the caller supplied `limit` — echoed in the envelope only then. Every
    /// fixture observation passed `limit` explicitly; what the server emits for an omitted one is
    /// unobserved, so this crate never claims an observation it does not have.
    pub echo_limit: Option<u64>,
    pub direction: SortDirection,
    pub page_token: Option<String>,
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

fn resolve(limit: Option<u64>, page_token: Option<String>, ascending: Option<bool>) -> Resolved {
    let clamped = limit.map(|l| l.min(MAX_LIMIT)).unwrap_or(DEFAULT_LIMIT);
    Resolved {
        limit: clamped,
        echo_limit: limit,
        direction: direction_for(ascending),
        page_token,
    }
}

impl ListQuery {
    pub fn resolve(&self) -> Resolved {
        resolve(self.limit, self.page_token.clone(), self.ascending)
    }
}

impl ListQueryNoDirection {
    pub fn resolve(&self) -> Resolved {
        resolve(self.limit, self.page_token.clone(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_omitted_limit_defaults_to_100_and_is_not_echoed() {
        let r = ListQuery::default().resolve();
        assert_eq!(r.limit, 100);
        assert_eq!(r.echo_limit, None, "an internal default must never be echoed as if observed");
    }

    #[test]
    fn a_supplied_limit_is_echoed_verbatim_even_under_the_max() {
        let r = ListQuery { limit: Some(3), ..Default::default() }.resolve();
        assert_eq!(r.limit, 3);
        assert_eq!(r.echo_limit, Some(3));
    }

    #[test]
    fn a_limit_above_the_maximum_is_clamped_not_rejected() {
        let r = ListQuery { limit: Some(9_999_999), ..Default::default() }.resolve();
        assert_eq!(r.limit, 100, "clamped to the maximum, never rejected as invalid");
        assert_eq!(r.echo_limit, Some(9_999_999), "echo is the caller's own value, not the clamp");
    }

    #[test]
    fn limit_zero_is_not_clamped_up_it_means_an_empty_page() {
        let r = ListQuery { limit: Some(0), ..Default::default() }.resolve();
        assert_eq!(r.limit, 0);
    }

    #[test]
    fn ascending_true_is_ascending_and_everything_else_is_descending() {
        assert_eq!(direction_for(Some(true)), SortDirection::Ascending);
        assert_eq!(direction_for(Some(false)), SortDirection::Descending);
        assert_eq!(direction_for(None), SortDirection::Descending, "fixture 22: newest first");
    }

    #[test]
    fn no_direction_variant_always_resolves_descending_regardless_of_field_absence() {
        let r = ListQueryNoDirection { limit: Some(5), page_token: None }.resolve();
        assert_eq!(r.direction, SortDirection::Descending);
    }
}
