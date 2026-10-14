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

//! Spawned-binary coverage of `ylva profile`: the home and workspace scopes,
//! starter parity with `ylva init`, `--force`, `--dry-run`, `edit`, and exit
//! codes. Each test runs against tempdirs, never the real `~/.ylva`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn run_ylva(
    ylva_home: &Path,
    working_directory: &Path,
    arguments: &[&str],
    editor_variables: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home)
        .current_dir(working_directory)
        .args(arguments);
    for (name, value) in editor_variables {
        command.env(name, value);
    }
    command.output().expect("spawn the ylva binary")
}

fn run_home(ylva_home: &Path, arguments: &[&str]) -> Output {
    run_ylva(ylva_home, ylva_home, arguments, &[])
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn profile_directory(profiles_root: &Path, name: &str) -> PathBuf {
    profiles_root.join(name)
}

fn workspace_profiles_root(workspace: &Path) -> PathBuf {
    workspace.join(ylva::YLVA_DIR_NAME).join("profiles")
}

fn make_workspace(parent: &Path) -> PathBuf {
    let workspace = parent.join("workspace");
    fs::create_dir_all(workspace.join(".git")).expect("create workspace marker");
    workspace
}

#[test]
fn list_on_an_empty_home_exits_zero() {
    let ylva_home = TempDir::new().expect("home tempdir");

    let output = run_home(ylva_home.path(), &["profile", "list"]);

    assert_eq!(exit_code(&output), 0, "list must succeed: {output:?}");
}

#[test]
fn list_shows_created_profiles_sorted() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    run_home(ylva_home.path(), &["profile", "create", "zeta"]);
    run_home(ylva_home.path(), &["profile", "create", "alpha"]);

    let output = run_home(ylva_home.path(), &["profile", "list"]);

    assert_eq!(exit_code(&output), 0);
    let stdout = stdout_of(&output);
    let alpha_position = stdout.find("alpha").expect("alpha is listed");
    let zeta_position = stdout.find("zeta").expect("zeta is listed");
    assert!(
        alpha_position < zeta_position,
        "list must be sorted: {stdout}"
    );
    assert!(
        stdout.contains(&profiles_root.join("alpha").display().to_string()),
        "list must show each profile's path: {stdout}"
    );
}

#[test]
fn create_writes_the_same_starters_as_init() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(exit_code(&run_home(ylva_home.path(), &["init"])), 0);

    let output = run_home(ylva_home.path(), &["profile", "create", "work"]);

    assert_eq!(exit_code(&output), 0, "create must succeed: {output:?}");
    for file_name in ["config.toml", ".env"] {
        let seeded_by_init =
            fs::read_to_string(profile_directory(&profiles_root, "default").join(file_name))
                .expect("init seeded the default profile");
        let seeded_by_create =
            fs::read_to_string(profile_directory(&profiles_root, "work").join(file_name))
                .expect("create seeded the work profile");
        assert_eq!(
            seeded_by_create, seeded_by_init,
            "{file_name} must match the init starter"
        );
    }
    assert!(
        !profile_directory(&profiles_root, "work")
            .join("agents")
            .exists(),
        "create must not create agents/"
    );
}

#[test]
fn duplicate_create_exits_two_and_changes_nothing() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );
    let config_path = profile_directory(&profiles_root, "work").join("config.toml");
    fs::write(&config_path, "# hand edited\n").expect("hand edit config");

    let output = run_home(ylva_home.path(), &["profile", "create", "work"]);

    assert_eq!(exit_code(&output), 2, "duplicate create must exit 2");
    assert_eq!(
        fs::read_to_string(&config_path).expect("config still present"),
        "# hand edited\n",
        "a refused create must not change the existing file"
    );
}

#[test]
fn create_force_overwrites_only_the_two_starters() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(exit_code(&run_home(ylva_home.path(), &["init"])), 0);
    let starter_config =
        fs::read_to_string(profile_directory(&profiles_root, "default").join("config.toml"))
            .expect("read starter");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );
    let work_directory = profile_directory(&profiles_root, "work");
    fs::write(work_directory.join("config.toml"), "# custom\n").expect("customize config");
    fs::create_dir_all(work_directory.join("agents")).expect("create agents dir");
    fs::write(work_directory.join("agents").join("keep.md"), "keep me").expect("write agent");

    let output = run_home(ylva_home.path(), &["profile", "create", "work", "--force"]);

    assert_eq!(
        exit_code(&output),
        0,
        "forced create must succeed: {output:?}"
    );
    assert_eq!(
        fs::read_to_string(work_directory.join("config.toml")).expect("config present"),
        starter_config,
        "--force must restore the config starter"
    );
    assert_eq!(
        fs::read_to_string(work_directory.join("agents").join("keep.md"))
            .expect("agents file kept"),
        "keep me",
        "--force must keep files other than the two starters"
    );
}

#[test]
fn delete_removes_the_profile_directory() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );

    let output = run_home(ylva_home.path(), &["profile", "delete", "work"]);

    assert_eq!(exit_code(&output), 0, "delete must succeed: {output:?}");
    assert!(!profile_directory(&profiles_root, "work").exists());
}

#[test]
fn delete_dry_run_keeps_the_profile_directory() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );

    let output = run_home(
        ylva_home.path(),
        &["profile", "delete", "work", "--dry-run"],
    );

    assert_eq!(exit_code(&output), 0, "dry run must succeed: {output:?}");
    assert!(
        stdout_of(&output).contains("would delete \"work\""),
        "dry run must say what it would delete: {}",
        stdout_of(&output)
    );
    assert!(profile_directory(&profiles_root, "work").exists());
}

