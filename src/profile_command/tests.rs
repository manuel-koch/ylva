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

use std::cell::RefCell;

use crate::config::{DEFAULT_PROFILE_NAME, PROFILE_CONFIG_FILE_NAME, PROFILE_ENV_FILE_NAME};
use crate::home_relative_path::display_path;
use crate::init_command::run_init;

/// Records every launch instead of starting an editor.
struct RecordingLauncher {
    exit_succeeds: bool,
    launches: RefCell<Vec<(String, PathBuf)>>,
}

impl RecordingLauncher {
    fn succeeding() -> Self {
        Self {
            exit_succeeds: true,
            launches: RefCell::new(Vec::new()),
        }
    }

    fn failing() -> Self {
        Self {
            exit_succeeds: false,
            launches: RefCell::new(Vec::new()),
        }
    }

    fn launched_commands(&self) -> Vec<String> {
        self.launches
            .borrow()
            .iter()
            .map(|(command, _)| command.clone())
            .collect()
    }
}

impl EditorLauncher for RecordingLauncher {
    fn launch(&self, editor_command: &str, file_path: &Path) -> io::Result<bool> {
        self.launches
            .borrow_mut()
            .push((editor_command.to_string(), file_path.to_path_buf()));
        Ok(self.exit_succeeds)
    }
}

/// Fails to start at all, as when `sh` is missing.
struct UnstartableLauncher;

impl EditorLauncher for UnstartableLauncher {
    fn launch(&self, _editor_command: &str, _file_path: &Path) -> io::Result<bool> {
        Err(io::Error::other("cannot start editor"))
    }
}

struct Outcome {
    code: u8,
    output: String,
    diagnostics: String,
}

fn execute_with_environment(
    command: &ProfileCommand,
    scope: &ProfileScope,
    environment: &ProfileEnvironment,
    launcher: &dyn EditorLauncher,
) -> Outcome {
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    let code = run_in_scope(
        command,
        scope,
        environment,
        launcher,
        &mut output,
        &mut diagnostics,
    )
    .expect("run_in_scope writes to in-memory buffers");
    Outcome {
        code,
        output: String::from_utf8(output).expect("utf-8 output"),
        diagnostics: String::from_utf8(diagnostics).expect("utf-8 diagnostics"),
    }
}

fn execute_with_editor(
    command: &ProfileCommand,
    scope: &ProfileScope,
    editor: &EditorSettings,
    launcher: &dyn EditorLauncher,
) -> Outcome {
    let environment = ProfileEnvironment {
        editor: editor.clone(),
        path_variable: None,
    };
    execute_with_environment(command, scope, &environment, launcher)
}

fn execute_quietly(command: &ProfileCommand, scope: &ProfileScope) -> Outcome {
    execute_with_editor(
        command,
        scope,
        &EditorSettings::default(),
        &RecordingLauncher::succeeding(),
    )
}

fn home_test_scope(temporary_directory: &Path) -> ProfileScope {
    ProfileScope::Home {
        ylva_home: temporary_directory.join("home"),
    }
}

fn workspace_test_scope(temporary_directory: &Path) -> ProfileScope {
    ProfileScope::Workspace {
        workspace_config_root: temporary_directory.join("workspace").join(YLVA_DIR_NAME),
    }
}

fn list_command(workspace: bool) -> ProfileCommand {
    ProfileCommand::List { workspace }
}

fn create_command(name: &str, workspace: bool, force: bool) -> ProfileCommand {
    ProfileCommand::Create {
        name: name.to_string(),
        workspace,
        force,
        shim_name: None,
    }
}

fn create_command_with_shim_name(name: &str, shim_name: &str) -> ProfileCommand {
    ProfileCommand::Create {
        name: name.to_string(),
        workspace: false,
        force: false,
        shim_name: Some(shim_name.to_string()),
    }
}

fn shims_command(workspace: bool) -> ProfileCommand {
    ProfileCommand::Shims { workspace }
}

fn delete_command(name: &str, workspace: bool, dry_run: bool) -> ProfileCommand {
    ProfileCommand::Delete {
        name: name.to_string(),
        workspace,
        dry_run,
    }
}

