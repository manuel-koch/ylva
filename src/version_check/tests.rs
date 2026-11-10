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
use std::sync::atomic::{AtomicUsize, Ordering};

struct StubFetcher {
    result: Option<String>,
    calls: AtomicUsize,
}

impl StubFetcher {
    fn new(result: Option<&str>) -> Self {
        Self {
            result: result.map(str::to_string),
            calls: AtomicUsize::new(0),
        }
    }
}

impl ReleaseFetcher for StubFetcher {
    async fn fetch_latest_version(&self) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.clone()
    }
}

fn cache(last_checked_unix: i64, latest_version: Option<&str>) -> VersionCheckCache {
    VersionCheckCache {
        last_checked_unix,
        latest_version: latest_version.map(str::to_string),
    }
}

#[test]
fn version_comparison_table() {
    let cases = [
        ("0.35.0", "0.36.0", true),
        ("0.35.0", "v0.35.1", true),
        ("v0.35.0", "0.35.0", false),
        ("0.35.0", "0.34.9", false),
        ("0.35", "0.35.0", false),
        ("0.35", "0.35.1", true),
        ("0.9.0", "0.10.0", true),
        ("0.35.0", "1.0.0-rc1", false),
        ("0.35.0", "garbage", false),
        ("garbage", "1.0.0", false),
        ("0.35.0", "", false),
    ];
    for (current, latest, expected) in cases {
        assert_eq!(
            is_newer_version(current, latest),
            expected,
            "{current} vs {latest}"
        );
    }
}

#[test]
fn should_check_boundaries() {
    let now = 1_000_000;
    assert!(should_check(now, None));
    assert!(!should_check(now, Some(&cache(now, None))));
    assert!(!should_check(
        now,
        Some(&cache(now - CHECK_INTERVAL_SECONDS + 1, None))
    ));
    assert!(should_check(
        now,
        Some(&cache(now - CHECK_INTERVAL_SECONDS, None))
    ));
    assert!(should_check(now, Some(&cache(now + 1, None))));
}

#[test]
fn environment_disables_for_non_empty_values_except_zero() {
    assert!(!environment_disables(None));
    assert!(!environment_disables(Some("".into())));
    assert!(!environment_disables(Some("0".into())));
    assert!(environment_disables(Some("1".into())));
}

#[test]
fn state_roundtrip_missing_and_corrupt() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    assert_eq!(load_state(&path), VersionCheckState::default());

    store_cache(&path, &cache(42, Some("1.2.3")));
    let state = load_state(&path);
    assert!(state.enabled);
    assert_eq!(state.cache, Some(cache(42, Some("1.2.3"))));

    std::fs::write(&path, "last_checked_unix = [").expect("write corrupt");
    assert_eq!(load_state(&path), VersionCheckState::default());
    std::fs::write(&path, "enabled = false\nlast_checked_unix = \"x\"").expect("write mistyped");
    assert_eq!(load_state(&path), VersionCheckState::default());
}

#[test]
fn store_cache_keeps_enabled_flag_and_unknown_keys() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    std::fs::write(&path, "enabled = false\nextra = \"kept\"\n").expect("seed");

    store_cache(&path, &cache(7, Some("2.0.0")));

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("extra = \"kept\""), "{text}");
    let state = load_state(&path);
    assert!(!state.enabled);
    assert_eq!(state.cache, Some(cache(7, Some("2.0.0"))));
}

#[cfg(unix)]
#[test]
fn store_cache_writes_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(&directory.path().join("nested"));
    store_cache(&path, &cache(1, None));
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn check_skips_fetch_within_interval_and_uses_cached_version() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    store_cache(&path, &cache(1_000, Some("0.40.0")));
    let fetcher = StubFetcher::new(Some("9.9.9"));

    let hint = check_for_new_version(&fetcher, &path, 2_000, "0.35.0").await;

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(hint, Some(NewVersionHint::for_version("0.40.0")));
}

#[tokio::test]
async fn check_fetches_when_stale_and_writes_cache() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    let fetcher = StubFetcher::new(Some("v0.40.0"));

    let hint = check_for_new_version(&fetcher, &path, 5_000_000, "0.35.0")
        .await
        .expect("hint");

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hint.version, "0.40.0");
    assert_eq!(
        hint.release_url,
        format!("{REPOSITORY_URL}/releases/tag/v0.40.0")
    );
    assert_eq!(
        load_state(&path).cache,
        Some(cache(5_000_000, Some("0.40.0")))
    );
}

#[tokio::test]
async fn check_failure_keeps_cached_version_and_advances_timestamp() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    store_cache(&path, &cache(0, Some("0.40.0")));
    let fetcher = StubFetcher::new(None);

    let hint = check_for_new_version(&fetcher, &path, 5_000_000, "0.35.0").await;

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hint, Some(NewVersionHint::for_version("0.40.0")));
    assert_eq!(
        load_state(&path).cache,
        Some(cache(5_000_000, Some("0.40.0")))
    );
}

#[tokio::test]
async fn check_has_no_hint_when_current_or_disabled() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    let fetcher = StubFetcher::new(Some("0.35.0"));
    assert_eq!(
        check_for_new_version(&fetcher, &path, 5_000_000, "0.35.0").await,
        None
    );

    std::fs::write(&path, "enabled = false\n").expect("write");
    let fetcher = StubFetcher::new(Some("9.0.0"));
    assert_eq!(
        check_for_new_version(&fetcher, &path, 5_000_000, "0.35.0").await,
        None
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cached_hint_follows_cache_and_enabled_flag() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = cache_path(directory.path());
    assert_eq!(cached_hint(&path, "0.35.0"), None);

    store_cache(&path, &cache(1, Some("0.40.0")));
    assert_eq!(
        cached_hint(&path, "0.35.0"),
        Some(NewVersionHint::for_version("0.40.0"))
    );
    assert_eq!(cached_hint(&path, "0.40.0"), None);

    std::fs::write(
        &path,
        "enabled = false\nlast_checked_unix = 1\nlatest_version = \"0.40.0\"",
    )
    .expect("write");
    assert_eq!(cached_hint(&path, "0.35.0"), None);
}
