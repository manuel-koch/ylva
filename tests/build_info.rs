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

/// `build_timestamp_utc` matches `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\+Z$`
/// (the trailing `+Z` is literal — a `+00:00` style must fail).
#[test]
fn build_timestamp_utc_matches_utc_shape() {
    let timestamp = BUILD_INFO.build_timestamp_utc;
    assert!(matches_utc_shape(timestamp), "got: {timestamp:?}");
}

fn matches_utc_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 21 {
        return false;
    }
    let digit_groups = [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2)];
    for (offset, count) in digit_groups {
        let slice = &bytes[offset..offset + count];
        if !slice.iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'+'
        && bytes[20] == b'Z'
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
