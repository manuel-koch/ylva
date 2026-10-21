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
fn conditional_rules_are_wired_and_cli_override_requires_valid_policy() {
    let sandbox = Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        "/workspace".into(),
    )));
    let rules: crate::config::ToolSandboxConfig = toml::from_str(
        r#"
rules = [{ command = ["sort", "**"], only_arguments = ["-n"], verdict = "auto_approved" }]
"#,
    )
    .unwrap();
    let configuration = |rules, auto_approve| ToolRegistryConfig {
        allowed_tools: vec!["bash".into()],
        tool_sandbox_rules: HashMap::from([("bash".into(), rules)]),
        auto_approve,
        ..ToolRegistryConfig::default()
    };
    let registry = default_registry(
        sandbox.clone(),
        configuration(rules.rules.clone(), std::collections::HashSet::new()),
    )
    .unwrap();
    let tool = registry.get("bash").unwrap();
    assert_eq!(
        tool.needs_approval(&serde_json::json!({"command":"sort -n file"})),
        ApprovalCheck::AutoApproved
    );
    assert_eq!(
        tool.needs_approval(&serde_json::json!({"command":"sort -o output file"})),
        ApprovalCheck::UserApprovalRequired
    );
    let registry = default_registry(
        sandbox.clone(),
        configuration(
            rules.rules,
            std::collections::HashSet::from([AutoApproveTool::Bash]),
        ),
    )
    .unwrap();
    assert_eq!(
        registry
            .get("bash")
            .unwrap()
            .needs_approval(&serde_json::json!({"command":"sort -o output file"})),
        ApprovalCheck::AutoApproved
    );
    let malformed: crate::config::ToolSandboxConfig = toml::from_str(
        r#"
rules = [
  { command = ["sort", "**"], verdict = "auto_approved" },
  { command = ["sort", "**"], with_arguments = ["["], verdict = "auto_denied" },
]
"#,
    )
    .unwrap();
    assert!(
        default_registry(
            sandbox,
            configuration(
                malformed.rules,
                std::collections::HashSet::from([AutoApproveTool::Bash])
            )
        )
        .is_err(),
        "CLI override cannot recover invalid policy construction"
    );
}

#[test]
fn registry_register_lookup_and_unknown_name() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    assert!(registry.get("test").is_some());
    assert!(registry.get("missing").is_none());
    assert_eq!(registry.names(), vec!["test".to_string()]);
}

#[test]
fn registry_schema_is_tool_schema_envelope() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    let schemas = registry.schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "test");
    assert!(schemas[0].description.starts_with("a test tool"));
    assert!(schemas[0].description.contains(&format!(
        "at most {} distinct tool calls",
        crate::agent::MAX_TOOL_CALLS_PER_RESPONSE
    )));
    assert_eq!(
        schemas[0].parameters["required"],
        serde_json::json!(["field"])
    );
}

#[test]
fn default_registry_registers_cli_tools() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry =
        default_registry(sandbox, ToolRegistryConfig::default()).expect("valid sandbox fixture");
    assert_eq!(registry.names().len(), 8);
    assert!(registry.get(read::ReadTool::NAME).is_some());
    assert!(registry.get(write::WriteTool::NAME).is_some());
    assert!(registry.get(glob::GlobTool::NAME).is_some());
    assert!(registry.get(bash::BashTool::NAME).is_some());
    assert!(registry.get(search::SearchTool::NAME).is_some());
    assert!(registry.get(edit::EditTool::NAME).is_some());
    assert!(registry.get(web_search::WebSearchTool::NAME).is_some());
    assert!(registry.get(web_fetch::WebFetchTool::NAME).is_some());
    assert!(registry.get(clarify::ClarifyTool::NAME).is_none());
}

#[test]
fn default_registry_registers_tui_tools_including_clarify() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            allowed_tools: DEFAULT_TUI_TOOLS.iter().map(|s| s.to_string()).collect(),
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");
    assert_eq!(registry.names().len(), 9);
    assert!(registry.get(clarify::ClarifyTool::NAME).is_some());
    assert!(registry.get(web_search::WebSearchTool::NAME).is_some());
    assert!(registry.get(web_fetch::WebFetchTool::NAME).is_some());
}

#[test]
fn default_registry_keeps_web_fetch_out_when_not_allowed() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            allowed_tools: vec![web_search::WebSearchTool::NAME.to_string()],
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");
    assert!(registry.get(web_fetch::WebFetchTool::NAME).is_none());
}

#[test]
fn auto_approve_web_fetch_wins_over_configured_deny_rule() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        web_fetch::WebFetchTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
            reason: Some("disabled by policy".to_string()),
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::WebFetch]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let web_fetch_tool = registry
        .get(web_fetch::WebFetchTool::NAME)
        .expect("web_fetch tool");
    assert_eq!(
        web_fetch_tool.needs_approval(&serde_json::json!({ "url": "https://docs.rs/" })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-web-fetch's catch-all rule must win over a configured deny rule"
    );
}

