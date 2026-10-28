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

fn load(root: &Path, name: &str) -> Result<ProfileConfig, ConfigError> {
    ProfileConfig::load_with_environment_at_workspace_root(root, name, &HashMap::new(), None)
}

#[test]
fn unknown_keys_warn_with_file_full_path_and_suggestion() {
    let temporary = tempfile::tempdir().unwrap();
    write_profile_config(
        temporary.path(),
        "typos",
        r#"
default_engin = "main"
[provider_connections.local]
provider = "ollama"
api_bsae_url = "http://localhost:11434"
[engines.main]
provider_connection = "local"
modle = "llama"
[web_search]
backend = "duckduckgo"
result_limt = 3
[agents.helper]
descripton = "helps"
"#,
    );
    let profile = load(temporary.path(), "typos").expect("unknown keys do not fail loading");
    let mut paths: Vec<(&str, Option<&str>)> = profile
        .config_warnings
        .iter()
        .map(|warning| (warning.key_path.as_str(), warning.suggestion.as_deref()))
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            ("agents.helper.descripton", Some("description")),
            ("default_engin", Some("default_engine")),
            ("engines.main.modle", Some("model")),
            (
                "provider_connections.local.api_bsae_url",
                Some("api_base_url")
            ),
            ("web_search.result_limt", Some("result_limit")),
        ]
    );
    let expected_file = super::super::super::profile_config_path(temporary.path(), "typos");
    assert!(profile
        .config_warnings
        .iter()
        .all(|warning| warning.file == expected_file));
}

#[test]
fn known_keys_produce_no_warnings() {
    let temporary = tempfile::tempdir().unwrap();
    write_profile_config(
        temporary.path(),
        "clean",
        r#"
default_engine = "main"
[provider_connections.local]
provider = "ollama"
[engines.main]
provider_connection = "local"
model = "llama"
[agents.helper]
description = "helps"
[tools.read]
rules = [{ pattern = "**", verdict = "auto_approved" }]
"#,
    );
    assert!(load(temporary.path(), "clean")
        .unwrap()
        .config_warnings
        .is_empty());
}

#[test]
fn agent_type_files_report_unknown_keys_in_every_format() {
    let temporary = tempfile::tempdir().unwrap();
    write_profile_config(temporary.path(), "files", "");
    for (file_name, contents) in [
        ("toml_agent.toml", "descripton = \"x\"\n"),
        ("toml_front.md", "+++\ndescripton = \"x\"\n+++\nPersona\n"),
        ("yaml_front.md", "---\ndescripton: x\n---\nPersona\n"),
    ] {
        write_agent_type_file(
            temporary.path(),
            &format!("files/agents/{file_name}"),
            contents,
        );
    }
    let profile = load(temporary.path(), "files").unwrap();
    assert_eq!(
        profile.config_warnings.len(),
        3,
        "{:?}",
        profile.config_warnings
    );
    assert!(profile
        .config_warnings
        .iter()
        .all(|warning| warning.key_path == "descripton"
            && warning.suggestion.as_deref() == Some("description")));
}

#[test]
fn unknown_keys_in_sandbox_rule_tables_are_hard_errors() {
    for (table, key) in [
        (
            "[tools.bash]\nrule = [{ command = [\"git\", \"*\"], verdict = \"auto_denied\" }]\n",
            "rule",
        ),
        (
            "[tools.read]\nrules = [{ pattern = \"**\", verdict = \"auto_denied\", reson = \"x\" }]\n",
            "reson",
        ),
        (
            "[agents.helper.tools.read]\nrules = [{ pattern = \"**\", verdict = \"auto_denied\", verdit = \"x\" }]\n",
            "verdit",
        ),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        write_profile_config(temporary.path(), "rules", table);
        let error = load(temporary.path(), "rules").expect_err(table);
        let message = error.to_string();
        assert!(message.contains(key), "{message}");
        assert!(message.contains("config.toml"), "{message}");
    }
}

#[test]
fn ambiguous_or_untargeted_sandbox_rules_are_rejected() {
    for rule in [
        "{ pattern = \"**\", command = [\"ls\"], verdict = \"auto_denied\" }",
        "{ verdict = \"auto_denied\" }",
        "{ pattern = \"**\", verdict = \"auto_denied\", target = \"https://x\" }",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        write_profile_config(
            temporary.path(),
            "rules",
            &format!("[tools.read]\nrules = [{rule}]\n"),
        );
        assert!(load(temporary.path(), "rules").is_err(), "{rule}");
    }
}

#[test]
fn an_unsupported_provider_kind_fails_at_load() {
    let temporary = tempfile::tempdir().unwrap();
    write_profile_config(
        temporary.path(),
        "typo",
        r#"
[provider_connections.local]
provider = "ollma"
"#,
    );
    let error = load(temporary.path(), "typo").expect_err("typo in provider kind fails");
    assert!(error.to_string().contains("ollma"), "{error}");
}

#[test]
fn favorite_and_allowed_model_keys_are_known_and_typos_get_hints() {
    let temporary = tempfile::tempdir().unwrap();
    write_profile_config(
        temporary.path(),
        "preferences",
        r#"
[provider_connections.local]
provider = "ollama"
favorite_models = ["a"]
allowed_models = ["a"]
favorite_model = ["b"]
alowed_models = ["b"]
"#,
    );

    let profile = load(temporary.path(), "preferences").expect("loads");
    let rendered: Vec<String> = profile
        .config_warnings
        .iter()
        .map(ToString::to_string)
        .collect();

    assert_eq!(rendered.len(), 2, "{rendered:?}");
    assert!(rendered
        .iter()
        .any(|line| line.contains("favorite_model`") && line.contains("`favorite_models`")));
    assert!(rendered
        .iter()
        .any(|line| line.contains("alowed_models") && line.contains("`allowed_models`")));
}
