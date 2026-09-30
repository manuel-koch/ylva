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
    backend_for, WebSearchBackend, WebSearchBackendKind, WEB_SEARCH_DEFAULT_RESULT_LIMIT,
    WEB_SEARCH_MAX_RESULT_LIMIT,
};

use super::ConfigError;

/// Internal HTTP timeout for one web-search request (seconds), used when
/// `[web_search] timeout_secs` is absent.
const DEFAULT_WEB_SEARCH_TIMEOUT_SECS: u64 = 30;

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
/// [`WEB_SEARCH_DEFAULT_RESULT_LIMIT`]), so an absent section behaves
/// exactly like the written-out form.
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
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        WebSearchConfig {
            backend: default_backend(),
            timeout_secs: default_timeout_secs(),
            result_limit: default_result_limit(),
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
        Ok(())
    }

    /// The internal HTTP timeout for one request. A configured `0` would make
    /// every request time out instantly, so it floors at one second.
    pub(crate) fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    /// Build the configured backend instance.
    pub(crate) fn build_backend(&self) -> Arc<dyn WebSearchBackend> {
        backend_for(&self.backend, self.http_timeout())
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
            message.contains("duckduckgo"),
            "names the accepted values: {message}"
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
        let backend = WebSearchConfig::default().build_backend();
        assert!(format!("{backend:?}").contains("DuckDuckGoBackend"));
    }
}