#[test]
fn configured_web_fetch_target_rule_applies_through_default_registry() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        web_fetch::WebFetchTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
            reason: None,
            target: Some("https://docs.rs".to_string()),
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let web_fetch_tool = registry
        .get(web_fetch::WebFetchTool::NAME)
        .expect("web_fetch tool");
    assert_eq!(
        web_fetch_tool.needs_approval(&serde_json::json!({ "url": "https://docs.rs/htmd/" })),
        ApprovalCheck::AutoApproved,
        "a matching target rule must resolve through default_registry"
    );
    assert_eq!(
        web_fetch_tool.needs_approval(&serde_json::json!({ "url": "https://example.com/" })),
        ApprovalCheck::UserApprovalRequired,
        "a non-matching target must abstain"
    );
}

#[test]
fn default_registry_registers_web_search_with_a_non_empty_schema() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry =
        default_registry(sandbox, ToolRegistryConfig::default()).expect("valid sandbox fixture");

    let schema = registry
        .schemas()
        .into_iter()
        .find(|schema| schema.name == web_search::WebSearchTool::NAME)
        .expect("web_search must expose a schema");
    assert!(
        schema
            .parameters
            .get("properties")
            .and_then(|properties| properties.as_object())
            .is_some_and(|properties| !properties.is_empty()),
        "the schema must describe the tool's arguments: {}",
        schema.parameters
    );
    assert!(schema.description.contains("terms"));
}

#[test]
fn auto_approve_web_search_wins_over_configured_deny_rule() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        web_search::WebSearchTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
            reason: Some("disabled by policy".to_string()),
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::WebSearch]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let web_search_tool = registry
        .get(web_search::WebSearchTool::NAME)
        .expect("web_search tool");
    assert_eq!(
        web_search_tool.needs_approval(&serde_json::json!({ "terms": "query" })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-web-search's catch-all rule must win over a configured deny rule"
    );
}

#[test]
fn configured_web_search_rule_applies_through_default_registry() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        web_search::WebSearchTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let web_search_tool = registry
        .get(web_search::WebSearchTool::NAME)
        .expect("web_search tool");
    assert_eq!(
            web_search_tool.needs_approval(&serde_json::json!({ "terms": "query" })),
            ApprovalCheck::AutoApproved,
            "[tools.web_search]'s configured network_request rule must resolve through default_registry"
        );
}

#[test]
fn brave_target_scoped_rule_matches_the_brave_endpoint_only() {
    // A brave-configured profile makes the tool's network match key the
    // fixed Brave authority, so only a rule targeting that authority may
    // auto-approve — the target-scoped contract of
    // `WebSearchBackend::network_target`.
    let brave_web_search = crate::config::WebSearchConfig {
        backend: "brave".to_string(),
        backends: crate::config::WebSearchBackendsConfig {
            searxng: None,
            brave: Some(crate::config::BraveBackendConfig {
                api_key: Some("test-subscription-token".to_string()),
            }),
        },
        ..crate::config::WebSearchConfig::default()
    };
    let tool_sandbox_rules_for = |target: &str| {
        let mut tool_sandbox_rules = HashMap::new();
        tool_sandbox_rules.insert(
            web_search::WebSearchTool::NAME.to_string(),
            vec![crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: Some(target.to_string()),
            }],
        );
        tool_sandbox_rules
    };
    let registry_for = |target: &str| {
        default_registry(
            std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
            ToolRegistryConfig {
                tool_sandbox_rules: tool_sandbox_rules_for(target),
                web_search: brave_web_search.clone(),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture")
    };

    let registry = registry_for("https://api.search.brave.com");
    let web_search_tool = registry
        .get(web_search::WebSearchTool::NAME)
        .expect("web_search tool");
    assert_eq!(
        web_search_tool.needs_approval(&serde_json::json!({ "terms": "query" })),
        ApprovalCheck::AutoApproved,
        "the brave network_target must match a rule targeting https://api.search.brave.com"
    );

    let other_registry = registry_for("https://html.duckduckgo.com");
    let other_web_search_tool = other_registry
        .get(web_search::WebSearchTool::NAME)
        .expect("web_search tool");
    assert_eq!(
        other_web_search_tool.needs_approval(&serde_json::json!({ "terms": "query" })),
        ApprovalCheck::UserApprovalRequired,
        "a rule for another authority must not match the brave endpoint"
    );
}

#[test]
fn default_registry_respects_explicit_allowed_tools_subset() {
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            allowed_tools: vec![
                bash::BashTool::NAME.to_string(),
                read::ReadTool::NAME.to_string(),
            ],
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");
    assert_eq!(registry.names().len(), 2);
    assert!(registry.get(bash::BashTool::NAME).is_some());
    assert!(registry.get(read::ReadTool::NAME).is_some());
    assert!(registry.get(edit::EditTool::NAME).is_none());
}

#[test]
fn default_registry_wires_configured_rules_per_tool_only() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        read::ReadTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let dot_path = temp.path().join(".env").to_string_lossy().to_string();

    let read_tool = registry.get(read::ReadTool::NAME).expect("read tool");
    assert_eq!(
        read_tool.needs_approval(&serde_json::json!({ "path": dot_path })),
        ApprovalCheck::AutoApproved,
        "read's [tools.read] config replaces the default floor's \
             dot_path verdict"
    );

    // write has no [tools.write] entry: unaffected, still uses the
    // default floor (dot_path -> user approval required).
    let write_tool = registry.get(write::WriteTool::NAME).expect("write tool");
    assert_eq!(
        write_tool.needs_approval(&serde_json::json!({ "path": dot_path, "content": "x" })),
        ApprovalCheck::UserApprovalRequired
    );
}

