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

//! `[web_search]` profile section: which search backend serves the
//! `web_search` tool and how it is paced.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::tools::backends::web_search::{
    backend_for, WebSearchBackend, WebSearchBackendError, WebSearchBackendKind,
    WebSearchBackendSettings, WEB_SEARCH_DEFAULT_RESULT_LIMIT, WEB_SEARCH_MAX_RESULT_LIMIT,
};

use super::ConfigError;

/// Internal HTTP timeout for one web-search request (seconds), used when
/// `[web_search] timeout_secs` is absent.
const DEFAULT_WEB_SEARCH_TIMEOUT_SECS: u64 = 30;

/// Config key of the SearXNG instance root, named in validation errors.
const SEARXNG_BASE_URL_KEY: &str = "[web_search.backends.searxng] base_url";

/// Config key of the Brave Search subscription token, named in validation
/// errors.
const BRAVE_API_KEY_KEY: &str = "[web_search.backends.brave] api_key";

fn default_backend() -> String {
    WebSearchBackendKind::DuckDuckGo.as_str().to_string()
}

fn default_timeout_secs() -> u64 {
    DEFAULT_WEB_SEARCH_TIMEOUT_SECS
}

fn default_result_limit() -> usize {
    WEB_SEARCH_DEFAULT_RESULT_LIMIT
}

/// A profile's `[web_search]` table.
///
/// Every field has an explicit default (`backend` = `"duckduckgo"`,
/// `timeout_secs` = [`DEFAULT_WEB_SEARCH_TIMEOUT_SECS`], `result_limit` =
/// [`WEB_SEARCH_DEFAULT_RESULT_LIMIT`], `backends` = no per-backend
/// settings), so an absent section behaves exactly like the written-out form.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WebSearchConfig {
    /// Search backend serving the `web_search` tool. Validated at config
    /// load; an unknown value is a [`ConfigError::InvalidWebSearchBackend`].
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Internal HTTP timeout for a single search request, in seconds. The
    /// tool's outer timeout is derived from it (internal +
    /// [`crate::agent::tools::web_search::WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS`]),
    /// so the backend's own timeout always fires first.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Number of results a `web_search` call returns when it omits `limit`.
    /// Capped at [`WEB_SEARCH_MAX_RESULT_LIMIT`] so config can never
    /// disagree with the tool schema.
    #[serde(default = "default_result_limit")]
    pub result_limit: usize,
    /// Per-backend settings under `[web_search.backends.<name>]`. A table for
    /// a backend that is *not* selected is still validated, but only the
    /// selected backend's table is required to be present.
    #[serde(default)]
    pub backends: WebSearchBackendsConfig,
}

/// The `[web_search.backends]` table: nested per-backend settings, one
/// sub-table per selectable [`WebSearchBackendKind`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WebSearchBackendsConfig {
    /// `[web_search.backends.searxng]`; present only when the profile
    /// configures a SearXNG instance.
    #[serde(default)]
    pub searxng: Option<SearXngBackendConfig>,
    /// `[web_search.backends.brave]`; present only when the profile
    /// configures a Brave Search subscription token.
    #[serde(default)]
    pub brave: Option<BraveBackendConfig>,
}

/// The `[web_search.backends.searxng]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SearXngBackendConfig {
    /// Root URL of the SearXNG instance (e.g. `https://searx.example.org`).
    /// Required — and non-empty — when `backend = "searxng"`; a value that is
    /// present must always be a parsable `http(s)` URL.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// The `[web_search.backends.brave]` table.
///
/// Holds the Brave Search API subscription token — a secret, so the intended
/// spelling is a `${ENV_VAR}` interpolation and [`Debug`] masks the value.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct BraveBackendConfig {
    /// Brave Search API subscription token. Required — and non-empty — when
    /// `backend = "brave"`; never read from the process environment
    /// implicitly.
    #[serde(default)]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for BraveBackendConfig {
    /// Report whether a key is configured without ever formatting it — a
    /// `Debug` output (e.g. a log line or an error message) must not leak the
    /// subscription token.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BraveBackendConfig")
            .field(
                "api_key",
                &self.api_key.as_ref().map(|_| "<REDACTED>".to_string()),
            )
            .finish()
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        WebSearchConfig {
            backend: default_backend(),
            timeout_secs: default_timeout_secs(),
            result_limit: default_result_limit(),
            backends: WebSearchBackendsConfig::default(),
        }
    }
}

