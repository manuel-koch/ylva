use super::*;

#[test]
fn tool_sandbox_rules_table_loads_named_and_glob_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "with-tools",
        r#"
[tools.read]
rules = [
  { rule = "dot_path", verdict = "user_approval_required" },
]

[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "with-tools").expect("the fixture loads");

    let read_rules = &profile.tools.get("read").expect("read table").rules;
    assert_eq!(read_rules.len(), 1);
    assert!(matches!(
        read_rules[0],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::DotPath,
            ..
        }
    ));

    let write_rules = &profile.tools.get("write").expect("write table").rules;
    assert_eq!(write_rules.len(), 2);
    assert!(matches!(
        write_rules[0],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::OutsideWorkspace,
            ..
        }
    ));
    assert!(
        matches!(&write_rules[1], crate::sandbox::SandboxRuleConfig::PathGlob { pattern, .. } if pattern == "**/build/**")
    );

    assert!(
        !profile.tools.contains_key("edit"),
        "a tool with no [tools.<name>] table has no entry, falling back to the default floor"
    );
}

#[test]
fn tool_bash_table_loads_command_glob_and_bash_named_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "with-bash-tools",
        r#"
[tools.bash]
rules = [
  { rule = "backgrounded", verdict = "auto_denied" },
  { command = ["git", "reflog", "*"], verdict = "auto_approved" },
]
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "with-bash-tools").expect("the fixture loads");
    let bash_rules = &profile.tools.get("bash").expect("bash table").rules;
    assert_eq!(bash_rules.len(), 2);
    assert!(matches!(
        bash_rules[0],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::Backgrounded,
            ..
        }
    ));
    assert!(matches!(
        &bash_rules[1],
        crate::sandbox::SandboxRuleConfig::CommandGlob { command, .. }
            if command == &vec!["git".to_string(), "reflog".to_string(), "*".to_string()]
    ));
}

#[test]
fn command_glob_rule_under_a_non_bash_tool_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-command-glob",
        r#"
[tools.read]
rules = [
  { command = ["git", "status"], verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-command-glob")
        .expect_err("a CommandGlob rule under [tools.read] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "read"
    ));
}

#[test]
fn path_glob_rule_under_tool_bash_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-path-glob",
        r#"