#[test]
fn auto_approve_write_wins_over_configured_deny_rule() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        write::WriteTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Write]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let dot_path = temp.path().join(".env").to_string_lossy().to_string();
    let write_tool = registry.get(write::WriteTool::NAME).expect("write tool");
    assert_eq!(
        write_tool.needs_approval(&serde_json::json!({ "path": dot_path, "content": "x" })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-write's catch-all rule must still win over a configured deny rule"
    );
}

#[test]
fn auto_approve_bash_wins_over_configured_deny_rule() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        bash::BashTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::Backgrounded,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Bash]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let bash_tool = registry.get(bash::BashTool::NAME).expect("bash tool");
    assert_eq!(
        bash_tool.needs_approval(&serde_json::json!({ "command": "sleep 10 &" })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-bash's catch-all rule must still win over a configured deny rule"
    );
}

#[test]
fn auto_approve_read_wins_over_configured_deny_rule() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        read::ReadTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Read]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let dot_path = temp.path().join(".env").to_string_lossy().to_string();
    let read_tool = registry.get(read::ReadTool::NAME).expect("read tool");
    assert_eq!(
        read_tool.needs_approval(&serde_json::json!({ "path": dot_path })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-read's catch-all rule must still win over a configured deny rule"
    );
}

#[test]
fn auto_approve_glob_wins_over_configured_deny_rule() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        glob::GlobTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::OutsideWorkspace,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Glob]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let outside = temp
        .path()
        .parent()
        .expect("temp dir has a parent")
        .join("other")
        .to_string_lossy()
        .to_string();
    let glob_tool = registry.get(glob::GlobTool::NAME).expect("glob tool");
    assert_eq!(
        glob_tool.needs_approval(&serde_json::json!({ "pattern": "*", "path": outside })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-glob's catch-all rule must still win over a configured deny rule"
    );
}

#[test]
fn auto_approve_search_wins_over_configured_deny_rule() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        search::SearchTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::OutsideWorkspace,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Search]),
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let outside = temp
        .path()
        .parent()
        .expect("temp dir has a parent")
        .join("other")
        .to_string_lossy()
        .to_string();
    let search_tool = registry.get(search::SearchTool::NAME).expect("search tool");
    assert_eq!(
        search_tool.needs_approval(&serde_json::json!({ "pattern": "foo", "path": outside })),
        ApprovalCheck::AutoApproved,
        "--auto-approve-search's catch-all rule must still win over a configured deny rule"
    );
}

