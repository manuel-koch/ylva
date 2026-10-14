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

//! Spawned-binary coverage of profile shims: the generated home and workspace
//! scripts, argument forwarding, `profile shims`, reconciling on `edit`, and the
//! refusals that leave every file untouched. Unix only, because shims are.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A profile config whose only problem is an engine that does not exist. Loading
/// it fails at startup and names the profile config path, so a test can tell that
/// the profile was resolved, with no network call.
const UNRESOLVABLE_ENGINE_CONFIG: &str = r#"default_engine = "missing"

[engines.available]
provider = "mock"
model = "test-model"
"#;

/// A temporary sandbox: a `YLVA_HOME`, a fake user home, a workspace, a
/// directory without a workspace marker, and a `bin` directory holding `ylva`.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("sandbox tempdir");
        for directory in ["ylva-home", "user-home", "scratch", "workspace/.git", "bin"] {
            fs::create_dir_all(root.path().join(directory)).expect("create sandbox directory");
        }
        symlink(
            env!("CARGO_BIN_EXE_ylva"),
            root.path().join("bin").join("ylva"),
        )
        .expect("link ylva into the bin directory");
        Self { root }
    }

    fn ylva_home(&self) -> PathBuf {
        self.root.path().join("ylva-home")
    }

    fn user_home(&self) -> PathBuf {
        self.root.path().join("user-home")
    }

    fn scratch(&self) -> PathBuf {
        self.root.path().join("scratch")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn bin_directory(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn home_shims(&self) -> PathBuf {
        self.ylva_home().join("shims")
    }

    fn workspace_shims(&self) -> PathBuf {
        self.workspace().join(ylva::YLVA_DIR_NAME).join("shims")
    }

    fn home_profile_config(&self, name: &str) -> PathBuf {
        self.ylva_home()
            .join("profiles")
            .join(name)
            .join("config.toml")
    }

    fn workspace_profile_config(&self, name: &str) -> PathBuf {
        self.workspace()
            .join(ylva::YLVA_DIR_NAME)
            .join("profiles")
            .join(name)
            .join("config.toml")
    }

    /// Run `ylva` directly with `YLVA_HOME` set and the given working directory.
    fn ylva(
        &self,
        working_directory: &Path,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, self.ylva_home())
            .env("HOME", self.user_home())
            .current_dir(working_directory)
            .args(arguments);
        for (name, value) in environment {
            command.env(name, value);
        }
        command.output().expect("spawn the ylva binary")
    }

    /// Run `ylva` directly from a shell without `YLVA_HOME`, so it uses `~/.ylva`.
    fn ylva_without_ylva_home(&self, working_directory: &Path, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ylva"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.user_home())
            .current_dir(working_directory)
            .args(arguments)
            .output()
            .expect("spawn the ylva binary")
    }

    /// Run `ylva` from the scratch directory, which has no workspace marker.
    fn home(&self, arguments: &[&str]) -> Output {
        self.ylva(&self.scratch(), arguments, &[])
    }

    /// Run `ylva` from inside the workspace.
    fn in_workspace(&self, arguments: &[&str]) -> Output {
        self.ylva(&self.workspace(), arguments, &[])
    }

    /// Run a generated shim from a shell that exports `YLVA_HOME` as the sandbox Ylva home.
    /// `PATH` holds only the `bin` directory and the system tools the script needs.
    fn run_shim(&self, shim_path: &Path, working_directory: &Path, arguments: &[&str]) -> Output {
        self.spawn_shim(
            shim_path,
            working_directory,
            arguments,
            Some(&self.ylva_home()),
        )
    }

    /// Run a generated shim from a shell without `YLVA_HOME`, the same environment as a
    /// plain `ylva` run in that shell.
    fn run_shim_without_ylva_home(
        &self,
        shim_path: &Path,
        working_directory: &Path,
        arguments: &[&str],
    ) -> Output {
        self.spawn_shim(shim_path, working_directory, arguments, None)
    }

    fn spawn_shim(
        &self,
        shim_path: &Path,
        working_directory: &Path,
        arguments: &[&str],
        ylva_home: Option<&Path>,
    ) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.bin_directory().display());
        let mut command = Command::new(shim_path);
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.user_home())
            .current_dir(working_directory)
            .args(arguments);
        if let Some(ylva_home) = ylva_home {
            command.env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home);
        }
        command.output().expect("run the shim")
    }
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn overwrite_config(config_path: &Path, text: &str) {
    fs::write(config_path, text).expect("overwrite profile config");
}