impl WebSearchConfig {
    /// Validate the section's values at config load. `path` is the resolved
    /// `config.toml`, named in the error so a bad value is traceable to its
    /// source.
    pub(crate) fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        if WebSearchBackendKind::from_name(&self.backend).is_none() {
            return Err(ConfigError::InvalidWebSearchBackend {
                path: path.to_path_buf(),
                backend: self.backend.clone(),
                expected: WebSearchBackendKind::supported_names_display(),
            });
        }
        self.validate_searxng_backend(path)?;
        self.validate_brave_backend(path)
    }

    /// Validate the `[web_search.backends.searxng]` table against the
    /// selected backend.
    ///
    /// The *selected* backend must have a usable `base_url`; a table that is
    /// present but unselected is only checked for a parsable value (an empty
    /// string is tolerated there, because the setting is unused).
    fn validate_searxng_backend(&self, path: &Path) -> Result<(), ConfigError> {
        let selected_searxng = self.backend == WebSearchBackendKind::SearXng.as_str();
        let Some(table) = self.backends.searxng.as_ref() else {
            return if selected_searxng {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::SearXng.as_str().to_string(),
                    key: "[web_search.backends.searxng]".to_string(),
                    reason: "the table is required when backend = \"searxng\" selects it"
                        .to_string(),
                })
            } else {
                Ok(())
            };
        };
        let Some(base_url) = table.base_url.as_deref() else {
            return if selected_searxng {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::SearXng.as_str().to_string(),
                    key: SEARXNG_BASE_URL_KEY.to_string(),
                    reason: "must name the SearXNG instance".to_string(),
                })
            } else {
                Ok(())
            };
        };
        if base_url.trim().is_empty() {
            return if selected_searxng {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::SearXng.as_str().to_string(),
                    key: SEARXNG_BASE_URL_KEY.to_string(),
                    reason: "must be a non-empty URL".to_string(),
                })
            } else {
                Ok(())
            };
        }
        match reqwest::Url::parse(base_url.trim()) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => Ok(()),
            Ok(url) => Err(ConfigError::InvalidWebSearchBackendSetting {
                path: path.to_path_buf(),
                backend: WebSearchBackendKind::SearXng.as_str().to_string(),
                key: SEARXNG_BASE_URL_KEY.to_string(),
                reason: format!("must use the http or https scheme, got '{}'", url.scheme()),
            }),
            Err(error) => Err(ConfigError::InvalidWebSearchBackendSetting {
                path: path.to_path_buf(),
                backend: WebSearchBackendKind::SearXng.as_str().to_string(),
                key: SEARXNG_BASE_URL_KEY.to_string(),
                reason: format!("must be a parsable URL: {error}"),
            }),
        }
    }

    /// Validate the `[web_search.backends.brave]` table against the
    /// selected backend.
    ///
    /// The *selected* backend must have a usable `api_key`; a table that is
    /// present but unselected is not inspected at all — an empty string is
    /// tolerated there, because the setting is unused. Unlike SearXNG there
    /// is no URL to parse: Brave's endpoint is fixed.
    fn validate_brave_backend(&self, path: &Path) -> Result<(), ConfigError> {
        let selected_brave = self.backend == WebSearchBackendKind::Brave.as_str();
        let Some(table) = self.backends.brave.as_ref() else {
            return if selected_brave {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::Brave.as_str().to_string(),
                    key: "[web_search.backends.brave]".to_string(),
                    reason: "the table is required when backend = \"brave\" selects it".to_string(),
                })
            } else {
                Ok(())
            };
        };
        let Some(api_key) = table.api_key.as_deref() else {
            return if selected_brave {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::Brave.as_str().to_string(),
                    key: BRAVE_API_KEY_KEY.to_string(),
                    reason: "must hold the Brave Search API subscription token".to_string(),
                })
            } else {
                Ok(())
            };
        };
        if api_key.trim().is_empty() {
            return if selected_brave {
                Err(ConfigError::InvalidWebSearchBackendSetting {
                    path: path.to_path_buf(),
                    backend: WebSearchBackendKind::Brave.as_str().to_string(),
                    key: BRAVE_API_KEY_KEY.to_string(),
                    reason: "must be a non-empty API key".to_string(),
                })
            } else {
                Ok(())
            };
        }
        Ok(())
    }

    /// The internal HTTP timeout for one request. A configured `0` would make
    /// every request time out instantly, so it floors at one second.
    pub(crate) fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    /// Build the configured backend instance.
    ///
    /// The backend-specific config tables are mapped onto their own
    /// [`WebSearchBackendSettings`] variant, so the factory never receives a
    /// setting belonging to a different backend. An `Err` here is only
    /// reachable when `validate()` was skipped — profile load always
    /// validates first, so `default_registry` turns the error into an
    /// explicit invariant panic rather than falling back to another backend.
    pub(crate) fn build_backend(&self) -> Result<Arc<dyn WebSearchBackend>, WebSearchBackendError> {
        let settings = match WebSearchBackendKind::from_name(&self.backend) {
            Some(WebSearchBackendKind::DuckDuckGo) => WebSearchBackendSettings::DuckDuckGo,
            Some(WebSearchBackendKind::SearXng) => WebSearchBackendSettings::SearXng {
                base_url: self
                    .backends
                    .searxng
                    .as_ref()
                    .and_then(|table| table.base_url.as_deref()),
            },
            Some(WebSearchBackendKind::Brave) => WebSearchBackendSettings::Brave {
                api_key: self
                    .backends
                    .brave
                    .as_ref()
                    .and_then(|table| table.api_key.as_deref()),
            },
            None => WebSearchBackendSettings::Unsupported {
                backend_name: &self.backend,
            },
        };
        backend_for(settings, self.http_timeout())
    }

    /// The default result count for calls omitting `limit`, capped at the
    /// tool schema's maximum so config can never disagree with the schema.
    pub(crate) fn effective_result_limit(&self) -> usize {
        self.result_limit.clamp(1, WEB_SEARCH_MAX_RESULT_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_section() {
        let config = WebSearchConfig::default();
        assert_eq!(config.backend, "duckduckgo");
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.result_limit, WEB_SEARCH_DEFAULT_RESULT_LIMIT);
        assert_eq!(config.backends.searxng, None);
        assert_eq!(config.backends.brave, None);
    }

    #[test]
    fn absent_section_deserializes_to_the_defaults() {
        let config: WebSearchConfig = toml::from_str("").expect("empty table parses");
        assert_eq!(config, WebSearchConfig::default());
    }

    #[test]
    fn explicit_values_deserialize() {
        let config: WebSearchConfig =
            toml::from_str("backend = \"duckduckgo\"\ntimeout_secs = 5\nresult_limit = 3\n")
                .expect("values parse");
        assert_eq!(config.backend, "duckduckgo");
        assert_eq!(config.timeout_secs, 5);
        assert_eq!(config.result_limit, 3);
    }

    #[test]
    fn nested_searxng_table_deserializes() {
        let config: WebSearchConfig = toml::from_str(
            "backend = \"searxng\"\n\n[backends.searxng]\nbase_url = \"https://searx.example.org\"\n",
        )
        .expect("the nested table parses");
        assert_eq!(config.backend, "searxng");
        assert_eq!(
            config.backends.searxng,
            Some(SearXngBackendConfig {
                base_url: Some("https://searx.example.org".to_string())
            })
        );
    }

    #[test]
    fn nested_brave_table_deserializes() {
        let config: WebSearchConfig = toml::from_str(
            "backend = \"brave\"\n\n[backends.brave]\napi_key = \"sentinel-brave-token\"\n",
        )
        .expect("the nested table parses");
        assert_eq!(config.backend, "brave");
        assert_eq!(
            config.backends.brave,
            Some(BraveBackendConfig {
                api_key: Some("sentinel-brave-token".to_string())
            })
        );
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("sentinel-brave-token"),
            "the token must never reach a Debug output: {rendered}"
        );
        assert!(
            rendered.contains("<REDACTED>"),
            "the masked key stays visible as redacted: {rendered}"
        );
    }

    #[test]
    fn unknown_backend_is_rejected_naming_the_config_path() {
        let config = WebSearchConfig {
            backend: "startpage".to_string(),
            ..WebSearchConfig::default()
        };
        let path = Path::new("/home/user/.ylva/profiles/default/config.toml");
        let error = config
            .validate(path)
            .expect_err("unknown backend is rejected");
        let message = error.to_string();
        assert!(message.contains("startpage"), "{message}");
        assert!(
            message.contains("duckduckgo")
                && message.contains("searxng")
                && message.contains("brave"),
            "names every accepted value: {message}"
        );
        assert!(
            message.contains("/home/user/.ylva/profiles/default/config.toml"),
            "names the config path: {message}"
        );
    }

    #[test]
    fn known_backend_validates() {
        WebSearchConfig::default()
            .validate(Path::new("config.toml"))
            .expect("the default backend is supported");
        let brave = WebSearchConfig {
            backend: "brave".to_string(),
            backends: WebSearchBackendsConfig {
                searxng: None,
                brave: Some(BraveBackendConfig {
                    api_key: Some("test-subscription-token".to_string()),
                }),
            },
            ..WebSearchConfig::default()
        };
        brave
            .validate(Path::new("config.toml"))
            .expect("a brave config with its token is supported");
    }

    #[test]
    fn searxng_without_the_table_is_rejected_naming_path_and_key() {
        let config = WebSearchConfig {
            backend: "searxng".to_string(),
            ..WebSearchConfig::default()
        };
        let path = Path::new("/home/user/.ylva/profiles/default/config.toml");
        let error = config
            .validate(path)
            .expect_err("searxng needs its instance table");
        let message = error.to_string();
        assert!(
            matches!(
                &error,
                ConfigError::InvalidWebSearchBackendSetting { backend, key, .. }
                    if backend == "searxng" && key.contains("backends.searxng")
            ),
            "{message}"
        );
        assert!(
            message.contains("/home/user/.ylva/profiles/default/config.toml"),
            "names the config path: {message}"
        );
    }

    #[test]
    fn searxng_without_base_url_is_rejected_naming_path_and_key() {
        let config: WebSearchConfig =
            toml::from_str("backend = \"searxng\"\n\n[backends.searxng]\n")
                .expect("the table parses");
        let path = Path::new("config.toml");
        let error = config
            .validate(path)
            .expect_err("searxng needs its instance URL");
        let message = error.to_string();
        assert!(
            matches!(
                &error,
                ConfigError::InvalidWebSearchBackendSetting { key, .. }
                    if key.contains("base_url")
            ),
            "{message}"
        );
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("searxng"), "{message}");
    }

    #[test]
    fn searxng_with_an_unusable_base_url_is_rejected_naming_the_reason() {
        let cases = [
            (
                "[backends.searxng]\nbase_url = \"\"\n",
                "non-empty",
                "an empty URL names no instance",
            ),
            (
                "[backends.searxng]\nbase_url = \"not a url\"\n",
                "parsable URL",
                "an unparsable URL is rejected",
            ),
            (
                "[backends.searxng]\nbase_url = \"ftp://searx.example.org\"\n",
                "http or https scheme",
                "a non-http scheme is rejected",
            ),
        ];
        for (table, reason_fragment, case) in cases {
            let config: WebSearchConfig =
                toml::from_str(&format!("backend = \"searxng\"\n\n{table}"))
                    .expect("the table parses");
            let error = match config.validate(Path::new("config.toml")) {
                Ok(()) => panic!("{case} must be rejected"),
                Err(error) => error,
            };
            let message = error.to_string();
            assert!(
                matches!(
                    &error,
                    ConfigError::InvalidWebSearchBackendSetting { key, .. }
                        if key.contains("base_url")
                ),
                "{case}: {message}"
            );
            assert!(
                message.contains(reason_fragment),
                "{case}: the reason is named: {message}"
            );
        }
    }

    #[test]
    fn unselected_searxng_table_is_still_validated() {
        let config: WebSearchConfig =
            toml::from_str("\n[backends.searxng]\nbase_url = \"not a url\"\n")
                .expect("the table parses");
        let error = config
            .validate(Path::new("config.toml"))
            .expect_err("a present but unselected table must still be parsable");
        assert!(error.to_string().contains("parsable URL"), "{error}");
    }

    #[test]
    fn unselected_searxng_table_with_a_valid_url_validates() {
        let config: WebSearchConfig = toml::from_str(
            "backend = \"duckduckgo\"\n\n[backends.searxng]\nbase_url = \"https://searx.example.org\"\n",
        )
        .expect("the table parses");
        config
            .validate(Path::new("config.toml"))
            .expect("a valid unselected table is accepted");
        assert_eq!(config.backend, "duckduckgo", "duckduckgo is untouched");
    }

    #[test]
    fn duckduckgo_ignores_an_empty_unselected_searxng_base_url() {
        let config: WebSearchConfig =
            toml::from_str("\n[backends.searxng]\nbase_url = \"\"\n").expect("the table parses");
        config
            .validate(Path::new("config.toml"))
            .expect("an unused empty value is tolerated");
    }

    #[test]
    fn brave_without_the_table_is_rejected_naming_path_backend_and_key() {
        let config = WebSearchConfig {
            backend: "brave".to_string(),
            ..WebSearchConfig::default()
        };
        let path = Path::new("/home/user/.ylva/profiles/default/config.toml");
        let error = config
            .validate(path)
            .expect_err("brave needs its api_key table");
        let message = error.to_string();
        assert!(
            matches!(
                &error,
                ConfigError::InvalidWebSearchBackendSetting { backend, key, .. }
                    if backend == "brave" && key == "[web_search.backends.brave]"
            ),
            "{message}"
        );
        assert!(
            message.contains("/home/user/.ylva/profiles/default/config.toml"),
            "names the config path: {message}"
        );
        assert!(message.contains("brave"), "names the backend: {message}");
        assert!(message.contains("table is required"), "{message}");
    }

    #[test]
    fn brave_without_api_key_is_rejected_naming_path_backend_and_key() {
        let config: WebSearchConfig =
            toml::from_str("backend = \"brave\"\n\n[backends.brave]\n").expect("the table parses");
        let path = Path::new("/home/user/.ylva/profiles/default/config.toml");
        let error = config
            .validate(path)
            .expect_err("brave needs its subscription token");
        let message = error.to_string();
        assert!(
            matches!(
                &error,
                ConfigError::InvalidWebSearchBackendSetting { backend, key, .. }
                    if backend == "brave" && key.contains("backends.brave") && key.contains("api_key")
            ),
            "{message}"
        );
        assert!(message.contains("subscription token"), "{message}");
        assert!(
            message.contains("/home/user/.ylva/profiles/default/config.toml"),
            "names the config path: {message}"
        );
    }

    #[test]
    fn brave_with_a_blank_api_key_is_rejected_naming_the_reason() {
        let cases = [
            ("[backends.brave]\napi_key = \"\"\n", "an empty key"),
            (
                "[backends.brave]\napi_key = \"   \"\n",
                "a whitespace-only key",
            ),
        ];
        for (table, case) in cases {
            let config: WebSearchConfig =
                toml::from_str(&format!("backend = \"brave\"\n\n{table}"))
                    .expect("the table parses");
            let error = config
                .validate(Path::new("config.toml"))
                .expect_err("a blank key must fail");
            let message = error.to_string();
            assert!(
                matches!(
                    &error,
                    ConfigError::InvalidWebSearchBackendSetting { key, reason, .. }
                        if key.contains("api_key") && reason == "must be a non-empty API key"
                ),
                "{case}: {message}"
            );
            assert!(message.contains("config.toml"), "{case}: {message}");
        }
    }

    #[test]
    fn unselected_brave_table_with_an_empty_api_key_validates() {
        let config: WebSearchConfig =
            toml::from_str("\n[backends.brave]\napi_key = \"\"\n").expect("the table parses");
        config
            .validate(Path::new("config.toml"))
            .expect("an unused empty value is tolerated");
        assert_eq!(config.backend, "duckduckgo", "the default stays selected");
    }

    #[test]
    fn duckduckgo_defaults_are_untouched_by_a_present_brave_table() {
        let config: WebSearchConfig = toml::from_str(
            "backend = \"duckduckgo\"\n\n[backends.brave]\napi_key = \"test-subscription-token\"\n",
        )
        .expect("the table parses");
        config
            .validate(Path::new("config.toml"))
            .expect("an unselected brave table never breaks the default");
        assert_eq!(config.backend, "duckduckgo");
        let backend = config.build_backend().expect("the default still builds");
        assert!(format!("{backend:?}").contains("DuckDuckGoBackend"));
    }

    #[test]
    fn zero_timeout_floors_to_one_second() {
        let config = WebSearchConfig {
            timeout_secs: 0,
            ..WebSearchConfig::default()
        };
        assert_eq!(config.http_timeout(), Duration::from_secs(1));
    }

    #[test]
    fn result_limit_is_capped_at_the_schema_maximum() {
        let config = WebSearchConfig {
            result_limit: 500,
            ..WebSearchConfig::default()
        };
        assert_eq!(config.effective_result_limit(), WEB_SEARCH_MAX_RESULT_LIMIT);
        let zero = WebSearchConfig {
            result_limit: 0,
            ..WebSearchConfig::default()
        };
        assert_eq!(zero.effective_result_limit(), 1);
    }

    #[test]
    fn build_backend_returns_the_configured_backend() {
        let backend = WebSearchConfig::default()
            .build_backend()
            .expect("the default config always builds");
        assert!(format!("{backend:?}").contains("DuckDuckGoBackend"));

        let searxng: WebSearchConfig = toml::from_str(
            "backend = \"searxng\"\n\n[backends.searxng]\nbase_url = \"https://searx.example.org/\"\n",
        )
        .expect("the table parses");
        let backend = searxng.build_backend().expect("a valid config builds");
        assert!(
            format!("{backend:?}").contains("SearXngBackend"),
            "the configured backend is built, not substituted: {backend:?}"
        );

        let sentinel_token = "sentinel-brave-token";
        let brave: WebSearchConfig = toml::from_str(&format!(
            "backend = \"brave\"\n\n[backends.brave]\napi_key = \"{sentinel_token}\"\n"
        ))
        .expect("the table parses");
        let backend = brave.build_backend().expect("a valid config builds");
        let rendered_backend = format!("{backend:?}");
        assert!(
            rendered_backend.contains("BraveBackend"),
            "the configured backend is built, not substituted: {rendered_backend}"
        );
        assert!(
            !rendered_backend.contains(sentinel_token),
            "the API key must never reach a Debug output: {rendered_backend}"
        );
        let rendered_config = format!("{brave:?}");
        assert!(
            !rendered_config.contains(sentinel_token),
            "the containing config must not leak the token either: {rendered_config}"
        );
    }

    #[test]
    fn build_backend_never_substitutes_a_backend_for_an_invalid_state() {
        // Hand-built states that `validate()` would reject at profile load.
        let missing_table = WebSearchConfig {
            backend: "searxng".to_string(),
            ..WebSearchConfig::default()
        };
        assert!(
            missing_table
                .build_backend()
                .is_err_and(|error| error.to_string().contains("base_url")),
            "searxng without base_url must not yield a different backend"
        );

        let missing_url = WebSearchConfig {
            backend: "searxng".to_string(),
            backends: WebSearchBackendsConfig {
                searxng: Some(SearXngBackendConfig { base_url: None }),
                brave: None,
            },
            ..WebSearchConfig::default()
        };
        assert!(
            missing_url.build_backend().is_err(),
            "an empty base_url must not yield a different backend"
        );

        let unsupported = WebSearchConfig {
            backend: "startpage".to_string(),
            ..WebSearchConfig::default()
        };
        assert!(
            unsupported
                .build_backend()
                .is_err_and(|error| { error.to_string().contains("duckduckgo, searxng, brave") }),
            "an unknown backend must not yield a different backend"
        );

        let brave_without_key = WebSearchConfig {
            backend: "brave".to_string(),
            ..WebSearchConfig::default()
        };
        assert!(
            brave_without_key.build_backend().is_err_and(|error| error
                .to_string()
                .contains("api_key")
                && error.to_string().contains("brave")
                && !error.to_string().contains("DuckDuckGo")),
            "brave without api_key must not yield a different backend"
        );
    }
}
