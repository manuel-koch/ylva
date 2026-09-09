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

//! `--profile <name>` selects a specific profile explicitly, so a missing
//! `config.toml` for it is a hard error, never a silent fallback to the
//! in-code default profile. This is spawned-binary-level behavior (argument
//! parsing plus `ProfileConfig::load`'s error path wired to `main`'s exit
//! code), so it is exercised here rather than as a `src/config.rs` unit test.

use std::fs;
use std::process::Command;

use tempfile::TempDir;

/// An explicitly named profile with no config file on disk exits non-zero
/// and names the profile in the error, instead of silently running with the
/// in-code default profile.
#[test]
fn an_explicitly_named_missing_profile_is_a_hard_error() {
    let ylva_home = TempDir::new().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi", "--profile", "never-written"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an unknown --profile must not exit zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("never-written"),
        "the error must name the missing profile: {stderr}"
    );
    assert!(
        stderr.contains("not found"),
        "the error must say the profile was not found: {stderr}"
    );
}

/// Profile validation errors identify the selected profile and its config
/// file, rather than printing only the validation detail.
#[test]
fn a_profile_validation_error_names_the_profile_config_file() {
    let ylva_home = TempDir::new().expect("tempdir");
    let profile_name = "broken";
    let profile_directory = ylva_home.path().join("profiles").join(profile_name);
    fs::create_dir_all(&profile_directory).expect("create profile directory");
    fs::write(
        profile_directory.join("config.toml"),
        r#"
default_engine = "missing"

[engines.available]
provider = "mock"
model = "test-model"
"#,
    )
    .expect("write profile config");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi", "--profile", profile_name])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an invalid profile must not start successfully"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(profile_name),
        "the error must name the selected profile: {stderr}"
    );
    assert!(
        stderr.contains(
            profile_directory
                .join("config.toml")
                .to_str()
                .expect("profile path is valid UTF-8")
        ),
        "the error must name the full profile config path: {stderr}"
    );
    assert!(
        stderr.contains("references engine 'missing'"),
        "the error must retain the validation detail: {stderr}"
    );
}

/// With no `--profile` flag at all, a fresh `YLVA_HOME` (so no config file
/// exists anywhere) still runs successfully against the in-code default
/// profile — the implicit case stays tolerant.
#[test]
fn no_profile_flag_tolerates_a_missing_default_profile_config() {
    let ylva_home = TempDir::new().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        output.status.success(),
        "the implicit default profile must not fail just because no config file exists yet; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A Ylva home whose chats root is not usable exits non-zero instead of
/// starting a transient session without persistence.
#[test]
fn an_unusable_chats_root_is_a_hard_error() {
    let ylva_home = TempDir::new().expect("tempdir");
    fs::write(ylva_home.path().join("chats"), b"not a directory").expect("create chats file");

    let output = Command::new(env!("CARGO_BIN_EXE_ylva"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, ylva_home.path())
        .current_dir(ylva_home.path())
        .args(["--no-tui", "--prompt", "hi"])
        .output()
        .expect("spawn the ylva binary");

    assert!(
        !output.status.success(),
        "an unusable chats root must not start a transient session"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("store I/O error"),
        "the error must identify the persistence failure: {stderr}"
    );
}
