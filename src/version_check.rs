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

//! Daily check for a newer Ylva release.
//!
//! The GitHub Releases API is queried at most once per 24 hours. The time of
//! the last check and the latest version found live in
//! `<ylva_home>/version-check.toml`, so later starts can show the hint without
//! any network request. Failures are only logged; a corrupt cache counts as
//! absent. The network call sits behind [`ReleaseFetcher`] so tests never
//! touch the network.

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::build_info::BUILD_INFO;
use crate::http::provider_client_builder;
use crate::private_fs::{create_private_dir_all, replace_file};
use crate::unix_time::unix_now;

/// Environment variable that disables the check; wins over the cache file.
pub const DISABLE_ENVIRONMENT_VARIABLE: &str = "YLVA_NO_VERSION_CHECK";

const CACHE_FILE_NAME: &str = "version-check.toml";
const CHECK_INTERVAL_SECONDS: i64 = 24 * 60 * 60;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// A newer release than the running binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewVersionHint {
    /// Version without a leading `v`.
    pub version: String,
    pub release_url: String,
}

impl NewVersionHint {
    fn for_version(version: &str) -> Self {
        let version = version.trim_start_matches('v').to_string();
        let release_url = format!("{REPOSITORY_URL}/releases/tag/v{version}");
        Self {
            version,
            release_url,
        }
    }

