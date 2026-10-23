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

//! Workspace configuration is only used after `ylva trust` approved its
//! exact content (ADR-0003).

use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;

const UNRESOLVABLE_ENGINE_CONFIG: &str = r#"
default_engine = "missing"

[engines.available]
provider = "mock"
model = "test-model"
"#;

struct Fixture {
    home: TempDir,
    workspace: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Fixture {
            home: TempDir::new().expect("home"),
            workspace: TempDir::new().expect("workspace"),
        };
        let profile_directory = fixture.workspace.path().join(".ylva/profiles/default");
        fs::create_dir_all(&profile_directory).expect("workspace profile");
        fixture.write_config(UNRESOLVABLE_ENGINE_CONFIG);
        fixture
    }

    fn write_config(&self, contents: &str) {
        fs::write(
            self.workspace
                .path()
                .join(".ylva/profiles/default/config.toml"),
            contents,
        )
        .expect("write config");
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

    fn run_prompt(&self) -> String {
        stderr_of(&self.run(&["--no-tui", "--prompt", "hi"]))
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn untrusted_workspace_configuration_refuses_startup() {
    let fixture = Fixture::new();

    let output = fixture.run(&["--no-tui", "--prompt", "hi"]);
    let stderr = stderr_of(&output);

    assert!(!output.status.success(), "{stderr}");
    assert!(fixture.run(&["--version"]).status.success());

    assert!(
        stderr.contains("is not trusted") && stderr.contains("never trusted"),
        "{stderr}"
    );
    assert!(!stderr.contains("references engine 'missing'"), "{stderr}");
}

#[test]
fn trusted_workspace_configuration_is_used_until_it_changes() {
    let fixture = Fixture::new();
    let trust = fixture.run(&["trust"]);
    assert!(trust.status.success(), "{}", stderr_of(&trust));

    let stderr = fixture.run_prompt();
    assert!(stderr.contains("references engine 'missing'"), "{stderr}");
    assert!(!stderr.contains("is not trusted"), "{stderr}");

    fixture.write_config(&format!("{UNRESOLVABLE_ENGINE_CONFIG}\n[tools.bash]\n"));
    let stderr = fixture.run_prompt();
    assert!(
        stderr.contains("is not trusted") && stderr.contains("changed since it was trusted"),
        "{stderr}"
    );
}

#[test]
fn revoke_withdraws_the_trust() {
    let fixture = Fixture::new();
    assert!(fixture.run(&["trust"]).status.success());

    let revoke = fixture.run(&["trust", "--revoke"]);
    assert!(revoke.status.success(), "{}", stderr_of(&revoke));

    assert!(fixture.run_prompt().contains("is not trusted"));
}
