// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Approximate token estimation for context budgeting (blueprint §4C).
//!
//! Counts are cached once when a message enters the context and are
//! **advisory**: they drive compaction thresholds, they are never billed
//! usage and never shown as such. `cl100k_base` is the common local estimate
//! for every model, including the metadata-only per-request calibration
//! record. It is not asserted to be the serving model's tokenizer; provider
//! usage observations record that distinction.

/// Which approximation the context uses when caching per-message token counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenEstimator {
    /// `tiktoken-rs` byte-pair encoding (`cl100k_base`).
    #[default]
    Tiktoken,
    /// Four characters ≈ one token. Retained for deterministic tests and
    /// callers that explicitly choose this fallback.
    FourCharactersPerToken,
}

impl TokenEstimator {
    /// Approximate token count of `text`.
    pub fn estimate_tokens(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        match self {
            Self::Tiktoken => tiktoken_rs::cl100k_base_singleton()
                .lock()
                .encode_ordinary(text)
                .len(),
            Self::FourCharactersPerToken => text.chars().count().div_ceil(4),
        }
    }

    /// Keep the longest UTF-8 prefix whose estimate fits `max_tokens`.
    pub fn truncate(&self, text: &str, max_tokens: usize) -> (String, bool) {
        truncate_text_to_token_budget(*self, text, max_tokens)
    }
}

/// Keep the longest UTF-8 prefix of `text` whose local estimate does not
/// exceed `max_tokens`. The returned boolean says whether content was cut.
pub fn truncate_text_to_token_budget(
    estimator: TokenEstimator,
    text: &str,
    max_tokens: usize,
) -> (String, bool) {
    if estimator.estimate_tokens(text) <= max_tokens {
        return (text.to_string(), false);
    }
    if max_tokens == 0 || text.is_empty() {
        return (String::new(), !text.is_empty());
    }

    let mut boundaries: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    boundaries.push(text.len());
    let mut low = 0usize;
    let mut high = boundaries.len() - 1;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let boundary = boundaries[middle];
        if estimator.estimate_tokens(&text[..boundary]) <= max_tokens {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    (text[..boundaries[low]].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_character_heuristic_counts_four_characters_per_token_rounded_up() {
        let estimator = TokenEstimator::FourCharactersPerToken;

        assert_eq!(estimator.estimate_tokens(""), 0);
        assert_eq!(estimator.estimate_tokens("abc"), 1);
        assert_eq!(estimator.estimate_tokens("abcd"), 1);
        assert_eq!(estimator.estimate_tokens("abcde"), 2);
        assert_eq!(estimator.estimate_tokens("hello world"), 3);
        assert_eq!(
            estimator.estimate_tokens("héllo wörld"),
            3,
            "the heuristic counts characters, not bytes"
        );
    }

    #[test]
    fn tiktoken_counts_more_than_zero_for_real_text_and_grows_with_length() {
        let estimator = TokenEstimator::Tiktoken;

        assert_eq!(estimator.estimate_tokens(""), 0);
        let short = estimator.estimate_tokens("hello");
        let long = estimator.estimate_tokens(
            "hello world, this sentence is noticeably longer than the previous one",
        );
        assert!(short > 0, "a non-empty string is at least one token");
        assert!(
            long > short,
            "a longer string costs more tokens: short={short} long={long}"
        );
    }

    #[test]
    fn both_estimators_are_stable_for_the_same_input() {
        let text = "stable estimate across repeated calls";
        for estimator in [
            TokenEstimator::Tiktoken,
            TokenEstimator::FourCharactersPerToken,
        ] {
            assert_eq!(
                estimator.estimate_tokens(text),
                estimator.estimate_tokens(text),
                "cached counts must be reproducible: {estimator:?}"
            );
        }
    }

    #[test]
    fn token_budget_truncation_preserves_utf8_and_respects_the_limit() {
        let text = "αβγδεζηθικλμνξοπρστυφχψω";
        let estimator = TokenEstimator::Tiktoken;
        let budget = 3;

        let (truncated, was_truncated) = truncate_text_to_token_budget(estimator, text, budget);

        assert!(was_truncated);
        assert!(text.starts_with(&truncated));
        assert!(estimator.estimate_tokens(&truncated) <= budget);
    }
}
