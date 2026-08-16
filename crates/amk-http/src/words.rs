//! Generated defaults that need randomness: the inbox username shape
//! (`reference/fixtures/23-inbox-defaults-and-key-shape.txt`) and inbox-collision suggestions
//! (`reference/fixtures/05-error-catalog.http`). Both draw from `uuid::Uuid::new_v4()` — already a
//! workspace dependency with a CSPRNG-backed v4 generator — rather than adding `rand` for one
//! call site; the dispatch contract pins exactly two new dependencies (axum, tower).
//!
//! **The word lists are ours.** Fixture 23 is one sample (`cleananimal661`): it evidences the
//! *shape* (adjective + noun + 3 digits, lowercase, no separator), not the vocabulary.

use uuid::Uuid;

const ADJECTIVES: &[&str] = &[
    "clean", "quiet", "bright", "swift", "calm", "brave", "eager", "gentle", "happy", "lively",
    "mellow", "nimble", "proud", "quick", "sturdy", "tidy", "vivid", "warm", "wise", "zesty",
    "amber", "coral", "dusty", "misty",
];

const NOUNS: &[&str] = &[
    "animal", "canyon", "cedar", "delta", "ember", "falcon", "glacier", "harbor", "island",
    "jasper", "kettle", "lagoon", "meadow", "nectar", "orchid", "pebble", "quartz", "ridge",
    "summit", "thicket", "umbra", "valley", "willow", "zephyr",
];

/// adjective + noun + 3 digits, lowercase, no separator — the shape observed in fixture 23
/// (`cleananimal661`, 14 characters).
pub fn generate_username() -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    let adjective = ADJECTIVES[bytes[0] as usize % ADJECTIVES.len()];
    let noun = NOUNS[bytes[1] as usize % NOUNS.len()];
    let digits = (u32::from(bytes[2]) * 256 + u32::from(bytes[3])) % 1000;
    format!("{adjective}{noun}{digits:03}")
}

/// One candidate: `base` plus 4 decimal digits (leading zeros allowed — see the dispatch
/// contract's note that this is unevidenced either way), no separator — the shape observed in
/// fixture 05 (`amk-probe4991`, `amk-probe6813`, `amk-probe9732`).
pub fn suggestion_candidate(base: &str) -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    let digits = (u32::from(bytes[0]) * 256 + u32::from(bytes[1])) % 10000;
    format!("{base}{digits:04}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_username_matches_the_observed_shape() {
        for _ in 0..200 {
            let u = generate_username();
            assert!(u
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
            assert!(u.chars().rev().take(3).all(|c| c.is_ascii_digit()), "{u}");
            let letters: String = u.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            assert!(!letters.is_empty(), "{u}");
        }
    }

    #[test]
    fn two_successive_generations_are_not_forced_identical() {
        // Not a strict uniqueness guarantee (the space is small enough to collide by chance over
        // many draws), just that this is not a hardcoded constant.
        let samples: std::collections::HashSet<_> = (0..50).map(|_| generate_username()).collect();
        assert!(samples.len() > 1, "generator must not be a constant");
    }

    #[test]
    fn suggestion_candidate_matches_the_observed_shape() {
        for _ in 0..200 {
            let s = suggestion_candidate("amk-probe");
            assert!(s.starts_with("amk-probe"));
            let suffix = &s["amk-probe".len()..];
            assert_eq!(suffix.len(), 4, "{s}");
            assert!(suffix.chars().all(|c| c.is_ascii_digit()), "{s}");
        }
    }
}
