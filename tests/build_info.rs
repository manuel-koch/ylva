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

mod common;

use common::matches_utc_shape;
use ylva::BUILD_INFO;

/// The compiled-in `version` must equal the manifest's package version.
#[test]
fn version_matches_cargo_package_version() {
    assert_eq!(BUILD_INFO.version, env!("CARGO_PKG_VERSION"));
}

/// `git_commit` is either a short 8-char hex digest or the fallback
/// `"unknown"` (non-git checkout, e.g. a tarball install).
#[test]
fn git_commit_is_short_sha_or_unknown() {
    let commit = BUILD_INFO.git_commit;
    let is_sha = commit.len() == 8 && commit.chars().all(|char| char.is_ascii_hexdigit());
    assert!(
        commit == "unknown" || is_sha,
        "git_commit must be an 8-char hex SHA-1 or `unknown`, got: {commit:?}"
    );
}

/// `build_timestamp_utc` matches `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\Z$`
/// (the trailing `Z` is literal — a `+00:00` style must fail).
#[test]
fn build_timestamp_utc_matches_utc_shape() {
    let timestamp = BUILD_INFO.build_timestamp_utc;
    assert!(matches_utc_shape(timestamp), "got: {timestamp:?}");
}

/// `profile` is one of the two Cargo build profiles.
#[test]
fn profile_is_debug_or_release() {
    assert!(
        BUILD_INFO.profile == "debug" || BUILD_INFO.profile == "release",
        "profile must be debug or release, got: {:?}",
        BUILD_INFO.profile
    );
}