fn edit_command(name: &str, workspace: bool) -> ProfileCommand {
    ProfileCommand::Edit {
        name: name.to_string(),
        workspace,
    }
}

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent");
    std::fs::write(path, contents).expect("write file");
}

fn profile_directory(scope: &ProfileScope, name: &str) -> PathBuf {
    scope.profiles_root().join(name)
}

#[test]
fn create_seeds_the_same_starters_as_init() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&create_command("work", false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    let init_directory = tempfile::tempdir().expect("init tempdir");
    run_init(init_directory.path(), false).expect("init succeeds");
    let init_profile = init_directory
        .path()
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME);
    let created_profile = profile_directory(&scope, "work");
    for file_name in [PROFILE_CONFIG_FILE_NAME, PROFILE_ENV_FILE_NAME] {
        assert_eq!(
            std::fs::read_to_string(created_profile.join(file_name)).expect("created starter"),
            std::fs::read_to_string(init_profile.join(file_name)).expect("init starter"),
            "starter {file_name} must match init"
        );
    }
    assert!(!created_profile.join("agents").exists());
}

#[test]
fn create_refuses_existing_starter_without_force() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let config_path = profile_directory(&scope, "work").join(PROFILE_CONFIG_FILE_NAME);
    write_file(&config_path, "theme = \"light\"\n");

    let outcome = execute_quietly(&create_command("work", false, false), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("--force"));
    assert!(outcome
        .diagnostics
        .contains("profile \"work\" already has config.toml or .env"));
    assert_eq!(
        std::fs::read_to_string(config_path).expect("config unchanged"),
        "theme = \"light\"\n"
    );
}

#[test]
fn force_overwrites_only_the_starter_files_and_keeps_agents() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let profile = profile_directory(&scope, "work");
    write_file(
        &profile.join(PROFILE_CONFIG_FILE_NAME),
        "theme = \"custom\"\n",
    );
    write_file(&profile.join(PROFILE_ENV_FILE_NAME), "CUSTOM=1\n");
    write_file(&profile.join("agents").join("reviewer.md"), "keep me");

    let outcome = execute_quietly(&create_command("work", false, true), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert_ne!(
        std::fs::read_to_string(profile.join(PROFILE_CONFIG_FILE_NAME)).expect("config"),
        "theme = \"custom\"\n"
    );
    assert_ne!(
        std::fs::read_to_string(profile.join(PROFILE_ENV_FILE_NAME)).expect("env"),
        "CUSTOM=1\n"
    );
    assert_eq!(
        std::fs::read_to_string(profile.join("agents").join("reviewer.md"))
            .expect("agent file kept"),
        "keep me"
    );
}

#[test]
fn create_keeps_an_existing_agents_directory_when_starters_are_missing() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let profile = profile_directory(&scope, "work");
    write_file(&profile.join("agents").join("reviewer.md"), "keep me");

    let outcome = execute_quietly(&create_command("work", false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(profile.join(PROFILE_CONFIG_FILE_NAME).is_file());
    assert!(profile.join("agents").join("reviewer.md").is_file());
}

#[test]
fn create_refuses_unreadable_config_even_with_force() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let profile = profile_directory(&scope, "work");
    std::fs::create_dir_all(profile.join(PROFILE_CONFIG_FILE_NAME)).expect("config as directory");

    let outcome = execute_quietly(&create_command("work", false, true), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("cannot read"));
    assert!(!profile.join(PROFILE_ENV_FILE_NAME).exists());
}

#[test]
fn create_refuses_invalid_toml_config_even_with_force() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let profile = profile_directory(&scope, "work");
    write_file(&profile.join(PROFILE_CONFIG_FILE_NAME), "key = ");

    let outcome = execute_quietly(&create_command("work", false, true), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("not valid TOML"));
    assert_eq!(
        std::fs::read_to_string(profile.join(PROFILE_CONFIG_FILE_NAME)).expect("config"),
        "key = "
    );
    assert!(!profile.join(PROFILE_ENV_FILE_NAME).exists());
}

#[test]
fn create_rejects_an_invalid_name_without_writing() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&create_command("../escape", false, false), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("invalid profile name"));
    assert!(!temporary_directory.path().join("home").exists());
}

