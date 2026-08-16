//! Path-segment id handling for the string-typed ids (`inbox_id`, `message_id`, `api_key_id`).
//!
//! `pod_id` is not here: it is a UUID and is extracted with axum's own `Path<Uuid>` (the
//! dependency section of the dispatch contract names this explicitly — `uuid`'s `serde` feature
//! exists for exactly this).
//!
//! # Why this does *not* call `amk_types::ids::*::from_path_segment`
//!
//! The dispatch contract says "decode with `from_path_segment`; never hand-roll it", written on
//! the assumption that a handler receives the *raw*, still-percent-encoded request path. axum 0.8
//! does not hand a handler that: both `Path<String>` and `RawPathParams` (checked against the
//! vendored axum 0.8.9 source, `src/extract/path/mod.rs`) percent-decode a path segment exactly
//! once before a handler ever sees it, and there is no public axum extractor that hands back the
//! undecoded segment.
//!
//! `from_path_segment`'s own doc says as much: *"Handlers normally receive an already-decoded
//! parameter (axum percent-decodes path params), so this is not the request path."* Calling it
//! again on axum's already-decoded value would percent-decode a **second** time — harmless for an
//! id with no literal `%` in it, and silent corruption for one that has: `user%2540@x` (a literal
//! `%40` in the local part, itself percent-encoded for the URL) single-decodes, correctly, to
//! `user%40@x`; a second decode turns that into `user@x`, merging it with a different address.
//! This is exactly the "literal encoded `%2F`; double-encoding" case the dispatch contract's own
//! edge-case list names.
//!
//! So: [`decode_segment`] takes axum's already-decoded `&str` and applies only the half of
//! `from_path_segment` that a single axum decode does not already cover — rejecting a NUL byte,
//! `amk_types::ids::has_forbidden_byte`'s rule, which `%00` survives percent-decoding as valid
//! UTF-8 and therefore is not caught by axum's own UTF-8 validation. This divergence from the
//! contract's literal wording is flagged in the dispatch report; the resolution is evidenced by
//! axum's own source and `amk_types`' own doc comment, not a guess.

use amk_types::ids::has_forbidden_byte;

/// A decoded path segment carried a byte no identifier may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("path segment contains a forbidden byte")]
pub struct ForbiddenByte;

/// Validate an axum-decoded path segment for use as a string-typed id (`inbox_id`, `message_id`,
/// `api_key_id`). Returns the segment unchanged — axum has already done the one percent-decode
/// this id gets — after rejecting a NUL byte.
pub fn decode_segment(raw: &str) -> Result<&str, ForbiddenByte> {
    if has_forbidden_byte(raw) {
        Err(ForbiddenByte)
    } else {
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amk_types::ids::MessageId;
    use percent_encoding::percent_decode_str;

    /// Simulates exactly what axum's `Path<String>` does to a route segment: one percent-decode.
    fn axum_style_decode(segment: &str) -> String {
        percent_decode_str(segment)
            .decode_utf8()
            .expect("test input is valid UTF-8 once decoded")
            .into_owned()
    }

    /// No route in *this* dispatch carries `message_id` (no messages endpoints are in the 25 —
    /// see the dispatch contract's scope table), but the decoding machinery is shared with
    /// `inbox_id`/`api_key_id` and will be reused verbatim the day a messages route lands. Proven
    /// now so that dispatch does not have to re-derive it.
    #[test]
    fn message_id_round_trips_through_a_single_axum_style_decode() {
        let observed = MessageId::new(
            "<010001a003ef6970-1732f5b7-5c17-485b-92fa-52ccd78a0004-000000@email.amazonses.com>",
        );
        let encoded = observed.to_path_segment();
        let decoded_once = axum_style_decode(&encoded);
        assert_eq!(decoded_once, observed.as_str());
        let checked = decode_segment(&decoded_once).unwrap();
        assert_eq!(MessageId::new(checked), observed);
    }

    #[test]
    fn round_trips_plus_percent_slash_question_hash_space_and_non_ascii() {
        for raw in [
            "user+tag@example.com",
            "100%done@example.com",
            "a/b@example.com",
            "a?b@example.com",
            "a#b@example.com",
            "a b@example.com",
            "üser@example.com",
        ] {
            let id = MessageId::new(raw);
            let encoded = id.to_path_segment();
            let decoded_once = axum_style_decode(&encoded);
            assert_eq!(decoded_once, raw, "single decode must recover the original for {raw:?}");
        }
    }

    /// The exact corruption a double-decode would cause: a literal `%2F` inside the id, percent-
    /// encoded once more (`%252F`) to survive the URL. A single decode (what axum performs) must
    /// yield `%2F` literally; decoding again would turn it into `/`.
    #[test]
    fn a_literal_percent_2f_survives_exactly_one_decode() {
        let raw = "weird%2Flocal@example.com";
        let id = MessageId::new(raw);
        let encoded = id.to_path_segment();
        // The literal `%` in the id must itself have been percent-encoded (`%25`) so that a
        // single decode recovers it, not a bare `%` that a decoder would misread as the start of
        // an escape sequence.
        assert!(
            encoded.contains("%25"),
            "the literal % must be escaped in the wire form: {encoded}"
        );
        let decoded_once = axum_style_decode(&encoded);
        assert_eq!(decoded_once, raw, "one decode must recover the literal %2F, not a slash");
    }

    #[test]
    fn double_encoding_the_whole_segment_does_not_round_trip_through_a_single_decode() {
        // A caller that (incorrectly) double-encodes the whole segment before sending it must not
        // silently resolve to the right id after axum's one decode — the wrong id (or garbage) is
        // the correct, honest outcome, not a silent "fix" that masks the caller's bug.
        let raw = "double@example.com";
        let once = MessageId::new(raw).to_path_segment();
        let twice =
            percent_encoding::utf8_percent_encode(&once, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let decoded_once = axum_style_decode(&twice);
        assert_ne!(decoded_once, raw, "a double-encoded segment must not resolve after one decode");
        assert_eq!(
            decoded_once, once,
            "one decode of a double-encoded segment undoes only one layer"
        );
    }

    #[test]
    fn an_over_long_segment_does_not_panic() {
        let raw = format!("{}@example.com", "a".repeat(8192));
        let encoded = MessageId::new(&raw).to_path_segment();
        let decoded_once = axum_style_decode(&encoded);
        assert_eq!(decoded_once, raw);
        assert!(decode_segment(&decoded_once).is_ok());
    }

    #[test]
    fn a_nul_byte_is_rejected_after_the_single_decode() {
        // %00 percent-decodes to a valid UTF-8 NUL character, so axum's own UTF-8 check does not
        // catch it — has_forbidden_byte is the one thing this module still has to do itself.
        let decoded_once = axum_style_decode("abc%00def");
        assert_eq!(decode_segment(&decoded_once), Err(ForbiddenByte));
    }

    #[test]
    fn an_ordinary_segment_is_accepted_unchanged() {
        assert_eq!(decode_segment("amk-probe@agentmail.to"), Ok("amk-probe@agentmail.to"));
    }
}