#[test]
fn default_registry_wires_configured_command_glob_rules_for_bash() {
    use crate::sandbox::ApprovalVerdictConfig;

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        bash::BashTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            command: vec!["git".to_string(), "status".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let bash_tool = registry.get(bash::BashTool::NAME).expect("bash tool");
    assert_eq!(
        bash_tool.needs_approval(&serde_json::json!({ "command": "git status" })),
        ApprovalCheck::AutoApproved,
        "[tools.bash]'s configured CommandGlob rule must resolve through default_registry"
    );
    // A different command not matched by the configured rule falls back
    // to the (now fully-replaced, empty-otherwise) configured list, which
    // abstains -> the tool's own conservative default (UserApprovalRequired).
    assert_eq!(
        bash_tool.needs_approval(&serde_json::json!({ "command": "git log" })),
        ApprovalCheck::UserApprovalRequired,
        "[tools.bash]'s configured rules fully replace the default floor"
    );
}

#[cfg(unix)]
#[test]
fn default_registry_wires_cross_filesystem_rule_for_read() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule};

    let temp = tempfile::tempdir().expect("tempdir");
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::with_root(
        temp.path().to_path_buf(),
    )));
    let mut tool_sandbox_rules = HashMap::new();
    tool_sandbox_rules.insert(
        read::ReadTool::NAME.to_string(),
        vec![crate::sandbox::SandboxRuleConfig::Named {
            rule: NamedSandboxRule::CrossFilesystem,
            verdict: ApprovalVerdictConfig::UserApprovalRequired,
            reason: None,
            target: None,
        }],
    );
    let registry = default_registry(
        sandbox,
        ToolRegistryConfig {
            tool_sandbox_rules,
            ..ToolRegistryConfig::default()
        },
    )
    .expect("valid sandbox fixture");

    let read_tool = registry.get(read::ReadTool::NAME).expect("read tool");
    assert_eq!(
        read_tool.needs_approval(&serde_json::json!({ "path": "/dev/null" })),
        ApprovalCheck::UserApprovalRequired,
        "read's configured cross_filesystem rule must surface through default_registry"
    );

    // A same-device in-workspace path does not match the rule; the
    // configured list replaced the default floor, so the sandbox abstains
    // and read auto-approves its own abstention.
    let same_device_path = temp.path().join("notes.txt").to_string_lossy().to_string();
    assert_eq!(
        read_tool.needs_approval(&serde_json::json!({ "path": same_device_path })),
        ApprovalCheck::AutoApproved,
        "a same-device path must not match cross_filesystem"
    );
}

#[test]
fn create_call_rewrites_absolute_path_like_argument_to_cwd_relative() {
    // Reads std::env::current_dir(); must hold SUBSCRIBER_LOCK to avoid
    // racing the logging test that temporarily mutates process cwd (see
    // docs/known-test-issues.md "Process-global cwd mutation in tests").
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .expect("subscriber mutex poisoned");
    let cwd = std::env::current_dir().expect("cwd");
    let absolute_path = cwd.join("Cargo.toml");

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    let call = registry
        .create_call(
            "test",
            uuid::Uuid::new_v4(),
            serde_json::json!({ "field": absolute_path.to_str().unwrap() }),
        )
        .expect("test tool is registered");
    assert_eq!(
        call.arguments()["field"],
        serde_json::json!("Cargo.toml"),
        "absolute path-like argument inside cwd should be rewritten cwd-relative before dispatch"
    );
}

#[test]
fn create_call_leaves_out_of_cwd_absolute_path_unchanged() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .expect("subscriber mutex poisoned");
    let temporary_dir = tempfile::tempdir().unwrap();
    let outside_path = temporary_dir.path().join("other/file.rs");

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    let call = registry
        .create_call(
            "test",
            uuid::Uuid::new_v4(),
            serde_json::json!({ "field": outside_path.to_str().unwrap() }),
        )
        .expect("test tool is registered");
    assert_eq!(
        call.arguments()["field"],
        serde_json::json!(outside_path.to_str().unwrap()),
        "absolute path outside cwd should pass through unchanged"
    );
}

#[test]
fn create_call_leaves_non_path_like_argument_unchanged() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    // TestTool only declares "field" as path-like; a plain string value
    // that happens to be a real absolute path is untouched under any
    // other key.
    let call = registry
        .create_call(
            "test",
            uuid::Uuid::new_v4(),
            serde_json::json!({ "field": "hello", "other": "/etc/hosts" }),
        )
        .expect("test tool is registered");
    assert_eq!(call.arguments()["other"], serde_json::json!("/etc/hosts"));
}

#[test]
fn create_call_expands_path_tokens_before_dispatch() {
    // Reads std::env::current_dir(); must hold SUBSCRIBER_LOCK to avoid
    // racing the logging test that temporarily mutates process cwd (see
    // docs/known-test-issues.md "Process-global cwd mutation in tests").
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .expect("subscriber mutex poisoned");
    // A uniquely named variable, set and unset inside this test only, so
    // no shared process-environment entry is touched.
    let variable_name = "YLVA_TEST_PATH_TOKEN_EXPANSION";
    let temporary_dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var(variable_name, temporary_dir.path());

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TestTool));
    let call = registry
        .create_call(
            "test",
            uuid::Uuid::new_v4(),
            serde_json::json!({ "field": format!("${{{variable_name}}}/notes.md") }),
        )
        .expect("test tool is registered");
    std::env::remove_var(variable_name);

    assert_eq!(
        call.arguments()["field"],
        serde_json::json!(format!("{}/notes.md", temporary_dir.path().display())),
        "the token must be expanded before the argument reaches the tool"
    );
}

#[test]
fn write_tool_always_needs_user_approval() {
    let write_tool = write::WriteTool::default();
    let args = serde_json::json!({ "path": "src/main.rs", "content": "hello" });
    assert!(matches!(
        write_tool.needs_approval(&args),
        ApprovalCheck::UserApprovalRequired
    ));
}