#[test]
fn workspace_create_seeds_missing_gitignore_and_appends_to_existing_one() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = workspace_test_scope(temporary_directory.path());
    let gitignore = match &scope {
        ProfileScope::Workspace {
            workspace_config_root,
        } => workspace_config_root.join(".gitignore"),
        ProfileScope::Home { .. } => unreachable!("workspace scope"),
    };

    let first = execute_quietly(&create_command("work", true, false), &scope);
    assert_eq!(first.code, SUCCESS);
    assert!(std::fs::read_to_string(&gitignore)
        .expect("seeded gitignore")
        .contains(".env"));

    write_file(&gitignore, "custom\n");
    let second = execute_quietly(&create_command("other", true, true), &scope);
    assert_eq!(second.code, SUCCESS);
    assert_eq!(
        std::fs::read_to_string(&gitignore).expect("gitignore appended"),
        "custom\n.env\n.envrc\nchats/\n"
    );
}

#[test]
fn delete_removes_the_profile_tree() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    write_file(
        &profile_directory(&scope, "work")
            .join("agents")
            .join("a.md"),
        "x",
    );

    let outcome = execute_quietly(&delete_command("work", false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(!profile_directory(&scope, "work").exists());
}

#[test]
fn delete_dry_run_reports_and_keeps_the_profile() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);

    let outcome = execute_quietly(&delete_command("work", false, true), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert_eq!(
        outcome.output,
        format!(
            "would delete \"work\" at {}\n",
            display_path(&profile_directory(&scope, "work"))
        )
    );
    assert!(profile_directory(&scope, "work")
        .join(PROFILE_CONFIG_FILE_NAME)
        .is_file());
}

#[test]
fn delete_of_a_missing_profile_exits_two() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&delete_command("missing", false, false), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome
        .diagnostics
        .contains("profile \"missing\" does not exist"));
}

#[test]
fn delete_refuses_invalid_target_config_even_in_dry_run() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    let profile = profile_directory(&scope, "work");
    write_file(&profile.join(PROFILE_CONFIG_FILE_NAME), "key = ");
    write_file(&profile.join("agents").join("a.md"), "x");

    let dry_run = execute_quietly(&delete_command("work", false, true), &scope);
    let real_run = execute_quietly(&delete_command("work", false, false), &scope);

    assert_eq!(dry_run.code, USAGE_ERROR);
    assert_eq!(real_run.code, USAGE_ERROR);
    assert!(profile.join("agents").join("a.md").is_file());
}

#[test]
fn delete_default_is_allowed() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command(DEFAULT_PROFILE_NAME, false, false), &scope);

    let outcome = execute_quietly(&delete_command(DEFAULT_PROFILE_NAME, false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(!profile_directory(&scope, DEFAULT_PROFILE_NAME).exists());
}

#[test]
fn list_sorts_profiles_and_marks_problems_with_exit_zero() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("beta", false, false), &scope);
    std::fs::create_dir_all(profile_directory(&scope, "alpha")).expect("no config.toml");
    std::fs::create_dir_all(profile_directory(&scope, "bad.name")).expect("invalid name");
    write_file(
        &profile_directory(&scope, "broken").join(PROFILE_CONFIG_FILE_NAME),
        "key = ",
    );

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    let lines: Vec<&str> = outcome.output.lines().collect();
    assert_eq!(lines.len(), 4, "output: {}", outcome.output);
    assert!(lines[0].starts_with("alpha  ") && lines[0].ends_with("(no config.toml)"));
    assert!(lines[1].starts_with("bad.name  ") && lines[1].ends_with("(invalid name)"));
    assert!(lines[2].starts_with("beta  ") && !lines[2].ends_with(')'));
    assert!(lines[3].starts_with("broken  ") && lines[3].ends_with("(invalid config)"));
}

#[test]
fn list_reports_an_empty_or_missing_root_with_exit_zero() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(outcome.output.starts_with("No profiles in "));
}

