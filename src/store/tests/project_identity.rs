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

use std::path::Path;

use crate::store::project_key;

fn visible_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ylva-")
        .tempdir()
        .expect("tempdir")
}

fn make_main_checkout(root: &Path) {
    std::fs::create_dir_all(root.join(".git").join("worktrees")).expect("mkdir");
}

/// Builds a linked worktree at `linked` whose common dir is `common_directory`.
fn make_linked_worktree(linked: &Path, common_directory: &Path, name: &str) {
    let admin = common_directory.join("worktrees").join(name);
    std::fs::create_dir_all(&admin).expect("mkdir admin");
    std::fs::write(admin.join("commondir"), "../..\n").expect("commondir");
    std::fs::create_dir_all(linked).expect("mkdir linked");
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("git file");
}

#[test]
fn linked_worktree_root_shares_key_with_main_root() {
    let temp = visible_tempdir();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    make_main_checkout(&main);
    make_linked_worktree(&linked, &main.join(".git"), "linked");

    assert_eq!(project_key(&linked), project_key(&main));
}

#[test]
fn linked_worktree_subdirectory_maps_to_same_main_subdirectory() {
    let temp = visible_tempdir();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    make_main_checkout(&main);
    make_linked_worktree(&linked, &main.join(".git"), "linked");
    std::fs::create_dir_all(main.join("a/b")).expect("mkdir");
    std::fs::create_dir_all(linked.join("a/b")).expect("mkdir");

    assert_eq!(
        project_key(&linked.join("a/b")),
        project_key(&main.join("a/b"))
    );
    assert_ne!(project_key(&linked.join("a/b")), project_key(&main));
}

#[test]
fn main_checkout_and_non_git_directory_keep_plain_canonical_key() {
    use sha2::{Digest, Sha256};
    let temp = visible_tempdir();
    let main = temp.path().join("main");
    let plain = temp.path().join("plain");
    make_main_checkout(&main);
    std::fs::create_dir_all(&plain).expect("mkdir");

    for directory in [&main, &plain] {
        let canonical = std::fs::canonicalize(directory).expect("canonical");
        let expected: String = Sha256::digest(canonical.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(project_key(directory), expected);
    }
}

#[test]
fn submodule_style_git_file_falls_back_to_launch_directory() {
    let temp = visible_tempdir();
    let submodule = temp.path().join("sub");
    let modules = temp.path().join("super/.git/modules/sub");
    std::fs::create_dir_all(&modules).expect("mkdir");
    std::fs::create_dir_all(&submodule).expect("mkdir");
    std::fs::write(
        submodule.join(".git"),
        format!("gitdir: {}\n", modules.display()),
    )
    .expect("git file");

    let other = temp.path().join("other");
    std::fs::create_dir_all(&other).expect("mkdir");
    assert_ne!(project_key(&submodule), project_key(&other));
    assert_ne!(
        project_key(&submodule),
        project_key(&temp.path().join("super"))
    );
}

#[test]
fn malformed_git_file_falls_back_without_error() {
    let temp = visible_tempdir();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    std::fs::create_dir_all(&first).expect("mkdir");
    std::fs::create_dir_all(&second).expect("mkdir");
    std::fs::write(first.join(".git"), "garbage").expect("git file");
    std::fs::write(second.join(".git"), "gitdir: /does/not/exist\n").expect("git file");

    assert_ne!(project_key(&first), project_key(&second));
    assert_eq!(project_key(&first), project_key(&first));
}

#[test]
fn missing_main_checkout_still_yields_main_equivalent_key() {
    let temp = visible_tempdir();
    let linked = temp.path().join("linked");
    let admin = temp.path().join("admin");
    let gone = temp.path().join("gone");
    let present = temp.path().join("present");
    std::fs::create_dir_all(&admin).expect("mkdir");
    std::fs::create_dir_all(&linked).expect("mkdir");
    std::fs::create_dir_all(&present).expect("mkdir");
    std::fs::write(
        admin.join("commondir"),
        format!("{}\n", gone.join(".git").display()),
    )
    .expect("commondir");
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("git file");

    let missing_key = project_key(&linked);
    std::fs::create_dir_all(gone.join(".git")).expect("mkdir");
    assert_eq!(missing_key, project_key(&gone));
}

#[test]
fn bare_repository_worktree_maps_to_bare_directory_anchor() {
    let temp = visible_tempdir();
    let bare = temp.path().join("repo.git");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    make_linked_worktree(&first, &bare, "first");
    make_linked_worktree(&second, &bare, "second");

    assert_eq!(project_key(&first), project_key(&second));
    assert_eq!(project_key(&first), project_key(&bare));
}

#[cfg(unix)]
#[test]
fn symlinked_linked_worktree_canonicalizes_to_one_key() {
    let temp = visible_tempdir();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    make_main_checkout(&main);
    make_linked_worktree(&linked, &main.join(".git"), "linked");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&linked, &link).expect("symlink");

    assert_eq!(project_key(&link), project_key(&main));
}

#[test]
fn real_git_worktree_shares_key_with_main_checkout() {
    let temp = visible_tempdir();
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    std::fs::create_dir_all(&main).expect("mkdir");
    let run = |directory: &Path, arguments: &[&str]| -> bool {
        std::process::Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    if !run(&main, &["init", "-q"]) {
        return;
    }
    assert!(run(&main, &["commit", "-q", "--allow-empty", "-m", "init"]));
    assert!(run(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "side"
        ]
    ));

    assert_eq!(project_key(&linked), project_key(&main));
}