[tools.bash]
rules = [
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-path-glob")
        .expect_err("a PathGlob rule under [tools.bash] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn command_glob_rule_with_a_non_trailing_single_wildcard_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-command-wildcard-position",
        r#"
[tools.bash]
rules = [
  { command = ["git", "*", "status"], verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-command-wildcard-position")
        .expect_err("a `*` that isn't the last command token must be rejected");
    assert!(matches!(
        error,
        ConfigError::MisplacedCommandGlobWildcard { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn command_glob_rule_with_a_non_trailing_multi_wildcard_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-command-double-wildcard-position",
        r#"
[tools.bash]
rules = [
  { command = ["git", "**", "status"], verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-command-double-wildcard-position")
        .expect_err("a `**` that isn't the last command token must be rejected");
    assert!(matches!(
        error,
        ConfigError::MisplacedCommandGlobWildcard { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn command_glob_rule_with_a_trailing_wildcard_loads_successfully() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "good-command-wildcard-position",
        r#"
[tools.bash]
rules = [
  { command = ["git", "status", "*"], verdict = "auto_approved" },
]
"#,
    );

    ProfileConfig::load(temp.path(), "good-command-wildcard-position")
        .expect("a trailing `*` as the last command token must be accepted");
}

#[test]
fn outside_workspace_named_rule_under_tool_bash_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-named-rule",
        r#"
[tools.bash]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-named-rule")
        .expect_err("outside_workspace under [tools.bash] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn backgrounded_named_rule_under_a_path_tool_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-named-rule-path-tool",
        r#"
[tools.write]
rules = [
  { rule = "backgrounded", verdict = "auto_denied" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-named-rule-path-tool")
        .expect_err("backgrounded under [tools.write] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "write"
    ));
}

#[test]
fn a_profile_with_no_tool_tables_has_an_empty_tools_map() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(temp.path(), "no-tools", "default_engine = \"fast\"\n");
    let profile = ProfileConfig::load(temp.path(), "no-tools").expect("the fixture loads");
    assert!(
        profile.tools.is_empty(),
        "no [tools.*] tables means no configured overrides for any tool"
    );
}

#[test]
fn inline_agent_type_tools_table_parses_independent_of_profile_level_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "agent-scoped-tools",
        r#"
[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
]

[agents.custom]
allowed_tools = ["read", "write"]

[agents.custom.tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "agent-scoped-tools").expect("the fixture loads");

    let profile_write_rules = &profile.tools.get("write").expect("write table").rules;
    assert_eq!(profile_write_rules.len(), 1);

    let custom_agent = profile.agents.get("custom").expect("the custom agent");
    let agent_write_rules = &custom_agent
        .tools
        .get("write")
        .expect("agent-scoped write table")
        .rules;
    assert_eq!(
        agent_write_rules.len(),
        2,
        "the agent-scoped table is independent of, and need not match, the \
             profile-level table for the same tool"
    );
}

#[test]
fn tool_sandbox_rules_table_loads_cross_filesystem_rule() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "with-cross-fs",
        r#"
[tools.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "dot_path", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "with-cross-fs").expect("the fixture loads");
    let read_rules = &profile.tools.get("read").expect("read table").rules;
    assert_eq!(read_rules.len(), 3);
    assert!(
        matches!(
            read_rules[2],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::CrossFilesystem,
                ..
            }
        ),
        "cross_filesystem must parse as a named rule under [tools.read]: {:?}",
        read_rules[2]
    );
}

#[test]
fn cross_filesystem_named_rule_under_tool_bash_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-cross-fs-rule",
        r#"
[tools.bash]
rules = [
  { rule = "cross_filesystem", verdict = "auto_denied" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-cross-fs-rule")
        .expect_err("cross_filesystem under [tools.bash] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn agent_scoped_cross_filesystem_rule_parses_for_read() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "agent-scoped-cross-fs",
        r#"
[tools.read]
rules = [
  { rule = "dot_path", verdict = "auto_denied" },
]

[agents.custom]
allowed_tools = ["read"]

[agents.custom.tools.read]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { rule = "cross_filesystem", verdict = "user_approval_required" },
]
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "agent-scoped-cross-fs").expect("the fixture loads");

    let profile_read_rules = &profile.tools.get("read").expect("read table").rules;
    assert_eq!(profile_read_rules.len(), 1);

    let custom_agent = profile.agents.get("custom").expect("the custom agent");
    let agent_read_rules = &custom_agent
        .tools
        .get("read")
        .expect("agent-scoped read table")
        .rules;
    assert_eq!(agent_read_rules.len(), 2);
    assert!(
        matches!(
            agent_read_rules[1],
            crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::CrossFilesystem,
                ..
            }
        ),
        "the agent-scoped table carries its own cross_filesystem rule: {:?}",
        agent_read_rules[1]
    );
}

#[test]
fn max_tool_iterations_defaults_to_the_shared_limit_when_unconfigured() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "[agents.explorer]\npersona = \"You explore code.\"\n",
    );

    let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
    let explorer = profile.agents.get("explorer").expect("explorer agent type");
    assert_eq!(explorer.max_tool_iterations, None);
    assert_eq!(
        profile.max_tool_iterations_for("explorer"),
        crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
    );
    assert_eq!(
        profile.max_tool_iterations_for("default"),
        crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
    );
    assert_eq!(
        profile.max_tool_iterations_for("never-configured"),
        crate::agent::DEFAULT_MAX_TOOL_USE_ITERATIONS
    );
}

#[test]
fn inline_max_tool_iterations_overrides_a_file_authored_definition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "[agents.research]\nmax_tool_iterations = 7\n",
    );
    write_agent_type_file(
        &home,
        "agents/research.md",
        "+++\ncan_spawn = true\nmax_tool_iterations = 12\n+++\nInvestigate evidence.\n",
    );

    let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
    let research = profile.agents.get("research").expect("research agent type");
    assert_eq!(
        research.max_tool_iterations,
        Some(7),
        "the inline override replaces the file-authored limit"
    );
    assert_eq!(profile.max_tool_iterations_for("research"), 7);

    write_profile_config(
        &profiles_root,
        "solo",
        "[agents.solo]\nmax_tool_iterations = 20\n",
    );
    let solo_profile = ProfileConfig::load(&profiles_root, "solo").expect("profile loads");
    assert_eq!(solo_profile.max_tool_iterations_for("solo"), 20);
}

#[test]
fn a_zero_max_tool_iterations_is_rejected_during_profile_validation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "[agents.explorer]\nmax_tool_iterations = 0\n",
    );

    let error = ProfileConfig::load(&profiles_root, "work")
        .expect_err("a zero iteration limit must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidMaxToolIterations { .. }
    ));
    let message = error.to_string();
    assert!(message.contains("max_tool_iterations = 0"), "{message}");
    assert!(message.contains("explorer"), "{message}");
}

