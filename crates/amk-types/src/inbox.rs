//! Inbox resource.
//!
//! `inbox_id` IS the email address (live capture 03). Creation with a taken username returns
//! `already_exists` at **HTTP 403** with `suggestions[]` — see [`crate::ErrorCode::AlreadyExists`].

use crate::{ids::*, list_response, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Metadata values are scalars; `null` in an update request means "delete this key".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Bool(bool),
}

impl MetadataValue {
    /// Whether this value can be written to storage and read back as itself.
    ///
    /// # The defect this exists to close
    ///
    /// `PATCH /v0/inboxes/{id}` with `{"metadata":{"a":1.7976931348623157e+308}}` used to return
    /// **500**, and — worse — leave the inbox permanently unreadable: the row was written, and
    /// every later `GET`/`PATCH`/list touching it failed the same way. Found by the P1 gate's
    /// schemathesis conjunct (`not_a_server_error`), root-caused as a disagreement between the
    /// write path and the read path about one number:
    ///
    /// 1. serde_json accepts `1.7976931348623157e308` in **exponent form** on the way in.
    /// 2. Postgres `jsonb` normalises it to `numeric` and renders it back with **no exponent** —
    ///    `17976931348623157` followed by 292 zeros, a 309-digit integer literal.
    /// 3. serde_json parses that literal through its *long-integer* path, which is stricter than
    ///    its float path, and fails with `number out of range` — even though the value is below
    ///    [`f64::MAX`].
    ///
    /// | literal | parses as f64? |
    /// |---|---|
    /// | `1.7976931348623157e308` (what the client sends) | ok |
    /// | `1` + 308 zeros | ok |
    /// | `17976931348623157` + 292 zeros (**what jsonb emits**) | ERR |
    /// | `1` + 309 zeros | ERR |
    ///
    /// # Why this is derived and not a constant
    ///
    /// The failing boundary is a property of serde_json's long-integer parser, not a round number —
    /// note that `1e308` survives while the *smaller* `1.7976931348623157e308` does not. A
    /// hard-coded threshold would be wrong the day either side changes. So the check reconstructs
    /// jsonb's own rendering and asks serde_json directly.
    pub fn survives_storage_round_trip(&self) -> bool {
        match self {
            // Neither can be mangled by `numeric` normalisation: strings are stored verbatim and
            // booleans have two values. A string that merely LOOKS like a huge number is still a
            // string on both sides.
            Self::String(_) | Self::Bool(_) => true,
            Self::Number(v) => {
                // JSON cannot carry NaN/Infinity, so serde refuses these before they reach here.
                // Checked anyway: this guard must not depend on a caller upstream of it.
                v.is_finite() && serde_json::from_str::<f64>(&as_postgres_renders(*v)).is_ok()
            }
        }
    }
}

/// Renders `v` the way `jsonb` will hand it back: shortest round-trip decimal (Rust's `{}`, the
/// same digits Postgres keeps when it parses the literal into `numeric`) with any exponent
/// expanded into plain notation, because `numeric` output never uses an exponent.
fn as_postgres_renders(v: f64) -> String {
    let shortest = format!("{v}");
    let Some((mantissa, exp)) = shortest.split_once(['e', 'E']) else {
        return shortest;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return shortest;
    };
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int_part}{frac_part}");
    // Where the decimal point lands once the exponent is applied.
    let point = int_part.len() as i32 + exp;
    if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if (point as usize) >= digits.len() {
        format!("{sign}{digits}{}", "0".repeat(point as usize - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point as usize], &digits[point as usize..])
    }
}

