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

use glob::{MatchOptions, Pattern};
use serde::Deserialize;

/// Provider implementation kinds accepted in `provider = "..."`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Mock,
    Ollama,
    Openrouter,
    Edenai,
    Copilot,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Ollama => "ollama",
            Self::Openrouter => "openrouter",
            Self::Edenai => "edenai",
            Self::Copilot => "copilot",
        }
    }
}

/// A named provider connection.
///
/// The map key in `ProfileConfig::provider_connections` is the connection name shown to
/// users and referenced by an engine. `provider` is the implementation kind;
/// several named connections may use the same kind with different endpoints or
/// credentials.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProviderConnectionConfig {
    /// Provider implementation kind, such as `"ollama"` or `"openrouter"`.
    pub provider: ProviderKind,
    /// Provider base URL. Ollama uses the server origin and derives its
    /// OpenAI-compatible `/v1` and native `/api` paths; OpenRouter uses its
    /// origin and derives the OpenAI-compatible `/api/v1` path; EdenAI uses its
    /// origin and derives `/v3`. Copilot
    /// needs none: its API base is derived from the session token exchange.
    #[serde(default)]
    pub api_base_url: Option<String>,
    /// Optional credential for this connection.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Exact, case-sensitive model IDs the user marked as favorites for this
    /// connection, in picker order. Entries are never patterns.
    #[serde(default)]
    pub favorite_models: Vec<String>,
    /// Glob patterns restricting which model IDs may be selected or used
    /// through this connection. `None` (key omitted) means unrestricted; an
    /// explicit empty list is rejected when the profile loads.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
}

impl ProviderConnectionConfig {
    /// Compile the allowed-model patterns of this connection.
    pub fn model_policy(&self) -> Result<ModelPolicy, ModelPolicyError> {
        ModelPolicy::compile(self.allowed_models.as_deref())
    }

    /// Compile the allowed-model policy and verify that every configured
    /// favorite is permitted by it, without consulting any provider catalog.
    /// The error is a human-readable detail for the caller to attach context.
    pub fn validate_preferences(&self) -> Result<ModelPolicy, String> {
        let policy = self.model_policy().map_err(|error| error.to_string())?;
        for favorite in &self.favorite_models {
            if !policy.allows(favorite) {
                return Err(format!(
                    "favorite_models entry '{favorite}' is not permitted by allowed_models"
                ));
            }
        }
        Ok(policy)
    }
}

/// Why an `allowed_models` list cannot be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelPolicyError {
    #[error("allowed_models must not be empty; omit the key to allow every model")]
    EmptyList,
    #[error("allowed_models pattern '{pattern}' is malformed: {message}")]
    InvalidPattern { pattern: String, message: String },
}

/// Compiled allowed-model restriction of one provider connection.
///
/// Patterns are full-ID globs (`*`, `?`, character classes) matched with ASCII
/// case-insensitivity; `/` is ordinary ID content. A model is allowed when any
/// pattern matches. The policy never rewrites a model ID.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelPolicy {
    patterns: Option<Vec<Pattern>>,
}

const MODEL_MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

impl ModelPolicy {
    /// The policy of a connection without `allowed_models`.
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// Compile configured patterns; `None` is unrestricted.
    pub fn compile(patterns: Option<&[String]>) -> Result<Self, ModelPolicyError> {
        let Some(patterns) = patterns else {
            return Ok(Self::unrestricted());
        };
        if patterns.is_empty() {
            return Err(ModelPolicyError::EmptyList);
        }
        let compiled = patterns
            .iter()
            .map(|pattern| {
                Pattern::new(pattern).map_err(|error| ModelPolicyError::InvalidPattern {
                    pattern: pattern.clone(),
                    message: error.msg.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            patterns: Some(compiled),
        })
    }

    /// Whether `model_id` may be used.
    pub fn allows(&self, model_id: &str) -> bool {
        match &self.patterns {
            None => true,
            Some(patterns) => patterns
                .iter()
                .any(|pattern| pattern.matches_with(model_id, MODEL_MATCH_OPTIONS)),
        }
    }

    /// Reject `model_id` with an explanatory error when it is not allowed.
    pub fn ensure_allowed(&self, provider_connection: &str, model_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.allows(model_id),
            "model '{model_id}' is not permitted by allowed_models of provider connection \
             '{provider_connection}'"
        );
        Ok(())
    }
}

/// Compatibility alias for code that used the shorter provider name.
pub type ProviderConfig = ProviderConnectionConfig;

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(patterns: &[&str]) -> ModelPolicy {
        let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
        ModelPolicy::compile(Some(&patterns)).expect("patterns compile")
    }

    #[test]
    fn star_crosses_slashes_and_matches_the_full_id_only() {
        let policy = policy(&["openai/gpt-6*"]);
        assert!(policy.allows("openai/gpt-6"));
        assert!(policy.allows("openai/gpt-6/turbo:free"));
        assert!(!policy.allows("xopenai/gpt-6"), "no implicit prefix match");
        assert!(!policy.allows("openai/gpt-5"));
    }

    #[test]
    fn question_mark_and_character_classes_match_single_characters() {
        let policy = policy(&["m?del-[abc]"]);
        assert!(policy.allows("model-a"));
        assert!(policy.allows("MODEL-C"));
        assert!(!policy.allows("model-d"));
        assert!(!policy.allows("modeel-a"));
    }

    #[test]
    fn multiple_wildcards_and_inner_star_combine_in_one_pattern() {
        let policy = policy(&["openai/gpt-?-*[0-9]", "a/*-mini"]);
        assert!(policy.allows("openai/gpt-5-turbo-2"));
        assert!(policy.allows("OPENAI/GPT-4-7"));
        assert!(
            !policy.allows("openai/gpt-55-turbo-2"),
            "? is one character"
        );
        assert!(
            !policy.allows("openai/gpt-5-turbo-x"),
            "class must match last"
        );
        assert!(policy.allows("a/x/y-mini"));
        assert!(!policy.allows("a/x-mini-large"), "inner star is anchored");
    }

    #[test]
    fn matching_is_ascii_case_insensitive_only() {
        let policy = policy(&["straße"]);
        assert!(policy.allows("STRAßE"));
        assert!(!policy.allows("STRASSE"), "no Unicode case folding");
    }

    #[test]
    fn any_matching_pattern_allows_the_model() {
        let policy = policy(&["a/*", "b/*"]);
        assert!(policy.allows("b/x"));
        assert!(!policy.allows("c/x"));
    }

    #[test]
    fn leading_dots_need_no_literal_match() {
        assert!(policy(&["*"]).allows(".hidden/model"));
    }

    #[test]
    fn omitted_list_is_unrestricted_and_empty_or_malformed_lists_fail() {
        assert!(ModelPolicy::compile(None).expect("ok").allows("any"));
        assert_eq!(
            ModelPolicy::compile(Some(&[])),
            Err(ModelPolicyError::EmptyList)
        );
        assert!(matches!(
            ModelPolicy::compile(Some(&["[".to_string()])),
            Err(ModelPolicyError::InvalidPattern { .. })
        ));
    }

    #[test]
    fn ensure_allowed_names_connection_and_original_model_spelling() {
        let error = policy(&["a/*"])
            .ensure_allowed("cloud", "Other/Model")
            .expect_err("rejected")
            .to_string();
        assert!(
            error.contains("Other/Model") && error.contains("cloud"),
            "{error}"
        );
    }
}
