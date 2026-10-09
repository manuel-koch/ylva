use super::*;

#[test]
fn root_spawn_tool_advertises_configured_agent_type_descriptions() {
    let (factory, mut profile) = make_factory();
    profile.agents.insert(
        "local-discovery".to_string(),
        AgentConfig {
            engine: None,
            compaction: None,
            description: Some(
                "Read-only repository exploration without file modifications.".to_string(),
            ),
            allowed_tools: None,
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: false,
            tools: HashMap::new(),
        },
    );

    let (agent, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the root agent");
    let spawn_schema = agent
        .registry()
        .schemas()
        .into_iter()
        .find(|schema| schema.name == "spawn_subagent")
        .expect("root agent should expose spawn_subagent");

    assert!(spawn_schema.description.contains(
        "- local-discovery: Read-only repository exploration without file modifications."
    ));
    assert!(spawn_schema
        .description
        .contains("- default: No description provided."));
}

#[test]
fn root_and_child_contexts_share_global_and_agent_type_prompt_once() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(MockProvider::new(
            vec!["answer".to_string()],
            Duration::from_millis(0),
        ));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let factory = AgentFactory::new(HashMap::from([("default".to_string(), engine)]))
            .with_system_prompt("global soul");
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            agents: HashMap::from([(
                "specialist".to_string(),
                AgentConfig {
                    engine: None,
                    compaction: None,
                    description: None,
                    allowed_tools: None,
                    timeout_secs: None,
                    max_tool_iterations: None,
                    persona: Some("specialist persona".to_string()),
                    can_spawn: false,
                    tools: HashMap::new(),
                },
            )]),
            ..ProfileConfig::default()
        };

        let (mut root, _root_ui_rx) = factory
            .create(&profile, "specialist", Role::Root)
            .expect("root construction succeeds");
        let root_handle = root.run();
        root.send_command(crate::agent::AgentCommand::Prompt("root task".to_string()))
            .await;
        drop(root);
        let root_context = tokio::time::timeout(Duration::from_secs(2), root_handle)
            .await
            .expect("root run completes")
            .expect("root task does not panic")
            .expect("root task succeeds");
        let root_system_messages: Vec<_> = root_context
            .messages()
            .iter()
            .filter_map(|message| match message {
                Message::System { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            root_system_messages.first().copied(),
            Some("global soul\n\nspecialist persona")
        );
        assert_eq!(
            root_system_messages
                .iter()
                .filter(|content| content.contains("global soul"))
                .count(),
            1
        );

        let (finish_tx, _finish_rx) = mpsc::channel(1);
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(1);
        let (mut child, _child_ui_rx) = factory
            .create(
                &profile,
                "specialist",
                Role::Child(ChildRoleConfig {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    session_id: None,
                    briefing: Some("child briefing".to_string()),
                    parent_cancel: CancellationToken::new(),
                    subagent_ui_tx,
                    finish_tx,
                }),
            )
            .expect("child construction succeeds");
        let child_handle = child.run();
        child
            .send_command(crate::agent::AgentCommand::Prompt("child task".to_string()))
            .await;
        let child_context = tokio::time::timeout(Duration::from_secs(2), child_handle)
            .await
            .expect("child run completes")
            .expect("child task does not panic")
            .expect("child task succeeds");
        let child_system_messages: Vec<_> = child_context
            .messages()
            .iter()
            .filter_map(|message| match message {
                Message::System { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            child_system_messages.first().copied(),
            Some("global soul\n\nspecialist persona")
        );
        assert_eq!(
            child_system_messages
                .iter()
                .filter(|content| content.contains("global soul"))
                .count(),
            1
        );
        assert!(child_system_messages.contains(&"child briefing"));
        assert!(child_system_messages
            .iter()
            .any(|content| content.contains("tool-use")));
    });
}

#[test]
fn create_honors_per_agent_type_allowed_tools_override() {
    // Regression test for Part A: `profile.agents.<agent_type>.allowed_tools`
    // must be honored per `agent_type`, not silently ignored in favor of
    // the factory-wide default list — so a distinct `agent_type` (e.g. a
    // restricted child role) can genuinely be limited to a different tool
    // set than the root.
    let (factory, profile) = make_factory();

    // "default" has no `allowed_tools` override in `make_factory`, so it
    // falls back to the factory-wide default list, which includes `bash`.
    let (unrestricted, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the unrestricted agent");
    assert!(
        unrestricted.registry().get("bash").is_some(),
        "expected the unrestricted agent_type to keep the factory-wide \
             default tool set, which includes bash"
    );

    let mut restricted_profile = profile.clone();
    restricted_profile.agents.insert(
        "restricted".to_string(),
        AgentConfig {
            engine: None,
            compaction: None,
            description: None,
            allowed_tools: Some(vec!["read".to_string()]),
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: false,
            tools: HashMap::new(),
        },
    );
    let (restricted, _ui_rx) = factory
        .create(&restricted_profile, "restricted", Role::Root)
        .expect("factory creates the restricted agent");
    assert!(
        restricted.registry().get("bash").is_none(),
        "expected the per-agent_type allowed_tools override to exclude \
             bash, which was not in its configured list"
    );
    assert!(
        restricted.registry().get("read").is_some(),
        "expected the per-agent_type allowed_tools override to still \
             register the tool it did configure"
    );
}

#[test]
fn create_honors_per_agent_type_tools_sandbox_rules_override() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Two agent types share one profile where the profile-wide
    // `[tools.write]` table only denies writes outside the workspace; one
    // agent type additionally sets `[agents.<type>.tools.write]` with an
    // extra glob rule auto-approving writes under `scratch/`. Only that
    // agent type's `write` tool should auto-approve a `scratch/` path;
    // the other agent type (profile default only) must still abstain
    // (require user approval) for that same path.
    let workspace_root = std::fs::canonicalize(std::env::current_dir().expect("current_dir"))
        .expect("canonicalize cwd");
    let scratch_path = workspace_root.join("scratch/file.txt");
    let scratch_glob_pattern = format!("{}/scratch/**", workspace_root.to_string_lossy());
    let write_args = serde_json::json!({
        "path": scratch_path.to_string_lossy(),
        "content": "hello",
    });

    let (factory, mut profile) = make_factory();
    profile.tools.insert(
        "write".to_string(),
        crate::config::ToolSandboxConfig {
            rules: vec![crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::OutsideWorkspace,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            }],
        },
    );
    profile.agents.insert(
        "scratch-writer".to_string(),
        AgentConfig {
            engine: None,
            compaction: None,
            description: None,
            allowed_tools: None,
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: false,
            tools: HashMap::from([(
                "write".to_string(),
                crate::config::ToolSandboxConfig {
                    rules: vec![
                        crate::sandbox::SandboxRuleConfig::Named {
                            rule: crate::sandbox::NamedSandboxRule::OutsideWorkspace,
                            verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
                            reason: None,
                            target: None,
                        },
                        crate::sandbox::SandboxRuleConfig::PathGlob {
                            pattern: scratch_glob_pattern,
                            verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                            reason: None,
                        },
                    ],
                },
            )]),
        },
    );

    let (default_agent, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the default agent");
    let default_write = default_agent
        .registry()
        .get("write")
        .expect("write tool registered");
    assert_eq!(
        default_write.needs_approval(&write_args),
        crate::sandbox::ApprovalCheck::UserApprovalRequired,
        "the default agent type has no agent-scoped tools override, so it \
             must fall back to the profile-level table alone, which does not \
             auto-approve scratch/ paths"
    );

    let (scratch_agent, _ui_rx) = factory
        .create(&profile, "scratch-writer", Role::Root)
        .expect("factory creates the scratch-writer agent");
    let scratch_write = scratch_agent
        .registry()
        .get("write")
        .expect("write tool registered");
    assert_eq!(
        scratch_write.needs_approval(&write_args),
        crate::sandbox::ApprovalCheck::AutoApproved,
        "the scratch-writer agent type's own tools.write table must win \
             over the profile-level table for that tool"
    );
}
