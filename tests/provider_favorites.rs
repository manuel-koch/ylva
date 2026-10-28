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

//! Provider favorites and allowed models across the public configuration API
//! and the spawned binary: saving, reloading and unmarking favorites,
//! independent connections, allowed-model rejection at startup, and workspace
//! trust after a favorite was saved into a workspace profile.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;
use ylva::config::{FavoriteSourceLayer, ProfileConfig};

const TWO_CONNECTIONS: &str = r#"# kept comment
[provider_connections.alpha]
provider = "mock"
favorite_models = ["a/one"]
allowed_models = ["a/*"]

[provider_connections.beta]
provider = "mock"
favorite_models = ["b/one"]

[engines.main]
provider_connection = "alpha"
model = "a/one"
"#;

struct Fixture {
    home: TempDir,
    workspace: TempDir,
}

impl Fixture {
    fn with_workspace_profile(contents: &str) -> Self {
        let fixture = Fixture {
            home: TempDir::new().expect("home"),
            workspace: TempDir::new().expect("workspace"),
        };
        fs::create_dir_all(fixture.workspace_profile().parent().expect("parent"))
            .expect("workspace profile directory");
        fs::write(fixture.workspace_profile(), contents).expect("write profile");
        fixture
    }

    fn workspace_profile(&self) -> PathBuf {
        self.workspace
            .path()
            .join(".ylva/profiles/default/config.toml")
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ylva"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(ylva::YLVA_HOME_ENVIRONMENT_VARIABLE, self.home.path())
            .current_dir(self.workspace.path())
            .args(arguments)
            .output()
            .expect("spawn the ylva binary")
    }

    fn load(&self) -> (ProfileConfig, Option<ylva::config::FavoriteModelsWriter>) {
        ProfileConfig::load_with_favorite_writer(
            &self.home.path().join("profiles"),
            "default",
            &HashMap::new(),
            Some(self.workspace.path()),
            false,
        )
        .expect("load the profile")
    }
}

fn favorites_of(profile: &ProfileConfig, connection: &str) -> Vec<String> {
    profile.provider_connections[connection]
        .favorite_models
        .clone()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn trust_store_files(home: &Path) -> Vec<PathBuf> {
    fn collect(directory: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, found);
            } else {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    collect(home, &mut found);
    found.sort();
    found
}

#[test]
fn saved_favorites_survive_reload_and_unmarking_keeps_connections_independent() {
    let fixture = Fixture::with_workspace_profile(TWO_CONNECTIONS);
    let (profile, writer) = fixture.load();
    let mut writer = writer.expect("a workspace profile is writable");
    assert_eq!(favorites_of(&profile, "alpha"), vec!["a/one"]);

    let outcome = writer
        .set_favorite("alpha", "a/two", true)
        .expect("save the favorite");
    assert!(outcome.changed);
    assert_eq!(outcome.layer, FavoriteSourceLayer::Workspace);

    let (reloaded, _) = fixture.load();
    assert_eq!(favorites_of(&reloaded, "alpha"), vec!["a/one", "a/two"]);
    assert_eq!(favorites_of(&reloaded, "beta"), vec!["b/one"]);

    writer
        .set_favorite("alpha", "a/one", false)
        .expect("unmark");
    writer
        .set_favorite("alpha", "a/two", false)
        .expect("unmark");
    let (reloaded, _) = fixture.load();
    assert!(favorites_of(&reloaded, "alpha").is_empty());
    assert_eq!(favorites_of(&reloaded, "beta"), vec!["b/one"]);

    let raw = fs::read_to_string(fixture.workspace_profile()).expect("read profile");
    assert!(raw.contains("# kept comment"));
    assert!(raw.contains(r#"allowed_models = ["a/*"]"#));
    assert!(!raw.contains("a/two"));
}

#[test]
fn a_stale_source_is_rejected_instead_of_overwriting_an_external_edit() {
    let fixture = Fixture::with_workspace_profile(TWO_CONNECTIONS);
    let (_, writer) = fixture.load();
    let mut writer = writer.expect("writer");
    let edited = format!("{TWO_CONNECTIONS}\n# edited elsewhere\n");
    fs::write(fixture.workspace_profile(), &edited).expect("external edit");

    let error = writer
        .set_favorite("alpha", "a/two", true)
        .expect_err("stale source");

    assert!(
        error.to_string().to_lowercase().contains("restart"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace_profile()).expect("read"),
        edited
    );
}

#[test]
fn a_workspace_save_needs_explicit_trust_and_never_approves_itself() {
    let fixture = Fixture::with_workspace_profile(TWO_CONNECTIONS);
    let trust = fixture.run(&["trust"]);
    assert!(trust.status.success(), "{}", stderr_of(&trust));
    let trusted_files = trust_store_files(fixture.home.path());

    let (_, writer) = fixture.load();
    writer
        .expect("writer")
        .set_favorite("alpha", "a/two", true)
        .expect("save");

    assert_eq!(trust_store_files(fixture.home.path()), trusted_files);
    let output = fixture.run(&["--no-tui", "--prompt", "hi"]);
    let stderr = stderr_of(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("is not trusted") && stderr.contains("changed since it was trusted"),
        "{stderr}"
    );

    let retrust = fixture.run(&["trust"]);
    assert!(retrust.status.success(), "{}", stderr_of(&retrust));
}

#[test]
fn startup_rejects_configured_models_and_favorites_the_allow_list_forbids() {
    for (name, contents, expected) in [
        (
            "model",
            r#"
[provider_connections.alpha]
provider = "mock"
allowed_models = ["a/*"]

[engines.main]
provider_connection = "alpha"
model = "other/model"
"#,
            "allowed_models",
        ),
        (
            "favorite",
            r#"
[provider_connections.alpha]
provider = "mock"
favorite_models = ["other/model"]
allowed_models = ["a/*"]

[engines.main]
provider_connection = "alpha"
model = "a/one"
"#,
            "allowed_models",
        ),
        (
            "empty",
            r#"
[provider_connections.alpha]
provider = "mock"
allowed_models = []
"#,
            "allowed_models",
        ),
    ] {
        let fixture = Fixture::with_workspace_profile(contents);
        assert!(fixture.run(&["trust"]).status.success());

        let output = fixture.run(&["--no-tui", "--prompt", "hi"]);
        let stderr = stderr_of(&output);

        assert!(!output.status.success(), "{name}: {stderr}");
        assert!(stderr.contains(expected), "{name}: {stderr}");
    }
}