#[test]
fn home_shim_selects_its_profile_from_the_callers_ylva_home() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);
    overwrite_config(
        &sandbox.home_profile_config("work"),
        UNRESOLVABLE_ENGINE_CONFIG,
    );

    let output = sandbox.run_shim(
        &sandbox.home_shims().join("ylva-work"),
        &sandbox.scratch(),
        &["--no-tui", "--prompt", "hi"],
    );

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("references engine 'missing'"),
        "the shim must select the work profile from its YLVA_HOME: {stderr}"
    );
    assert!(
        stderr.contains("profiles/work/config.toml"),
        "the error must name the work profile config: {stderr}"
    );
}

#[test]
fn home_shim_forwards_a_quoted_argument_as_one_word() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);

    let output = sandbox.run_shim(
        &sandbox.home_shims().join("ylva-work"),
        &sandbox.scratch(),
        &["profile", "create", "my team"],
    );

    assert_eq!(
        exit_code(&output),
        2,
        "the invalid name must refuse: {output:?}"
    );
    assert!(
        stderr_of(&output).contains("invalid value 'my team'"),
        "the quoted argument must arrive as one word: {}",
        stderr_of(&output)
    );
}

#[test]
fn home_shim_rejects_a_second_profile_flag_in_forwarded_arguments() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);

    let output = sandbox.run_shim(
        &sandbox.home_shims().join("ylva-work"),
        &sandbox.scratch(),
        &["--profile", "other", "profile", "list"],
    );

    assert_eq!(
        exit_code(&output),
        2,
        "a repeated --profile must exit 2: {}",
        stderr_of(&output)
    );
}

/// `session`, `profile`, and `init` return before profile loading, so this checks
/// that a shim forwards arguments and runs in the caller's `YLVA_HOME`, not profile
/// selection.
#[test]
fn session_list_and_init_run_through_a_home_shim() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);
    let shim = sandbox.home_shims().join("ylva-work");

    let listed = sandbox.run_shim(&shim, &sandbox.scratch(), &["session", "list"]);
    assert_eq!(
        exit_code(&listed),
        0,
        "session list: {}",
        stderr_of(&listed)
    );

    let initialized = sandbox.run_shim(&shim, &sandbox.scratch(), &["init"]);
    assert_eq!(
        exit_code(&initialized),
        0,
        "init: {}",
        stderr_of(&initialized)
    );
    assert!(
        sandbox
            .ylva_home()
            .join("profiles")
            .join("default")
            .join("config.toml")
            .is_file(),
        "init through the shim must write into YLVA_HOME"
    );
}

#[test]
fn workspace_shim_changes_to_the_workspace_root_before_running_ylva() {
    let sandbox = Sandbox::new();
    assert_eq!(
        exit_code(&sandbox.in_workspace(&["profile", "create", "--workspace", "team"])),
        0
    );
    overwrite_config(
        &sandbox.workspace_profile_config("team"),
        UNRESOLVABLE_ENGINE_CONFIG,
    );

    let output = sandbox.run_shim(
        &sandbox.workspace_shims().join("ylva-team"),
        &sandbox.scratch(),
        &["--no-tui", "--prompt", "hi"],
    );

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("references engine 'missing'"),
        "the workspace profile is only found after the shim cds to the workspace: {stderr}"
    );
}

/// Reads the `Ylva home:` value from `ylva --version` output.
fn reported_ylva_home(output: &Output) -> String {
    stdout_of(output)
        .lines()
        .find_map(|line| line.strip_prefix("Ylva home:"))
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| panic!("no Ylva home line in: {}", stdout_of(output)))
}