#[test]
fn list_aligns_name_and_directory_columns_without_a_header() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    std::fs::create_dir_all(profile_directory(&scope, "a")).expect("no config.toml");
    std::fs::create_dir_all(profile_directory(&scope, "bbb")).expect("no config.toml");
    execute_quietly(&create_command("ccc", false, false), &scope);

    let outcome = execute_quietly(&list_command(false), &scope);

    let lines: Vec<&str> = outcome.output.lines().collect();
    assert_eq!(lines.len(), 3, "output: {}", outcome.output);
    assert!(lines[0].starts_with("a    "), "output: {}", outcome.output);
    assert!(lines[1].starts_with("bbb  "), "output: {}", outcome.output);
    assert!(lines[2].starts_with("ccc  "), "output: {}", outcome.output);
    let marks_column = |line: &str| line.find("(no config.toml)").expect("marks column");
    assert_eq!(marks_column(lines[0]), marks_column(lines[1]));
    assert!(
        lines.iter().all(|line| !line.ends_with(' ')),
        "output: {}",
        outcome.output
    );
}

#[test]
fn list_in_workspace_scope_shows_only_workspace_profiles() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let home = home_test_scope(temporary_directory.path());
    let workspace = workspace_test_scope(temporary_directory.path());
    execute_quietly(&create_command("home-only", false, false), &home);
    execute_quietly(&create_command("work", true, false), &workspace);

    let outcome = execute_quietly(&list_command(true), &workspace);

    assert_eq!(outcome.code, SUCCESS);
    assert!(outcome.output.contains("work  "));
    assert!(!outcome.output.contains("home-only"));
}

#[test]
fn merged_list_marks_home_profiles_shadowed_by_the_workspace() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let home = home_test_scope(temporary_directory.path());
    let workspace = workspace_test_scope(temporary_directory.path());
    execute_quietly(&create_command("shared", false, false), &home);
    execute_quietly(&create_command("home-only", false, false), &home);
    execute_quietly(&create_command("shared", true, false), &workspace);
    execute_quietly(&create_command("work", true, false), &workspace);
    let mut output = Vec::new();

    let code = list(&[workspace.clone(), home.clone()], &mut output).expect("list");

    let output = String::from_utf8(output).expect("utf-8 output");
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(code, SUCCESS);
    assert_eq!(lines.len(), 5, "output: {output}");
    assert!(lines[0].starts_with("shared  ") && !lines[0].contains("shadowed"));
    assert!(lines[0].contains(&display_path(&workspace.profiles_root().join("shared"))));
    assert!(lines[1].starts_with("work  ") && !lines[1].contains("shadowed"));
    assert!(
        lines[2].is_empty(),
        "scopes are separated by a blank line: {output}"
    );
    assert!(lines[3].starts_with("home-only  ") && !lines[3].contains("shadowed"));
    assert!(lines[4].starts_with("shared  ") && lines[4].ends_with("(shadowed by workspace)"));
    assert!(lines[4].contains(&display_path(&home.profiles_root().join("shared"))));
}

#[test]
fn merged_list_shows_the_home_profiles_when_no_workspace_is_found() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let home = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &home);
    let mut output = Vec::new();

    let code = list(std::slice::from_ref(&home), &mut output).expect("list");

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, SUCCESS);
    assert!(output.starts_with("work  ") && !output.contains("shadowed"));
}

#[test]
fn merged_list_reports_each_empty_scope() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let home = home_test_scope(temporary_directory.path());
    let workspace = workspace_test_scope(temporary_directory.path());
    let mut output = Vec::new();

    let code = list(&[workspace.clone(), home.clone()], &mut output).expect("list");

    let output = String::from_utf8(output).expect("utf-8 output");
    assert_eq!(code, SUCCESS);
    assert_eq!(output.lines().count(), 2, "output: {output}");
    assert!(output.contains(&display_path(&workspace.profiles_root())));
    assert!(output.contains(&display_path(&home.profiles_root())));
}

#[test]
fn merge_list_scopes_lists_the_workspace_before_the_home() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let home = home_test_scope(temporary_directory.path());
    let workspace = workspace_test_scope(temporary_directory.path());

    assert_eq!(
        merge_list_scopes(Some(workspace.clone()), Some(home.clone())),
        vec![workspace, home.clone()]
    );
    assert_eq!(merge_list_scopes(None, Some(home.clone())), vec![home]);
}