    /// The single-line text shown by the logo and `--version`.
    pub fn summary(&self) -> String {
        format!("v{} available: {}", self.version, self.release_url)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionCheckCache {
    pub last_checked_unix: i64,
    pub latest_version: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VersionCheckState {
    pub enabled: bool,
    pub cache: Option<VersionCheckCache>,
}

impl Default for VersionCheckState {
    fn default() -> Self {
        Self {
            enabled: true,
            cache: None,
        }
    }
}

#[derive(Deserialize)]
struct StoredState {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    last_checked_unix: Option<i64>,
    latest_version: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

/// Source of the latest released version.
pub(crate) trait ReleaseFetcher {
    /// Latest release version, `None` when it cannot be determined.
    fn fetch_latest_version(&self) -> impl Future<Output = Option<String>> + Send;
}

/// Production fetcher querying the GitHub Releases API.
pub(crate) struct GithubReleaseFetcher;

impl ReleaseFetcher for GithubReleaseFetcher {
    async fn fetch_latest_version(&self) -> Option<String> {
        let repository_path = REPOSITORY_URL.strip_prefix("https://github.com/")?;
        let url = format!("https://api.github.com/repos/{repository_path}/releases/latest");
        let result = async {
            let client = provider_client_builder(CONNECT_TIMEOUT, TOTAL_TIMEOUT)
                .timeout(TOTAL_TIMEOUT)
                .build()?;
            let body: serde_json::Value = client
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok::<_, reqwest::Error>(body)
        }
        .await;
        match result {
            Ok(body) => body
                .get("tag_name")
                .and_then(serde_json::Value::as_str)
                .map(|tag| tag.trim_start_matches('v').to_string()),
            Err(error) => {
                tracing::debug!(%error, "release check request failed");
                None
            }
        }
    }
}

fn parse_version(text: &str) -> Option<Vec<u64>> {
    text.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|component| component.parse::<u64>().ok())
        .collect()
}

/// Whether `latest` is strictly newer than `current`. Compares numeric dotted
/// versions only; anything else (e.g. a pre-release suffix) is never newer.
pub(crate) fn is_newer_version(current: &str, latest: &str) -> bool {
    let (Some(mut current), Some(mut latest)) = (parse_version(current), parse_version(latest))
    else {
        return false;
    };
    let length = current.len().max(latest.len());
    current.resize(length, 0);
    latest.resize(length, 0);
    latest > current
}

/// Whether a network check is due; also true when the cache is absent or its
/// timestamp lies in the future.
pub(crate) fn should_check(now: i64, cache: Option<&VersionCheckCache>) -> bool {
    match cache {
        None => true,
        Some(cache) => {
            let age = now.saturating_sub(cache.last_checked_unix);
            !(0..CHECK_INTERVAL_SECONDS).contains(&age)
        }
    }
}

/// Whether the environment disables the check (any non-empty value but `0`).
pub(crate) fn environment_disables(value: Option<OsString>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

pub(crate) fn cache_path(ylva_home: &Path) -> PathBuf {
    ylva_home.join(CACHE_FILE_NAME)
}

/// Reads the persisted state; a missing or corrupt file means enabled with no
/// cache.
pub(crate) fn load_state(path: &Path) -> VersionCheckState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return VersionCheckState::default();
    };
    match toml::from_str::<StoredState>(&text) {
        Ok(stored) => VersionCheckState {
            enabled: stored.enabled,
            cache: stored
                .last_checked_unix
                .map(|last_checked_unix| VersionCheckCache {
                    last_checked_unix,
                    latest_version: stored.latest_version,
                }),
        },
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring corrupt version-check cache");
            VersionCheckState::default()
        }
    }
}

/// Persists the cache, editing the existing file in place so `enabled` and
/// unknown keys survive. Failures are only logged.
pub(crate) fn store_cache(path: &Path, cache: &VersionCheckCache) {
    let mut document = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .unwrap_or_default();
    document["last_checked_unix"] = toml_edit::value(cache.last_checked_unix);
    match &cache.latest_version {
        Some(version) => document["latest_version"] = toml_edit::value(version.as_str()),
        None => {
            document.remove("latest_version");
        }
    }
    let result = path
        .parent()
        .map_or(Ok(()), create_private_dir_all)
        .and_then(|()| replace_file(path, document.to_string().as_bytes(), 0o600));
    if let Err(error) = result {
        tracing::warn!(path = %path.display(), %error, "could not write version-check cache");
    }
}

fn hint_if_newer(current_version: &str, latest: Option<&str>) -> Option<NewVersionHint> {
    latest
        .filter(|latest| is_newer_version(current_version, latest))
        .map(NewVersionHint::for_version)
}

/// Cheap synchronous read: the hint from the cache alone, `None` when
/// disabled, absent or not newer.
pub(crate) fn cached_hint(path: &Path, current_version: &str) -> Option<NewVersionHint> {
    let state = load_state(path);
    if !state.enabled {
        return None;
    }
    let cache = state.cache?;
    hint_if_newer(current_version, cache.latest_version.as_deref())
}

/// Refreshes the cache through `fetcher` when a check is due and returns the
/// hint for the resulting latest version. The timestamp advances even when the
/// fetch fails, so an offline machine does not retry on every start; the
/// previously cached version is kept then.
pub(crate) async fn check_for_new_version<F: ReleaseFetcher>(
    fetcher: &F,
    path: &Path,
    now: i64,
    current_version: &str,
) -> Option<NewVersionHint> {
    let state = load_state(path);
    if !state.enabled {
        return None;
    }
    let mut latest_version = state
        .cache
        .as_ref()
        .and_then(|cache| cache.latest_version.clone());
    if should_check(now, state.cache.as_ref()) {
        if let Some(fetched) = fetcher.fetch_latest_version().await {
            latest_version = Some(fetched.trim_start_matches('v').to_string());
        }
        store_cache(
            path,
            &VersionCheckCache {
                last_checked_unix: now,
                latest_version: latest_version.clone(),
            },
        );
    }
    hint_if_newer(current_version, latest_version.as_deref())
}

/// Startup result: the instantly known hint and, when a network refresh was
/// started, a channel delivering the refreshed hint.
pub(crate) struct VersionCheckStart {
    pub initial_hint: Option<NewVersionHint>,
    pub refresh: Option<mpsc::UnboundedReceiver<NewVersionHint>>,
}

/// Reads the cached hint and, when a check is due, refreshes in a background
/// task. Must run inside a Tokio runtime. Does nothing when disabled.
pub(crate) fn start_version_check(ylva_home: Option<&Path>) -> VersionCheckStart {
    let none = VersionCheckStart {
        initial_hint: None,
        refresh: None,
    };
    let Some(ylva_home) = ylva_home else {
        return none;
    };
    if environment_disables(std::env::var_os(DISABLE_ENVIRONMENT_VARIABLE)) {
        return none;
    }
    let path = cache_path(ylva_home);
    let state = load_state(&path);
    if !state.enabled {
        return none;
    }
    let initial_hint = state
        .cache
        .as_ref()
        .and_then(|cache| hint_if_newer(BUILD_INFO.version, cache.latest_version.as_deref()));
    let now = unix_now();
    if !should_check(now, state.cache.as_ref()) {
        return VersionCheckStart {
            initial_hint,
            refresh: None,
        };
    }
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Some(hint) =
            check_for_new_version(&GithubReleaseFetcher, &path, now, BUILD_INFO.version).await
        {
            let _ = sender.send(hint);
        }
    });
    VersionCheckStart {
        initial_hint,
        refresh: Some(receiver),
    }
}

/// Cached hint for `--version`; no network, no runtime. `None` when the
/// environment disables the check.
pub(crate) fn cached_hint_for_home(ylva_home: Option<&Path>) -> Option<NewVersionHint> {
    if environment_disables(std::env::var_os(DISABLE_ENVIRONMENT_VARIABLE)) {
        return None;
    }
    cached_hint(&cache_path(ylva_home?), BUILD_INFO.version)
}

#[cfg(test)]
mod tests;
