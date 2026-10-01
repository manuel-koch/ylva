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
    WEB_SEARCH_DEFAULT_RESULT_LIMIT, WEB_SEARCH_MAX_RESULT_LIMIT,
};

use super::ConfigError;

/// Internal HTTP timeout for one web-search request (seconds), used when
/// `[web_search] timeout_secs` is absent.
const DEFAULT_WEB_SEARCH_TIMEOUT_SECS: u64 = 30;

/// Config key of the SearXNG instance root, named in validation errors.
const SEARXNG_BASE_URL_KEY: &str = "[web_search.backends.searxng] base_url";

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
        self.validate_searxng_backend(path)
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

    /// The internal HTTP timeout for one request. A configured `0` would make
    /// every request time out instantly, so it floors at one second.
    pub(crate) fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    /// Build the configured backend instance.
    ///
    /// An `Err` here is only reachable when `validate()` was skipped — profile
    /// load always validates first, so `default_registry` turns the error into
    /// an explicit invariant panic rather than falling back to another
    /// backend.
    pub(crate) fn build_backend(&self) -> Result<Arc<dyn WebSearchBackend>, WebSearchBackendError> {
        let searxng_base_url = self
            .backends
            .searxng
            .as_ref()
            .and_then(|table| table.base_url.as_deref());
        backend_for(&self.backend, searxng_base_url, self.http_timeout())
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
            message.contains("duckduckgo") && message.contains("searxng"),
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
                .is_err_and(|error| { error.to_string().contains("duckduckgo, searxng") }),
            "an unknown backend must not yield a different backend"
        );
    }
}