#[test]
fn merge_list_scopes_skips_a_home_that_reads_the_workspace_profiles() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let workspace = workspace_test_scope(temporary_directory.path());
    let shim_home = ProfileScope::Home {
        ylva_home: temporary_directory
            .path()
            .join("workspace")
            .join(YLVA_DIR_NAME),
    };

    assert_eq!(
        merge_list_scopes(Some(workspace.clone()), Some(shim_home)),
        vec![workspace]
    );
}

#[test]
fn workspace_scope_is_absent_without_a_workspace_root() {
    assert_eq!(workspace_scope(None), None);
    let root = PathBuf::from("/project");
    assert_eq!(
        workspace_scope(Some(root.clone())),
        Some(ProfileScope::Workspace {
            workspace_config_root: root.join(YLVA_DIR_NAME),
        })
    );
}

#[test]
fn unreadable_profiles_root_exits_two() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let ylva_home = temporary_directory.path().join("home");
    write_file(&ylva_home.join(PROFILES_DIRECTORY_NAME), "not a directory");
    let scope = ProfileScope::Home { ylva_home };

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(!outcome.diagnostics.is_empty());
}

#[test]
fn visual_takes_precedence_over_editor() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let launcher = RecordingLauncher::succeeding();
    let editor = EditorSettings {
        visual: Some("vim".to_string()),
        editor: Some("nano".to_string()),
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, SUCCESS);
    assert_eq!(launcher.launched_commands(), vec!["vim".to_string()]);
    let (_, launched_path) = launcher.launches.borrow()[0].clone();
    assert_eq!(
        launched_path,
        profile_directory(&scope, "work").join(PROFILE_CONFIG_FILE_NAME)
    );
}

#[test]
fn editor_is_used_when_visual_is_unset() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let launcher = RecordingLauncher::succeeding();
    let editor = EditorSettings {
        visual: None,
        editor: Some("nano".to_string()),
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, SUCCESS);
    assert_eq!(launcher.launched_commands(), vec!["nano".to_string()]);
}

#[test]
fn blank_visual_falls_back_to_editor() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let launcher = RecordingLauncher::succeeding();
    let editor = EditorSettings {
        visual: Some("   ".to_string()),
        editor: Some("nano".to_string()),
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, SUCCESS);
    assert_eq!(launcher.launched_commands(), vec!["nano".to_string()]);
}

#[test]
fn edit_without_any_editor_exits_two_and_names_both_variables() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let launcher = RecordingLauncher::succeeding();

    let outcome = execute_with_editor(
        &edit_command("work", false),
        &scope,
        &EditorSettings::default(),
        &launcher,
    );

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("VISUAL"));
    assert!(outcome.diagnostics.contains("EDITOR"));
    assert!(launcher.launched_commands().is_empty());
}

#[test]
fn edit_without_config_exits_two_with_create_hint() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    std::fs::create_dir_all(profile_directory(&scope, "work")).expect("profile directory");
    let launcher = RecordingLauncher::succeeding();
    let editor = EditorSettings {
        visual: Some("vim".to_string()),
        editor: None,
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("ylva profile create work"));
    assert!(outcome
        .diagnostics
        .contains("profile \"work\" has no config.toml"));
    assert!(launcher.launched_commands().is_empty());
}

#[test]
fn edit_with_failing_editor_exits_one() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let launcher = RecordingLauncher::failing();
    let editor = EditorSettings {
        visual: Some("vim".to_string()),
        editor: None,
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, GENERAL_ERROR);
    assert!(outcome.diagnostics.contains("non-zero"));
}

#[test]
fn edit_with_unstartable_editor_exits_two() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let editor = EditorSettings {
        visual: Some("vim".to_string()),
        editor: None,
    };

    let outcome = execute_with_editor(
        &edit_command("work", false),
        &scope,
        &editor,
        &UnstartableLauncher,
    );

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("cannot start editor"));
}

fn shims_directory(scope: &ProfileScope) -> PathBuf {
    scope.shim_directories().shims_directory
}

#[test]
fn create_writes_a_shim_named_after_the_profile() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&create_command("work", false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    let shim_path = shims_directory(&scope).join("ylva-work");
    assert!(shim_path.is_file());
    assert!(outcome
        .output
        .contains(&format!("Shim {}", shim_path.display())));
    assert!(outcome
        .output
        .contains("# Update your PATH to use shims:\n# export PATH="));
    let script = std::fs::read_to_string(&shim_path).expect("shim is readable");
    assert!(script.contains("exec ylva --profile 'work' \"$@\""));
}