/// A home shim copied into a workspace's `.ylva/shims` must use the same Ylva home as
/// plain `ylva` in the same shell. The shim no longer derives `YLVA_HOME` from its
/// own location, so chats cannot move into the workspace.
#[test]
fn home_shim_in_a_workspace_uses_the_same_ylva_home_as_plain_ylva() {
    let sandbox = Sandbox::new();
    assert_eq!(
        exit_code(
            &sandbox.ylva_without_ylva_home(&sandbox.scratch(), &["profile", "create", "work"])
        ),
        0
    );
    let workspace_shims = sandbox.workspace_shims();
    fs::create_dir_all(&workspace_shims).expect("create the workspace shims directory");
    fs::copy(
        sandbox.user_home().join(".ylva/shims/ylva-work"),
        workspace_shims.join("ylva-work"),
    )
    .expect("copy the home shim into the workspace");

    let plain =
        sandbox.ylva_without_ylva_home(&sandbox.workspace(), &["--profile", "work", "--version"]);
    let shim = sandbox.run_shim_without_ylva_home(
        &workspace_shims.join("ylva-work"),
        &sandbox.scratch(),
        &["--version"],
    );

    assert_eq!(exit_code(&plain), 0, "plain ylva: {}", stderr_of(&plain));
    assert_eq!(exit_code(&shim), 0, "shim: {}", stderr_of(&shim));
    assert_eq!(
        reported_ylva_home(&shim),
        reported_ylva_home(&plain),
        "the shim must store chats where plain ylva does"
    );
    assert_eq!(reported_ylva_home(&shim), "~/.ylva");
}

/// The workspace `default` profile supplies config only. Chats still go to the
/// caller's Ylva home, the same as a run without a workspace profile.
#[test]
fn workspace_default_profile_keeps_chats_in_the_callers_ylva_home() {
    let sandbox = Sandbox::new();
    assert_eq!(
        exit_code(&sandbox.in_workspace(&["init", "--workspace"])),
        0
    );

    let without_ylva_home = sandbox.ylva_without_ylva_home(&sandbox.workspace(), &["--version"]);
    let with_ylva_home = sandbox.in_workspace(&["--version"]);

    assert_eq!(reported_ylva_home(&without_ylva_home), "~/.ylva");
    assert_eq!(
        reported_ylva_home(&with_ylva_home),
        sandbox.ylva_home().display().to_string()
    );
}

#[test]
fn custom_shim_name_survives_profile_shims_and_create_rejects_ylva() {
    let sandbox = Sandbox::new();
    let created = sandbox.home(&["profile", "create", "office-team", "--shim-name", "office"]);
    assert_eq!(exit_code(&created), 0, "create: {}", stderr_of(&created));
    assert!(
        stdout_of(&created).contains("# Update your PATH to use shims:"),
        "create must print the PATH hint for the new shim: {}",
        stdout_of(&created)
    );
    let custom_shim = sandbox.home_shims().join("office");
    assert!(custom_shim.is_file(), "--shim-name must name the shim file");
    assert!(!sandbox.home_shims().join("ylva-office-team").exists());

    let shims = sandbox.home(&["profile", "shims"]);

    assert_eq!(exit_code(&shims), 0, "profile shims: {}", stderr_of(&shims));
    assert!(
        custom_shim.is_file(),
        "profile shims must keep a custom shim"
    );
    let stdout = stdout_of(&shims);
    assert!(
        stdout.contains("office-team  ") && stdout.contains("/shims/office"),
        "shims must list the profile name before its shim path: {stdout}"
    );
    assert!(
        stdout.contains("# Update your PATH to use shims:\n# export PATH="),
        "shims must explain the PATH change and print the line as a comment: {stdout}"
    );
    assert!(
        stdout.contains("is not on PATH"),
        "shims must state whether the directory is on PATH: {stdout}"
    );

    let rejected = sandbox.home(&["profile", "create", "ylva-cli", "--shim-name", "ylva"]);
    assert_eq!(exit_code(&rejected), 2, "a shim named ylva must exit 2");
    assert!(
        !sandbox
            .ylva_home()
            .join("profiles")
            .join("ylva-cli")
            .exists(),
        "a rejected shim name must not create the profile"
    );
}

#[test]
fn edit_with_a_successful_editor_reconciles_the_shims() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);
    assert!(sandbox.home_shims().join("ylva-work").is_file());
    overwrite_config(
        &sandbox.home_profile_config("work"),
        "shim_name = \"desk\"\n",
    );

    let edited = sandbox.ylva(
        &sandbox.scratch(),
        &["profile", "edit", "work"],
        &[("EDITOR", "true")],
    );

    assert_eq!(exit_code(&edited), 0, "edit: {}", stderr_of(&edited));
    assert!(
        sandbox.home_shims().join("desk").is_file(),
        "edit must write the newly claimed shim"
    );
    assert!(
        !sandbox.home_shims().join("ylva-work").exists(),
        "edit must remove the shim the profile no longer claims"
    );
    assert!(
        stdout_of(&edited).contains("Removed "),
        "{}",
        stdout_of(&edited)
    );
}

