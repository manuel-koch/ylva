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

//! `[web_fetch]` profile section: which fetch backend serves the tool.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::tools::backends::web_fetch::{backend_for, WebFetchBackend, WebFetchBackendKind};

use super::ConfigError;

/// Default HTTP timeout for one web-fetch request, in seconds.
pub const DEFAULT_WEB_FETCH_TIMEOUT_SECS: u64 = 30;

fn default_backend() -> String {
    WebFetchBackendKind::Native.as_str().to_string()
}

/// A profile's `[web_fetch]` table.
///
/// The backend defaults to `"native"` when the section is absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WebFetchConfig {
    /// Fetch backend serving the `web_fetch` tool. Validated at config
    /// load; an unknown value is a [`ConfigError::InvalidWebFetchBackend`].
    #[serde(default = "default_backend")]
    pub backend: String,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        WebFetchConfig {
            backend: default_backend(),
        }
    }
}

impl WebFetchConfig {
    /// Validate the section's values at config load. `path` is the resolved
    /// `config.toml`, named in the error so a bad value is traceable to its
    /// source.
    pub(crate) fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        if WebFetchBackendKind::from_name(&self.backend).is_none() {
            return Err(ConfigError::InvalidWebFetchBackend {
                path: path.to_path_buf(),
                backend: self.backend.clone(),
                expected: WebFetchBackendKind::supported_names_display(),
            });
        }
        Ok(())
    }

    /// Build the configured backend instance.
    pub(crate) fn build_backend(&self, http_timeout: Duration) -> Arc<dyn WebFetchBackend> {
        backend_for(&self.backend, http_timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_section() {
        let config = WebFetchConfig::default();
        assert_eq!(config.backend, "native");
    }

    #[test]
    fn absent_section_deserializes_to_the_defaults() {
        let config: WebFetchConfig = toml::from_str("").expect("empty table parses");
        assert_eq!(config, WebFetchConfig::default());
    }

    #[test]
    fn explicit_values_deserialize() {
        let config: WebFetchConfig =
            toml::from_str("backend = \"native\"\n").expect("values parse");
        assert_eq!(config.backend, "native");
    }

    #[test]
    fn unknown_backend_is_rejected_naming_the_config_path() {
        let config = WebFetchConfig {
            backend: "camofox".to_string(),
        };
        let path = Path::new("/home/user/.ylva/profiles/default/config.toml");
        let error = config
            .validate(path)
            .expect_err("unknown backend is rejected");
        let message = error.to_string();
        assert!(message.contains("camofox"), "{message}");
        assert!(
            message.contains("native"),
            "names the accepted values: {message}"
        );
        assert!(
            message.contains("/home/user/.ylva/profiles/default/config.toml"),
            "names the config path: {message}"
        );
    }

    #[test]
    fn known_backend_validates() {
        WebFetchConfig::default()
            .validate(Path::new("config.toml"))
            .expect("the default backend is supported");
    }

    #[test]
    fn build_backend_returns_the_native_backend() {
        let backend = WebFetchConfig::default()
            .build_backend(Duration::from_secs(DEFAULT_WEB_FETCH_TIMEOUT_SECS));
        assert!(format!("{backend:?}").contains("HttpFetchBackend"));
    }
}