#[test]
fn create_prints_the_path_hint_after_the_shim_line() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&create_command("work", false, false), &scope);

    let shim_line = format!(
        "Shim {}",
        shims_directory(&scope).join("ylva-work").display()
    );
    let hint_start = outcome
        .output
        .find("# Update your PATH to use shims:")
        .expect("create prints the PATH hint");
    assert!(outcome.output.find(&shim_line).expect("shim line") < hint_start);
    assert!(outcome.output.contains("Created profile \"work\" at "));
    assert!(outcome.output.contains("is not on PATH"));
}

#[test]
fn create_with_shim_name_names_the_shim_after_it() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&create_command_with_shim_name("work", "office"), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(shims_directory(&scope).join("office").is_file());
    assert!(!shims_directory(&scope).join("ylva-work").exists());
    let config =
        std::fs::read_to_string(profile_directory(&scope, "work").join(PROFILE_CONFIG_FILE_NAME))
            .expect("config is readable");
    assert!(config.starts_with("shim_name = \"office\"\n"));
}

#[test]
fn create_with_a_claimed_shim_name_exits_two_and_writes_nothing() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command_with_shim_name("other", "office"), &scope);

    let outcome = execute_quietly(&create_command_with_shim_name("work", "office"), &scope);

    assert_eq!(outcome.code, USAGE_ERROR);
    assert!(outcome.diagnostics.contains("other"));
    assert!(outcome.diagnostics.contains("work"));
    assert!(!profile_directory(&scope, "work").exists());
}

#[test]
fn delete_removes_the_shim_of_the_deleted_profile() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let shim_path = shims_directory(&scope).join("ylva-work");
    assert!(shim_path.is_file());

    let outcome = execute_quietly(&delete_command("work", false, false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(!shim_path.exists());
    assert!(outcome
        .output
        .contains(&format!("Removed {}", shim_path.display())));
}

#[test]
fn delete_dry_run_keeps_the_shim() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);

    execute_quietly(&delete_command("work", false, true), &scope);

    assert!(shims_directory(&scope).join("ylva-work").is_file());
}

#[test]
fn edit_reconciles_a_renamed_shim_name_after_the_editor_exits() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let config_path = profile_directory(&scope, "work").join(PROFILE_CONFIG_FILE_NAME);
    let starter = std::fs::read_to_string(&config_path).expect("config is readable");
    write_file(
        &config_path,
        &config_text_with_shim_name(&starter, "office"),
    );

    let editor = EditorSettings {
        visual: Some("true".to_string()),
        editor: None,
    };

    let outcome = execute_with_editor(
        &edit_command("work", false),
        &scope,
        &editor,
        &RecordingLauncher::succeeding(),
    );

    assert_eq!(outcome.code, SUCCESS);
    assert!(shims_directory(&scope).join("office").is_file());
    assert!(!shims_directory(&scope).join("ylva-work").exists());
    assert!(outcome.output.contains("Removed "));
}

#[test]
fn edit_with_failing_editor_leaves_the_shims_untouched() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let config_path = profile_directory(&scope, "work").join(PROFILE_CONFIG_FILE_NAME);
    let starter = std::fs::read_to_string(&config_path).expect("config is readable");
    write_file(
        &config_path,
        &config_text_with_shim_name(&starter, "office"),
    );
    let launcher = RecordingLauncher::failing();
    let editor = EditorSettings {
        visual: Some("vim".to_string()),
        editor: None,
    };

    let outcome = execute_with_editor(&edit_command("work", false), &scope, &editor, &launcher);

    assert_eq!(outcome.code, GENERAL_ERROR);
    assert!(shims_directory(&scope).join("ylva-work").is_file());
    assert!(!shims_directory(&scope).join("office").exists());
}

#[test]
fn list_never_reconciles_shims() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    write_file(&shims_directory(&scope).join("stray"), "x");

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(shims_directory(&scope).join("stray").is_file());
}