#[test]
fn list_changes_no_shim_file() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);
    fs::remove_file(sandbox.home_shims().join("ylva-work")).expect("remove the work shim");
    let stray_path = sandbox.home_shims().join("stray");
    fs::write(&stray_path, "# not ours\n").expect("write a stray file");

    let listed = sandbox.home(&["profile", "list"]);

    assert_eq!(exit_code(&listed), 0, "list: {}", stderr_of(&listed));
    assert!(
        !sandbox.home_shims().join("ylva-work").exists(),
        "list must not write a missing shim"
    );
    assert!(stray_path.is_file(), "list must not prune a stray file");
}

#[test]
fn delete_refuses_while_any_in_scope_config_is_broken() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);
    assert_eq!(
        exit_code(&sandbox.home(&["profile", "create", "broken"])),
        0
    );
    overwrite_config(
        &sandbox.home_profile_config("broken"),
        "this is = = not toml\n",
    );
    let work_shim = sandbox.home_shims().join("ylva-work");

    for arguments in [
        vec!["profile", "delete", "work"],
        vec!["profile", "delete", "work", "--dry-run"],
        vec!["profile", "delete", "broken"],
    ] {
        let output = sandbox.home(&arguments);
        assert_eq!(
            exit_code(&output),
            2,
            "{arguments:?} must refuse: {}",
            stderr_of(&output)
        );
    }
    assert!(
        sandbox.ylva_home().join("profiles").join("work").is_dir(),
        "a refused delete must keep the work profile"
    );
    assert!(
        sandbox.ylva_home().join("profiles").join("broken").is_dir(),
        "a refused delete must keep the broken profile"
    );
    assert!(
        work_shim.is_file(),
        "a refused delete must keep the work shim"
    );
}

#[test]
fn delete_removes_the_profile_and_its_shim() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "work"])), 0);

    let deleted = sandbox.home(&["profile", "delete", "work"]);

    assert_eq!(exit_code(&deleted), 0, "delete: {}", stderr_of(&deleted));
    assert!(!sandbox.ylva_home().join("profiles").join("work").exists());
    assert!(
        !sandbox.home_shims().join("ylva-work").exists(),
        "a valid delete must leave no shim behind"
    );
}

#[test]
fn duplicate_shim_claims_exit_two_and_change_nothing() {
    let sandbox = Sandbox::new();
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "alpha"])), 0);
    assert_eq!(exit_code(&sandbox.home(&["profile", "create", "beta"])), 0);
    let alpha_shim = sandbox.home_shims().join("ylva-alpha");
    let beta_shim = sandbox.home_shims().join("ylva-beta");
    let beta_script_before = fs::read_to_string(&beta_shim).expect("read the beta shim");
    overwrite_config(
        &sandbox.home_profile_config("beta"),
        "shim_name = \"ylva-alpha\"\n",
    );

    let output = sandbox.home(&["profile", "shims"]);

    assert_eq!(exit_code(&output), 2, "duplicate claims must exit 2");
    assert!(
        stderr_of(&output).contains("both claim the shim ylva-alpha"),
        "{}",
        stderr_of(&output)
    );
    assert!(
        alpha_shim.is_file(),
        "a refused reconcile must keep the alpha shim"
    );
    assert_eq!(
        fs::read_to_string(&beta_shim).expect("beta shim kept"),
        beta_script_before,
        "a refused reconcile must not rewrite the beta shim"
    );
}

#[test]
fn config_with_shim_name_still_loads_as_a_profile() {
    let sandbox = Sandbox::new();
    assert_eq!(
        exit_code(&sandbox.home(&["profile", "create", "custom", "--shim-name", "office"])),
        0
    );
    overwrite_config(
        &sandbox.home_profile_config("custom"),
        &format!("shim_name = \"office\"\n{UNRESOLVABLE_ENGINE_CONFIG}"),
    );

    let output = sandbox.ylva(
        &sandbox.scratch(),
        &["--profile", "custom", "--no-tui", "--prompt", "hi"],
        &[],
    );

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("references engine 'missing'"),
        "shim_name must not stop the profile from loading: {stderr}"
    );
}
