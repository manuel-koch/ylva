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

use super::super::console_app::display_registry_config;
use crate::agent::tools::default_registry;
use crate::config::ProfileConfig;
use crate::sandbox::Sandbox;

#[test]
fn display_registry_titles_the_backend_the_profile_configures() {
    let profile = ProfileConfig {
        web_search: crate::config::WebSearchConfig {
            backend: "searxng".to_string(),
            backends: crate::config::WebSearchBackendsConfig {
                searxng: Some(crate::config::SearXngBackendConfig {
                    base_url: Some("https://searx.example.org".to_string()),
                }),
                brave: None,
            },
            ..crate::config::WebSearchConfig::default()
        },
        ..ProfileConfig::default()
    };

    let registry = default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        display_registry_config(&profile),
    )
    .expect("valid sandbox fixture");
    let web_search = registry
        .get(crate::agent::tools::web_search::WebSearchTool::NAME)
        .expect("web_search is part of the CLI tool set");
    assert_eq!(
        web_search.title(),
        "Web Search (SearXNG)",
        "the rendered title must name the backend the profile selects"
    );
}

#[test]
fn display_registry_titles_brave_when_the_profile_configures_it() {
    let profile = ProfileConfig {
        web_search: crate::config::WebSearchConfig {
            backend: "brave".to_string(),
            backends: crate::config::WebSearchBackendsConfig {
                searxng: None,
                brave: Some(crate::config::BraveBackendConfig {
                    api_key: Some("test-subscription-token".to_string()),
                }),
            },
            ..crate::config::WebSearchConfig::default()
        },
        ..ProfileConfig::default()
    };

    let registry = default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        display_registry_config(&profile),
    )
    .expect("valid sandbox fixture");
    let web_search = registry
        .get(crate::agent::tools::web_search::WebSearchTool::NAME)
        .expect("web_search is part of the CLI tool set");
    assert_eq!(
        web_search.title(),
        "Web Search (Brave)",
        "the rendered title must name the backend the profile selects"
    );
}