#[test]
fn a_zero_max_tool_iterations_in_an_agent_type_file_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(&profiles_root, "work", "\n");
    write_agent_type_file(
        &home,
        "agents/research.md",
        "+++\nmax_tool_iterations = 0\n+++\nInvestigate evidence.\n",
    );

    let error = ProfileConfig::load(&profiles_root, "work")
        .expect_err("a file-authored zero iteration limit must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidMaxToolIterations { .. }
    ));
    assert!(error.to_string().contains("research"), "{error}");
}

#[test]
fn network_request_rule_under_a_path_tool_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-network-rule-for-read",
        r#"
[tools.read]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-network-rule-for-read")
        .expect_err("a network_request rule under [tools.read] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "read"
    ));
}

#[test]
fn network_request_rule_under_tool_bash_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-network-rule-for-bash",
        r#"
[tools.bash]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-network-rule-for-bash")
        .expect_err("a network_request rule under [tools.bash] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn target_on_a_non_network_rule_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "target-on-path-rule",
        r#"
[tools.read]
rules = [
  { rule = "outside_workspace", target = "https://docs.rs", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "target-on-path-rule")
        .expect_err("target on a path rule must be rejected");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::MisplacedSandboxRuleTarget { tool, index: 0, .. } if tool == "read"
    ));
    assert!(
        message.contains("network_request"),
        "the message must name the only rule kind accepting target: {message}"
    );
}

#[test]
fn target_on_a_command_rule_under_tool_bash_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "target-on-command-rule",
        r#"
[tools.bash]
rules = [
  { rule = "backgrounded", target = "https://docs.rs", verdict = "auto_denied" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "target-on-command-rule")
        .expect_err("target on a command rule must be rejected");
    assert!(matches!(
        error,
        ConfigError::MisplacedSandboxRuleTarget { tool, index: 0, .. } if tool == "bash"
    ));
}

#[test]
fn invalid_target_is_a_hard_error() {
    for (name, target) in [
        ("catch-all-target", "*"),
        ("mixed-wildcard-target", "https://a**b.example.com"),
        ("path-target", "https://github.com/owner"),
        ("bad-port-target", "https://github.com:https"),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        write_profile_config(
            temp.path(),
            name,
            &format!(
                r#"
[tools.web_fetch]
rules = [
  {{ rule = "network_request", target = "{target}", verdict = "auto_approved" }},
]
"#
            ),
        );

        let error = ProfileConfig::load(temp.path(), name)
            .expect_err("an invalid target must be rejected at startup");
        assert!(
            matches!(
                &error,
                ConfigError::InvalidSandboxRuleTarget { tool, index: 0, detail, .. }
                    if tool == "web_fetch" && !detail.is_empty()
            ),
            "expected InvalidSandboxRuleTarget for '{target}', got {error:?}"
        );
    }
}

