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

use super::*;

#[test]
fn web_fetch_section_loads_explicit_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "explicit-web-fetch",
        r#"
[web_fetch]
backend = "native"
timeout_secs = 5
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "explicit-web-fetch").expect("the fixture loads");
    assert_eq!(profile.web_fetch.backend, "native");
    assert_eq!(profile.web_fetch.timeout_secs, 5);
}

#[test]
fn absent_web_fetch_section_defaults_to_native() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(temp.path(), "no-web-fetch", "default_engine = \"fast\"\n");

    let profile = ProfileConfig::load(temp.path(), "no-web-fetch").expect("the fixture loads");
    assert_eq!(
        profile.web_fetch,
        crate::config::WebFetchConfig::default(),
        "an absent [web_fetch] section behaves like the written-out default"
    );
}

#[test]
fn unknown_web_fetch_backend_is_a_hard_error_naming_the_supported_names() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-web-fetch-backend",
        r#"
[web_fetch]
backend = "camofox"
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-web-fetch-backend")
        .expect_err("an unknown backend must be rejected");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::InvalidWebFetchBackend { ref backend, .. } if backend == "camofox"
    ));
    assert!(
        message.contains("native"),
        "names accepted values: {message}"
    );
}

#[test]
fn web_search_section_loads_explicit_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "explicit-web-search",
        r#"
[web_search]
backend = "duckduckgo"
timeout_secs = 5
result_limit = 4
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "explicit-web-search").expect("the fixture loads");
    assert_eq!(profile.web_search.backend, "duckduckgo");
    assert_eq!(profile.web_search.timeout_secs, 5);
    assert_eq!(profile.web_search.result_limit, 4);
}

#[test]
fn absent_web_search_section_defaults_to_duckduckgo() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(temp.path(), "no-web-search", "default_engine = \"fast\"\n");

    let profile = ProfileConfig::load(temp.path(), "no-web-search").expect("the fixture loads");
    assert_eq!(
        profile.web_search,
        crate::config::WebSearchConfig::default(),
        "an absent [web_search] section behaves like the written-out default"
    );
    assert_eq!(profile.web_search.backend, "duckduckgo");
    assert_eq!(profile.web_search.timeout_secs, 30);
    assert_eq!(
        profile.web_search.result_limit,
        crate::agent::tools::backends::web_search::WEB_SEARCH_DEFAULT_RESULT_LIMIT
    );
}

#[test]
fn unknown_web_search_backend_is_rejected_naming_the_config_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-web-search-backend",
        "[web_search]\nbackend = \"startpage\"\n",
    );

    let error = ProfileConfig::load(temp.path(), "bad-web-search-backend")
        .expect_err("an unsupported backend must fail profile loading");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::InvalidWebSearchBackend { backend, .. } if backend == "startpage"
    ));
    assert!(message.contains("startpage"), "{message}");
    assert!(
        message.contains("duckduckgo, searxng, brave"),
        "names every accepted value: {message}"
    );
    assert!(
        message.contains("config.toml"),
        "the error must name the config path: {message}"
    );
}

#[test]
fn searxng_web_search_section_loads_its_instance_url() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "searxng-web-search",
        r#"
[web_search]
backend = "searxng"

[web_search.backends.searxng]
base_url = "https://searx.example.org"
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "searxng-web-search").expect("the fixture loads");
    assert_eq!(profile.web_search.backend, "searxng");
    assert_eq!(
        profile.web_search.backends.searxng,
        Some(crate::config::SearXngBackendConfig {
            base_url: Some("https://searx.example.org".to_string())
        })
    );
}

#[test]
fn searxng_web_search_backend_without_an_instance_url_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "searxng-without-instance",
        "[web_search]\nbackend = \"searxng\"\n",
    );

    let error = ProfileConfig::load(temp.path(), "searxng-without-instance")
        .expect_err("a missing instance table must fail profile loading");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::InvalidWebSearchBackendSetting { backend, .. } if backend == "searxng"
    ));
    assert!(
        message.contains("backends.searxng"),
        "names the config key: {message}"
    );
    assert!(
        message.contains("config.toml"),
        "the error must name the config path: {message}"
    );
}

#[test]
fn brave_web_search_section_loads_its_api_key() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "brave-web-search",
        r#"
[web_search]
backend = "brave"

[web_search.backends.brave]
api_key = "test-subscription-token"
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "brave-web-search").expect("the fixture loads");
    assert_eq!(profile.web_search.backend, "brave");
    assert_eq!(
        profile.web_search.backends.brave,
        Some(crate::config::BraveBackendConfig {
            api_key: Some("test-subscription-token".to_string())
        })
    );
    let rendered = format!("{profile:?}");
    assert!(
        !rendered.contains("test-subscription-token"),
        "the token must never reach a Debug output: {rendered}"
    );
}

#[test]
fn brave_web_search_backend_without_the_table_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "brave-without-table",
        "[web_search]\nbackend = \"brave\"\n",
    );

    let error = ProfileConfig::load(temp.path(), "brave-without-table")
        .expect_err("a missing brave table must fail profile loading");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::InvalidWebSearchBackendSetting { backend, .. } if backend == "brave"
    ));
    assert!(
        message.contains("backends.brave"),
        "names the config key: {message}"
    );
    assert!(
        message.contains("config.toml"),
        "the error must name the config path: {message}"
    );
}