#[test]
fn delete_default_removes_the_default_profile() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");
    assert_eq!(exit_code(&run_home(ylva_home.path(), &["init"])), 0);

    let output = run_home(ylva_home.path(), &["profile", "delete", "default"]);

    assert_eq!(
        exit_code(&output),
        0,
        "deleting default is allowed: {output:?}"
    );
    assert!(!profile_directory(&profiles_root, "default").exists());
}

#[test]
fn delete_of_a_missing_profile_exits_two() {
    let ylva_home = TempDir::new().expect("home tempdir");

    let output = run_home(ylva_home.path(), &["profile", "delete", "missing"]);

    assert_eq!(exit_code(&output), 2);
}

#[test]
fn workspace_create_and_delete_stay_in_the_workspace() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let scratch = TempDir::new().expect("scratch tempdir");
    let workspace = make_workspace(scratch.path());
    let workspace_profiles = workspace_profiles_root(&workspace);
    let home_profiles = ylva_home.path().join("profiles");

    let created = run_ylva(
        ylva_home.path(),
        &workspace,
        &["profile", "create", "--workspace", "team"],
        &[],
    );
    assert_eq!(exit_code(&created), 0, "workspace create: {created:?}");
    assert!(profile_directory(&workspace_profiles, "team")
        .join("config.toml")
        .exists());
    assert!(
        workspace
            .join(ylva::YLVA_DIR_NAME)
            .join(".gitignore")
            .exists(),
        "workspace create must seed .ylva/.gitignore"
    );

    let listed = run_ylva(
        ylva_home.path(),
        &workspace,
        &["profile", "list", "--workspace"],
        &[],
    );
    assert_eq!(exit_code(&listed), 0);
    assert!(stdout_of(&listed).contains("team"));

    let merged_listing = run_ylva(ylva_home.path(), &workspace, &["profile", "list"], &[]);
    assert_eq!(exit_code(&merged_listing), 0);
    assert!(
        stdout_of(&merged_listing).contains("team"),
        "the merged listing must show the workspace profile"
    );
    assert!(
        !home_profiles.join("team").exists(),
        "workspace create must not write to the home"
    );

    let deleted = run_ylva(
        ylva_home.path(),
        &workspace,
        &["profile", "delete", "--workspace", "team"],
        &[],
    );
    assert_eq!(exit_code(&deleted), 0, "workspace delete: {deleted:?}");
    assert!(!profile_directory(&workspace_profiles, "team").exists());
}

#[test]
fn workspace_command_without_a_workspace_root_exits_two() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let scratch = TempDir::new().expect("scratch tempdir");

    let output = run_ylva(
        ylva_home.path(),
        scratch.path(),
        &["profile", "create", "--workspace", "team"],
        &[],
    );

    assert_eq!(exit_code(&output), 2, "no workspace root must exit 2");
}

#[test]
fn edit_with_editor_true_exits_zero() {
    let ylva_home = TempDir::new().expect("home tempdir");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );

    let output = run_ylva(
        ylva_home.path(),
        ylva_home.path(),
        &["profile", "edit", "work"],
        &[("EDITOR", "true")],
    );

    assert_eq!(exit_code(&output), 0, "edit must succeed: {output:?}");
}

#[test]
fn edit_without_an_editor_exits_two() {
    let ylva_home = TempDir::new().expect("home tempdir");
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );

    let output = run_home(ylva_home.path(), &["profile", "edit", "work"]);

    assert_eq!(
        exit_code(&output),
        2,
        "edit without VISUAL or EDITOR must exit 2"
    );
}

#[test]
fn invalid_profile_names_exit_two() {
    let ylva_home = TempDir::new().expect("home tempdir");

    for invalid_name in ["../escape", "a/b", "has space", ""] {
        let output = run_home(ylva_home.path(), &["profile", "create", invalid_name]);
        assert_eq!(
            exit_code(&output),
            2,
            "create {invalid_name:?} must exit 2: {output:?}"
        );
    }
    assert!(
        !ylva_home.path().join("escape").exists(),
        "a rejected name must not write outside the profiles root"
    );
}

#[test]
fn profile_flag_is_ignored_by_profile_subcommands() {
    let ylva_home = TempDir::new().expect("home tempdir");
    let profiles_root = ylva_home.path().join("profiles");

    let output = run_home(
        ylva_home.path(),
        &["--profile", "x", "profile", "create", "y"],
    );

    assert_eq!(exit_code(&output), 0, "create y must succeed: {output:?}");
    assert!(profile_directory(&profiles_root, "y").exists());
    assert!(
        !profile_directory(&profiles_root, "x").exists(),
        "--profile must not create the named profile"
    );
}

#[test]
fn list_marks_only_the_default_line_as_the_implicit_default() {
    let ylva_home = TempDir::new().expect("home tempdir");
    assert_eq!(
        exit_code(&run_home(
            ylva_home.path(),
            &["profile", "create", "default"]
        )),
        0
    );
    assert_eq!(
        exit_code(&run_home(ylva_home.path(), &["profile", "create", "work"])),
        0
    );

    let output = run_home(ylva_home.path(), &["profile", "list"]);

    assert_eq!(exit_code(&output), 0, "list must succeed: {output:?}");
    let stdout = stdout_of(&output);
    let marked_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("(implicit default)"))
        .collect();
    assert_eq!(
        marked_lines.len(),
        1,
        "exactly one line is marked: {stdout}"
    );
    assert!(
        marked_lines[0].starts_with("default  "),
        "the mark belongs to the default line: {stdout}"
    );
    assert!(
        stdout
            .lines()
            .filter(|line| line.starts_with("work  "))
            .all(|line| !line.contains("implicit")),
        "the work line must carry no mark: {stdout}"
    );
}