#[test]
fn shims_lists_each_shim_and_prints_the_path_hint() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let shim_path = shims_directory(&scope).join("ylva-work");
    let environment = ProfileEnvironment::default();

    let outcome = execute_with_environment(
        &shims_command(false),
        &scope,
        &environment,
        &RecordingLauncher::succeeding(),
    );

    assert_eq!(outcome.code, SUCCESS);
    assert!(outcome
        .output
        .starts_with(&format!("work  {}\n", shim_path.display())));
    assert!(outcome.output.contains(&format!(
        "# Update your PATH to use shims:\n# export PATH=\"{}:$PATH\"\n# {} is not on PATH\n",
        shims_directory(&scope).display(),
        shims_directory(&scope).display()
    )));
}

#[test]
fn shims_reports_an_empty_directory_without_a_path_hint() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());

    let outcome = execute_quietly(&shims_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(outcome.output.starts_with("No shims in "));
    assert!(!outcome.output.contains("PATH"));
}

#[test]
fn shims_prunes_unclaimed_files_and_reports_them() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let stray_path = shims_directory(&scope).join("stray");
    write_file(&stray_path, "x");

    let outcome = execute_quietly(&shims_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(!stray_path.exists());
    assert!(outcome
        .output
        .contains(&format!("Removed {}", stray_path.display())));
}

#[test]
fn shims_reports_a_directory_already_on_path() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);
    let path_variable = std::env::join_paths([shims_directory(&scope)]).expect("join paths");
    let environment = ProfileEnvironment {
        editor: EditorSettings::default(),
        path_variable: Some(path_variable),
    };

    let outcome = execute_with_environment(
        &shims_command(false),
        &scope,
        &environment,
        &RecordingLauncher::succeeding(),
    );

    assert_eq!(outcome.code, SUCCESS);
    assert!(outcome.output.contains(&format!(
        "# Update your PATH to use shims:\n# export PATH=\"{}:$PATH\"\n# {} is already on PATH\n",
        shims_directory(&scope).display(),
        shims_directory(&scope).display()
    )));
}

#[test]
fn list_marks_default_as_the_implicit_default() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command(DEFAULT_PROFILE_NAME, false, false), &scope);
    execute_quietly(&create_command("work", false, false), &scope);

    let outcome = execute_quietly(&list_command(false), &scope);

    let lines: Vec<&str> = outcome.output.lines().collect();
    assert!(lines[0].starts_with("default  ") && lines[0].ends_with("(implicit default)"));
    assert!(lines[1].starts_with("work  ") && !lines[1].contains("implicit"));
}

#[test]
fn list_marks_default_in_workspace_scope() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let workspace = workspace_test_scope(temporary_directory.path());
    execute_quietly(
        &create_command(DEFAULT_PROFILE_NAME, true, false),
        &workspace,
    );

    let outcome = execute_quietly(&list_command(true), &workspace);

    assert_eq!(outcome.code, SUCCESS);
    assert!(
        outcome.output.starts_with("default  ") && outcome.output.contains("(implicit default)")
    );
}

#[test]
fn list_has_no_implicit_default_mark_without_a_default_directory() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    execute_quietly(&create_command("work", false, false), &scope);

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(
        !outcome.output.contains("implicit default"),
        "output: {}",
        outcome.output
    );
}

#[test]
fn list_marks_a_default_without_config_as_implicit_and_missing_config() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let scope = home_test_scope(temporary_directory.path());
    std::fs::create_dir_all(profile_directory(&scope, DEFAULT_PROFILE_NAME))
        .expect("default directory without config.toml");

    let outcome = execute_quietly(&list_command(false), &scope);

    assert_eq!(outcome.code, SUCCESS);
    assert!(
        outcome
            .output
            .trim_end()
            .ends_with("(implicit default) (no config.toml)"),
        "output: {}",
        outcome.output
    );
}

#[test]
fn shell_launcher_passes_the_path_as_an_argument() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let file_path = temporary_directory.path().join("config.toml");
    write_file(&file_path, "");

    let succeeded = ShellEditorLauncher
        .launch("test -f", &file_path)
        .expect("sh starts");
    let failed = ShellEditorLauncher
        .launch("test -d", &file_path)
        .expect("sh starts");

    assert!(succeeded);
    assert!(!failed);
}