pub type Metadata = BTreeMap<String, MetadataValue>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inbox {
    /// Live responses include this even though the SDK type omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<OrganizationId>,
    pub pod_id: PodId,
    pub inbox_id: InboxId,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    pub updated_at: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateInboxRequest {
    /// Randomly generated when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Defaults to the server's primary domain when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Idempotency key for creation: replay returns the original resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// The three states `UpdateInboxRequest::metadata` can be in on the wire.
///
/// `openapi.json` types the field `oneOf [UpdateMetadata, null]` and says, verbatim: *"Keys you
/// include are added or overwritten; keys you omit are left unchanged. To remove a single key,
/// send it with a null value. **To clear all metadata, send `metadata` as null.** Sending an empty
/// object is rejected; use null to clear."*
///
/// So absent, `null`, and `{…}` mean three different things. Modelled as an enum rather than
/// `Option<Option<…>>` because the states have names worth writing down, matching how
/// [`crate::api_key::KeyGrants`] already handles the same absent-versus-empty trap.
///
/// The bug this replaces: the field was `Option<BTreeMap<…>>` with `#[serde(default)]`, under
/// which `{"metadata": null}` and `{}` both deserialize to `None` — three wire states collapsed
/// into two, while the doc comment above the field asserted the distinction existed. A frozen
/// wire-type crate documenting behaviour its type cannot represent is worse than one that stays
/// silent, because downstream trusts it.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum MetadataUpdate {
    /// Field absent: metadata untouched.
    #[default]
    Unchanged,
    /// `"metadata": null` — clear every key.
    Clear,
    /// `"metadata": {…}` — merge; a key mapped to `null` deletes just that key.
    Merge(BTreeMap<String, Option<MetadataValue>>),
}

impl MetadataUpdate {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl Serialize for MetadataUpdate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            // `Unchanged` is skipped by `skip_serializing_if`, so it never reaches the wire
            // through the request type; serializing one directly is meaningless rather than
            // illegal, and `null` is the closest honest rendering.
            Self::Unchanged | Self::Clear => s.serialize_none(),
            Self::Merge(m) => m.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for MetadataUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Absence never reaches here — `#[serde(default)]` on the field supplies `Unchanged` —
        // which is precisely what makes the three states separable. A present `null` does reach
        // here, and becomes `Clear`.
        Ok(match Option::<BTreeMap<String, Option<MetadataValue>>>::deserialize(d)? {
            None => Self::Clear,
            Some(m) => Self::Merge(m),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateInboxRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Absent, `null` and `{…}` are three distinct requests — see [`MetadataUpdate`].
    #[serde(default, skip_serializing_if = "MetadataUpdate::is_unchanged")]
    pub metadata: MetadataUpdate,
}

list_response!(
    /// `{count, limit?, next_page_token?, inboxes: [...]}`
    ListInboxesResponse,
    inboxes,
    Inbox
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim create-inbox response from the live API (fixture 03).
    const LIVE: &str = r#"{"organization_id":"133c9cbe-f996-4094-a8d5-0c6603e022ea",
        "pod_id":"9047724b-2879-416b-8424-82ef81ab9397","inbox_id":"amk-probe@agentmail.to",
        "email":"amk-probe@agentmail.to","client_id":"amk-probe-inbox-1","display_name":"AMK Probe",
        "updated_at":"2026-08-15T05:39:38.948Z","created_at":"2026-08-15T05:39:38.948Z"}"#;

    #[test]
    fn parses_and_reemits_the_live_inbox_shape() {
        let inbox: Inbox = serde_json::from_str(LIVE).unwrap();
        assert_eq!(inbox.inbox_id.as_str(), "amk-probe@agentmail.to");
        assert_eq!(inbox.email, inbox.inbox_id.as_str(), "inbox_id IS the email");

        let out: serde_json::Value = serde_json::to_value(&inbox).unwrap();
        let expected: serde_json::Value = serde_json::from_str(LIVE).unwrap();
        assert_eq!(out, expected, "must round-trip the live shape exactly");
    }

    #[test]
    fn absent_metadata_is_omitted_not_null() {
        let inbox: Inbox = serde_json::from_str(LIVE).unwrap();
        let s = serde_json::to_string(&inbox).unwrap();
        assert!(!s.contains("metadata"), "absent optionals must be omitted: {s}");
    }

    #[test]
    fn update_request_distinguishes_delete_key_from_absent() {
        let req: UpdateInboxRequest =
            serde_json::from_str(r#"{"metadata":{"keep":"v","drop":null}}"#).unwrap();
        let MetadataUpdate::Merge(m) = req.metadata else {
            panic!("a present object is a merge");
        };
        assert!(m["drop"].is_none(), "null value means delete the key");
        assert!(m["keep"].is_some());
    }

    #[test]
    fn update_metadata_has_three_distinct_wire_states() {
        // openapi.json, verbatim: "To remove a single key, send it with a null value. To clear all
        // metadata, send `metadata` as null." Absent, null and {…} are three different requests,
        // and the previous Option<BTreeMap> collapsed the first two into None.
        let absent: UpdateInboxRequest = serde_json::from_str(r#"{"display_name":"x"}"#).unwrap();
        let clear: UpdateInboxRequest = serde_json::from_str(r#"{"metadata":null}"#).unwrap();
        let merge: UpdateInboxRequest = serde_json::from_str(r#"{"metadata":{"a":"1"}}"#).unwrap();

        assert_eq!(absent.metadata, MetadataUpdate::Unchanged);
        assert_eq!(clear.metadata, MetadataUpdate::Clear);
        assert!(matches!(merge.metadata, MetadataUpdate::Merge(_)));
        assert_ne!(
            absent.metadata, clear.metadata,
            "leaving metadata alone and wiping it are different requests"
        );
    }

    #[test]
    fn update_metadata_round_trips_each_state_to_the_right_json() {
        let omitted = serde_json::to_string(&UpdateInboxRequest {
            display_name: Some("x".into()),
            metadata: MetadataUpdate::Unchanged,
        })
        .unwrap();
        assert!(!omitted.contains("metadata"), "unchanged is omitted entirely: {omitted}");

        let cleared = serde_json::to_string(&UpdateInboxRequest {
            display_name: None,
            metadata: MetadataUpdate::Clear,
        })
        .unwrap();
        assert_eq!(cleared, r#"{"metadata":null}"#, "clear is the ONE place null is correct");

        let merged = serde_json::to_string(&UpdateInboxRequest {
            display_name: None,
            metadata: MetadataUpdate::Merge([("a".to_string(), None)].into_iter().collect()),
        })
        .unwrap();
        assert_eq!(merged, r#"{"metadata":{"a":null}}"#, "per-key null deletes that key");
    }

    #[test]
    fn metadata_accepts_string_number_and_bool() {
        let m: Metadata = serde_json::from_str(r#"{"s":"x","n":3,"b":true}"#).unwrap();
        assert!(matches!(m["s"], MetadataValue::String(_)));
        assert!(matches!(m["n"], MetadataValue::Number(_)));
        assert!(matches!(m["b"], MetadataValue::Bool(_)));
    }

    // ---- metadata numbers must survive the storage round-trip ----------------------------------
    // `[SPEC:.claude/contracts/amk-metadata-roundtrip.md]`. The table in
    // `MetadataValue::survives_storage_round_trip`'s own doc is the specification; these are it.

    /// The exact value schemathesis found. `f64::MAX`'s shortest form — below `f64::MAX`, and
    /// still unreadable once `jsonb` has expanded it.
    #[test]
    fn the_value_that_bricked_an_inbox_is_refused() {
        assert!(!MetadataValue::Number(1.7976931348623157e308).survives_storage_round_trip());
    }

    /// The case that proves the guard is not a blunt magnitude cap: `1e308` is LARGER in exponent
    /// than the refused value's leading digit suggests, yet it round-trips, because `jsonb` renders
    /// it as `1` followed by 308 zeros and serde_json parses that. A `v.abs() >= 1e308` check would
    /// wrongly refuse this one — which is why the guard reconstructs the rendering instead.
    #[test]
    fn one_e_308_is_accepted_even_though_a_smaller_value_is_not() {
        assert!(MetadataValue::Number(1e308).survives_storage_round_trip());
        assert!(!MetadataValue::Number(1.7976931348623157e308).survives_storage_round_trip());
    }

    #[test]
    fn ordinary_and_extreme_but_representable_numbers_are_accepted() {
        for v in [
            0.0,
            -0.0,
            1.0,
            -10_000_000.0,
            1e307,
            -1e307,
            2.0974644638236597e-254,
            f64::MIN_POSITIVE,
        ] {
            assert!(
                MetadataValue::Number(v).survives_storage_round_trip(),
                "{v:e} must be accepted"
            );
        }
    }

    /// The negative is asserted, not inferred from the positive — `f64::MIN` is `-f64::MAX`, and a
    /// sign-losing renderer would pass the positive case and corrupt this one.
    #[test]
    fn both_signs_of_the_out_of_range_value_are_refused() {
        assert!(!MetadataValue::Number(f64::MAX).survives_storage_round_trip());
        assert!(!MetadataValue::Number(f64::MIN).survives_storage_round_trip());
    }

    #[test]
    fn non_finite_numbers_are_refused_even_though_json_cannot_carry_them() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!MetadataValue::Number(v).survives_storage_round_trip());
        }
    }

    #[test]
    fn strings_and_bools_always_round_trip_including_a_string_that_looks_like_a_huge_number() {
        assert!(MetadataValue::Bool(true).survives_storage_round_trip());
        assert!(MetadataValue::String("x".into()).survives_storage_round_trip());
        assert!(
            MetadataValue::String("1.7976931348623157e308".into()).survives_storage_round_trip()
        );
    }

    /// Pins the renderer itself against what Postgres actually emitted for these inputs — verified
    /// with `select '{"a": ...}'::jsonb` on the dev cluster. Without this, a renderer bug could
    /// make the guard agree with itself while disagreeing with storage.
    #[test]
    fn the_renderer_reproduces_what_jsonb_emits() {
        assert_eq!(
            as_postgres_renders(1.7976931348623157e308),
            format!("17976931348623157{}", "0".repeat(292))
        );
        assert_eq!(as_postgres_renders(1e308), format!("1{}", "0".repeat(308)));
        assert_eq!(as_postgres_renders(-1e21), format!("-1{}", "0".repeat(21)));
        assert_eq!(as_postgres_renders(1.5e3), "1500");
        assert_eq!(as_postgres_renders(1e-7), "0.0000001");
        assert_eq!(as_postgres_renders(-1.25e-3), "-0.00125");
        // No exponent in the shortest form: passed through untouched.
        assert_eq!(as_postgres_renders(1.5), "1.5");
        assert_eq!(as_postgres_renders(-42.0), "-42");
    }
}
