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

#[cfg(unix)]
fn visible_temporary_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ylva-")
        .tempdir()
        .expect("tempdir")
}

#[test]
fn appends_canonical_path_when_supplied_path_is_a_symlink() {
    #[cfg(unix)]
    {
        let workspace = visible_temporary_directory();
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let target = outside.path().join("kdbx-vault-sync.sh");
        std::fs::write(&target, "x").expect("seed target");
        let link = workspace.path().join("kdbx-vault-sync.sh");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let resolved = crate::sandbox::SandboxContext::canonicalize_best_effort(&link);
        let supplied = "kdbx-vault-sync.sh";
        let title_arguments = canonical_path_title_arguments(
            vec![("path".to_string(), serde_json::json!(supplied))],
            supplied,
            &resolved,
        );
        let appended = title_arguments
            .iter()
            .find(|(key, _)| key == CANONICAL_PATH_ARGUMENT)
            .map(|(_, value)| value.as_str().expect("string value").to_string());
        assert_eq!(appended.as_deref(), Some(resolved.to_str().unwrap()));
    }
    #[cfg(not(unix))]
    {
        // Non-unix targets have no symlinks: the same-path case must hold.
        let arguments = vec![("path".to_string(), serde_json::json!("a.txt"))];
        let resolved = std::path::absolute("a.txt").unwrap();
        assert!(canonical_path_title_arguments(arguments, "a.txt", &resolved).is_empty());
    }
}

#[test]
fn omits_canonical_path_for_plain_existing_path() {
    let workspace = visible_temporary_directory();
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "x").expect("seed");
    let resolved = crate::sandbox::SandboxContext::canonicalize_best_effort(&file);
    let supplied = file.to_str().unwrap();
    let title_arguments = canonical_path_title_arguments(
        vec![("path".to_string(), serde_json::json!(supplied))],
        supplied,
        &resolved,
    );
    assert!(
        title_arguments
            .iter()
            .all(|(key, _)| key != CANONICAL_PATH_ARGUMENT),
        "an identical path must not gain the disclosure prop: {title_arguments:?}"
    );
}

#[test]
fn omits_canonical_path_when_spelling_only_differs_lexically() {
    let workspace = visible_temporary_directory();
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "x").expect("seed");
    let resolved = crate::sandbox::SandboxContext::canonicalize_best_effort(&file);
    // The supplied spelling uses a redundant `.` segment but names the same
    // location; lexical normalization must recognize it.
    let supplied = format!("{}/./a.txt", workspace.path().display());
    let title_arguments = canonical_path_title_arguments(
        vec![("path".to_string(), serde_json::json!(supplied))],
        &supplied,
        &resolved,
    );
    assert!(
        title_arguments
            .iter()
            .all(|(key, _)| key != CANONICAL_PATH_ARGUMENT),
        "an only-lexically-different spelling must not gain the prop: {title_arguments:?}"
    );
}

#[test]
fn omits_canonical_path_for_empty_inputs() {
    let result = canonical_path_title_arguments(vec![], "", std::path::Path::new("/tmp/a"));
    assert!(
        result.iter().all(|(key, _)| key != CANONICAL_PATH_ARGUMENT),
        "an empty supplied path must not gain the prop: {result:?}"
    );
    let arguments = vec![("path".to_string(), serde_json::json!("a.txt"))];
    let result = canonical_path_title_arguments(arguments, "a.txt", std::path::Path::new(""));
    assert!(
        result.iter().all(|(key, _)| key != CANONICAL_PATH_ARGUMENT),
        "an empty resolution must not disclose anything: {result:?}"
    );
}

#[test]
fn returned_value_is_absolute_for_a_relative_supplied_path() {
    #[cfg(unix)]
    {
        let workspace = visible_temporary_directory();
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let target = outside.path().join("real.conf");
        std::fs::write(&target, "x").expect("seed target");
        let link = workspace.path().join("real.conf");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let resolved = crate::sandbox::SandboxContext::canonicalize_best_effort(&link);
        let title_arguments = canonical_path_title_arguments(vec![], "real.conf", &resolved);
        let value = title_arguments[0].1.as_str().expect("string value");
        assert!(
            std::path::Path::new(value).is_absolute(),
            "the disclosure must be absolute: {value}"
        );
    }
    #[cfg(not(unix))]
    {
        // No symlink difference is expressible off-unix; nothing to assert
        // beyond the same-path case covered above.
    }
}

#[test]
fn prop_is_ordered_after_the_calls_own_arguments() {
    #[cfg(unix)]
    {
        let workspace = visible_temporary_directory();
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        let target = outside.path().join("script.sh");
        std::fs::write(&target, "x").expect("seed target");
        let link = workspace.path().join("script.sh");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let resolved = crate::sandbox::SandboxContext::canonicalize_best_effort(&link);
        let title_arguments = canonical_path_title_arguments(
            vec![
                ("mode".to_string(), serde_json::json!("search_replace")),
                ("path".to_string(), serde_json::json!("script.sh")),
            ],
            "script.sh",
            &resolved,
        );
        assert_eq!(
            title_arguments.last().map(|(key, _)| key.as_str()),
            Some(CANONICAL_PATH_ARGUMENT)
        );
    }
    #[cfg(not(unix))]
    {
        // Same-path case: nothing appended, nothing to order.
        let arguments = vec![("mode".to_string(), serde_json::json!("search_replace"))];
        let resolved = std::path::absolute("script.sh").unwrap();
        let result = canonical_path_title_arguments(arguments, "script.sh", &resolved);
        assert!(result.iter().all(|(key, _)| key != CANONICAL_PATH_ARGUMENT));
    }
}