#[test]
fn network_request_rule_loads_under_tool_web_search() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "network-web-search",
        r#"
[tools.web_search]
rules = [
  { rule = "network_request", verdict = "auto_approved" },
]
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "network-web-search").expect("the fixture loads");
    let rules = &profile
        .tools
        .get("web_search")
        .expect("web_search table")
        .rules;
    assert_eq!(rules.len(), 1);
    assert!(matches!(
        rules[0],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            ..
        }
    ));
}

#[test]
fn network_request_rule_loads_under_agent_scoped_web_search() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "agent-scoped-network",
        r#"
[agents.custom]
allowed_tools = ["web_search"]

[agents.custom.tools.web_search]
rules = [
  { rule = "network_request", verdict = "auto_denied", reason = "off" },
]
"#,
    );

    let profile =
        ProfileConfig::load(temp.path(), "agent-scoped-network").expect("the fixture loads");
    let custom = profile.agents.get("custom").expect("the custom agent");
    let rules = &custom
        .tools
        .get("web_search")
        .expect("web_search table")
        .rules;
    assert_eq!(rules.len(), 1);
}

#[test]
fn path_glob_rule_under_tool_web_search_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-path-rule-for-web-search",
        r#"
[tools.web_search]
rules = [
  { pattern = "**", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-path-rule-for-web-search")
        .expect_err("a path rule under [tools.web_search] must be rejected");
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_search"
    ));
    assert!(
        message.contains("network_request"),
        "the message must name the accepted rule kind: {message}"
    );
}

#[test]
fn command_glob_rule_under_tool_web_search_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-command-rule-for-web-search",
        r#"
[tools.web_search]
rules = [
  { command = ["git", "status"], verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-command-rule-for-web-search")
        .expect_err("a command rule under [tools.web_search] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_search"
    ));
}

#[test]
fn network_request_rules_with_and_without_target_load_under_tool_web_fetch() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "web-fetch-targets",
        r#"
[tools.web_fetch]
rules = [
  { rule = "network_request", verdict = "user_approval_required" },
  { rule = "network_request", target = "https://docs.rs", verdict = "auto_approved" },
]
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "web-fetch-targets").expect("the fixture loads");
    let rules = &profile
        .tools
        .get("web_fetch")
        .expect("web_fetch table")
        .rules;
    assert_eq!(rules.len(), 2);
    assert!(matches!(
        rules[0],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            target: None,
            ..
        }
    ));
    assert!(matches!(
        rules[1],
        crate::sandbox::SandboxRuleConfig::Named {
            rule: crate::sandbox::NamedSandboxRule::NetworkRequest,
            ref target,
            ..
        } if target.as_deref() == Some("https://docs.rs")
    ));
}

#[test]
fn schemeless_and_wildcard_targets_load_under_tool_web_fetch() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "web-fetch-target-vocabulary",
        r#"
[tools.web_fetch]
rules = [
  { rule = "network_request", target = "github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://*.github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://**.github.com", verdict = "auto_approved" },
  { rule = "network_request", target = "https://github.com:*", verdict = "auto_approved" },
]
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "web-fetch-target-vocabulary")
        .expect("the full target vocabulary loads");
    let rules = &profile
        .tools
        .get("web_fetch")
        .expect("web_fetch table")
        .rules;
    assert_eq!(rules.len(), 4);
}

#[test]
fn path_rule_under_tool_web_fetch_is_a_hard_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "bad-path-rule-for-web-fetch",
        r#"
[tools.web_fetch]
rules = [
  { pattern = "**", verdict = "auto_approved" },
]
"#,
    );

    let error = ProfileConfig::load(temp.path(), "bad-path-rule-for-web-fetch")
        .expect_err("a path rule under [tools.web_fetch] must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidSandboxRuleKind { tool, index: 0, .. } if tool == "web_fetch"
    ));
}
