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

#[test]
fn registered_tool_titles_omit_unknown_arguments() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let config = ToolRegistryConfig {
        allowed_tools: DEFAULT_TUI_TOOLS
            .iter()
            .map(|name| name.to_string())
            .collect(),
        include_spawn_subagent: true,
        ..ToolRegistryConfig::default()
    };
    let registry = default_registry(sandbox, config);

    for tool_name in registry.names() {
        let call = registry
            .create_call(
                &tool_name,
                uuid::Uuid::new_v4(),
                serde_json::json!({ "unknown_argument": "not supported" }),
            )
            .expect("registered tool call");
        assert!(
            call.title_arguments().is_empty(),
            "{tool_name} displayed an unsupported argument"
        );
    }
}

#[test]
fn bash_registry_call_orders_title_arguments_command_then_label() {
    // The agent loop emits `UiUpdate::ToolPending { title_arguments }`
    // straight from `front.call.title_arguments()`, so the registry-built
    // call must already carry `command` (primary) ahead of `label` (prop).
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry = default_registry(sandbox, ToolRegistryConfig::default());
    let call = registry
        .create_call(
            bash::BashTool::NAME,
            uuid::Uuid::new_v4(),
            serde_json::json!({ "command": "ls -la", "label": "List workspace files" }),
        )
        .expect("bash tool call");
    assert_eq!(
        call.title_arguments(),
        vec![
            ("command".to_string(), serde_json::json!("ls -la")),
            (
                "label".to_string(),
                serde_json::json!("List workspace files")
            ),
        ]
    );
}

#[test]
fn tool_default_title_capitalises_name_words() {
    let tool = TestTool;
    // single word: first letter capitalised
    assert_eq!(tool.title(), "Test");
}

#[test]
fn tool_title_splits_on_hyphen_and_underscore() {
    assert_eq!(title_case_tool_name("web_search"), "Web Search");
    assert_eq!(title_case_tool_name("my-cool-tool"), "My Cool Tool");
    assert_eq!(title_case_tool_name("compile"), "Compile");
    assert_eq!(title_case_tool_name("deploy"), "Deploy");
    // Regression: the tool-bubble title must render as "Spawn Subagent",
    // not "Spawn_subagent" (only the leading char capitalised).
    assert_eq!(title_case_tool_name("spawn_subagent"), "Spawn Subagent");
}

#[test]
fn subtype_argument_defaults_to_none() {
    assert_eq!(TestTool.subtype_argument(), None);
    assert_eq!(
        crate::agent::tools::read::ReadTool::default().subtype_argument(),
        None
    );
}

#[test]
fn display_title_with_subtype_qualifies_the_title_and_drops_the_argument() {
    let arguments = vec![
        (
            "label".to_string(),
            serde_json::json!("Refactor the parser"),
        ),
        (
            "agent_type".to_string(),
            serde_json::json!("local-discovery"),
        ),
        ("task".to_string(), serde_json::json!("do it")),
    ];

    let (title, remaining) =
        display_title_with_subtype("Spawn Subagent", Some("agent_type"), &arguments);

    assert_eq!(title, "Spawn Subagent (local-discovery)");
    assert_eq!(
        remaining,
        vec![
            (
                "label".to_string(),
                serde_json::json!("Refactor the parser")
            ),
            ("task".to_string(), serde_json::json!("do it")),
        ],
        "the subtype argument must not be rendered a second time as a prop"
    );
}

#[test]
fn display_title_with_subtype_is_unchanged_when_the_argument_is_absent() {
    let arguments = vec![("label".to_string(), serde_json::json!("Refactor"))];

    let (title, remaining) =
        display_title_with_subtype("Spawn Subagent", Some("agent_type"), &arguments);

    assert_eq!(title, "Spawn Subagent");
    assert_eq!(remaining, arguments);
}

#[test]
fn display_title_without_a_subtype_argument_touches_nothing() {
    let arguments = vec![(
        "agent_type".to_string(),
        serde_json::json!("local-discovery"),
    )];

    let (title, remaining) = display_title_with_subtype("Web Search", None, &arguments);

    assert_eq!(title, "Web Search");
    assert_eq!(remaining, arguments);
}

#[test]
fn display_title_flattens_a_multiline_subtype_value() {
    let arguments = vec![("agent_type".to_string(), serde_json::json!("one\ntwo"))];

    let (title, remaining) = display_title_with_subtype("Subagent", Some("agent_type"), &arguments);

    assert_eq!(title, "Subagent (one↵two)");
    assert!(remaining.is_empty());
}
