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
fn configured_bash_timeout_preserves_the_default_sandbox_floor() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (factory, mut profile) = make_factory();
    profile.tools.insert(
        "bash".into(),
        crate::config::ToolConfig {
            timeout_secs: Some(900),
            ..crate::config::ToolConfig::default()
        },
    );
    let (agent, _ui_rx) = factory.create(&profile, "default", Role::Root).unwrap();
    let bash = agent.registry().get("bash").unwrap();
    assert_eq!(bash.timeout_secs(), 900);
    let call = bash.create_call(Uuid::new_v4(), serde_json::json!({"command": "echo hi"}));
    assert_eq!(
        call.timeout_secs(),
        900 + crate::agent::tools::bash::BASH_OUTER_TIMEOUT_GRACE_SECS
    );
    assert_eq!(
        bash.needs_approval(&serde_json::json!({"command": "echo hi"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired
    );
    assert!(matches!(
        bash.needs_approval(&serde_json::json!({"command": "echo hi &"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::AutoDenied { .. }
    ));
}

#[test]
fn empty_read_table_inherits_the_default_sandbox_floor() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (factory, mut profile) = make_factory();
    profile.tools.insert("read".into(), Default::default());
    let (agent, _ui_rx) = factory.create(&profile, "default", Role::Root).unwrap();
    assert_eq!(
        agent
            .registry()
            .get("read")
            .unwrap()
            .needs_approval(&serde_json::json!({"path": ".ylva-private-test-file"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired
    );
}

#[test]
fn agent_timeout_inherits_profile_rules_and_explicit_empty_rules_replace_them() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (factory, mut profile) = make_factory();
    profile.tools.insert(
        "bash".into(),
        toml::from_str(
            r#"rules = [
              { rule = "backgrounded", verdict = "auto_denied" },
              { command = ["git", "status"], verdict = "auto_approved" },
            ]"#,
        )
        .unwrap(),
    );
    let mut worker = profile.agents["default"].clone();
    worker.can_spawn = false;
    worker.tools.insert(
        "bash".into(),
        crate::config::ToolConfig {
            timeout_secs: Some(900),
            ..crate::config::ToolConfig::default()
        },
    );
    profile.agents.insert("worker".into(), worker);
    for (name, timeout) in [("default", 300), ("worker", 900)] {
        let (agent, _ui_rx) = factory.create(&profile, name, Role::Root).unwrap();
        let bash = agent.registry().get("bash").unwrap();
        assert_eq!(bash.timeout_secs(), timeout);
        assert_eq!(
            bash.needs_approval(&serde_json::json!({"command": "git status"}))
                .verdict,
            crate::sandbox::ApprovalVerdict::AutoApproved
        );
        assert!(matches!(
            bash.needs_approval(&serde_json::json!({"command": "echo hi &"}))
                .verdict,
            crate::sandbox::ApprovalVerdict::AutoDenied { .. }
        ));
    }

    profile
        .agents
        .get_mut("worker")
        .unwrap()
        .tools
        .get_mut("bash")
        .unwrap()
        .rules = Some(vec![]);
    let (agent, _ui_rx) = factory.create(&profile, "worker", Role::Root).unwrap();
    let bash = agent.registry().get("bash").unwrap();
    assert_eq!(
        bash.needs_approval(&serde_json::json!({"command": "git status"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired
    );
    assert_eq!(
        bash.needs_approval(&serde_json::json!({"command": "echo hi &"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired
    );

    profile.tools.get_mut("bash").unwrap().rules = Some(vec![]);
    let (agent, _ui_rx) = factory.create(&profile, "default", Role::Root).unwrap();
    assert_eq!(
        agent
            .registry()
            .get("bash")
            .unwrap()
            .needs_approval(&serde_json::json!({"command": "echo hi &"}))
            .verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired
    );
}

#[test]
fn web_tool_timeouts_resolve_independently_per_agent() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (factory, mut profile) = make_factory();
    for (name, timeout_secs) in [("web_fetch", 11), ("web_search", 12)] {
        profile.tools.insert(
            name.into(),
            crate::config::ToolConfig {
                timeout_secs: Some(timeout_secs),
                ..crate::config::ToolConfig::default()
            },
        );
    }
    let mut worker = profile.agents["default"].clone();
    worker.can_spawn = false;
    for (name, timeout_secs) in [("web_fetch", 21), ("web_search", 22)] {
        worker.tools.insert(
            name.into(),
            crate::config::ToolConfig {
                timeout_secs: Some(timeout_secs),
                ..crate::config::ToolConfig::default()
            },
        );
    }
    profile.agents.insert("worker".into(), worker);

    for (agent_type, fetch_timeout, search_timeout) in [("default", 21, 22), ("worker", 31, 32)] {
        let (agent, _ui_rx) = factory.create(&profile, agent_type, Role::Root).unwrap();
        assert_eq!(
            agent.registry().get("web_fetch").unwrap().timeout_secs(),
            fetch_timeout
        );
        assert_eq!(
            agent.registry().get("web_search").unwrap().timeout_secs(),
            search_timeout
        );
        let call = agent.registry().get("web_fetch").unwrap().create_call(
            Uuid::new_v4(),
            serde_json::json!({"url": "https://example.com"}),
        );
        assert_eq!(call.timeout_secs(), fetch_timeout);
    }
}

#[test]
fn agent_web_timeouts_reach_the_http_backends() {
    crate::test_support::run_locked(async {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (received_requests, requests_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut sockets = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "client closed before sending its request");
                    request.extend_from_slice(&chunk[..count]);
                }
                sockets.push(socket);
            }
            received_requests.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        let (factory, mut profile) = make_factory();
        profile.web_search = toml::from_str(&format!(
            "backend = \"searxng\"\n[backends.searxng]\nbase_url = \"{base_url}\""
        ))
        .unwrap();
        profile.tools.insert(
            "web_fetch".into(),
            toml::from_str(
                "timeout_secs = 10\nrules = [{ rule = \"network_private_address\", verdict = \"auto_approved\" }]",
            )
            .unwrap(),
        );
        profile.tools.insert(
            "web_search".into(),
            toml::from_str("timeout_secs = 10").unwrap(),
        );
        let mut worker = profile.agents["default"].clone();
        worker.can_spawn = false;
        for name in ["web_fetch", "web_search"] {
            worker
                .tools
                .insert(name.into(), toml::from_str("timeout_secs = 1").unwrap());
        }
        profile.agents.insert("worker".into(), worker);
        let (agent, _ui_rx) = factory.create(&profile, "worker", Role::Root).unwrap();
        let fetch = agent.registry().get("web_fetch").unwrap().create_call(
            Uuid::new_v4(),
            serde_json::json!({"url": format!("{base_url}/page")}),
        );
        let search = agent
            .registry()
            .get("web_search")
            .unwrap()
            .create_call(Uuid::new_v4(), serde_json::json!({"terms": "timeout"}));
        let (fetch_result, search_result, requests_result) =
            tokio::time::timeout(Duration::from_secs(4), async {
                tokio::join!(fetch.run(), search.run(), requests_received)
            })
            .await
            .expect("backend HTTP deadlines must fire before the outer tool deadlines");
        server.abort();
        assert!(
            requests_result.is_ok(),
            "both requests must reach the server"
        );
        assert!(fetch_result.is_err(), "fetch must time out");
        assert!(search_result.is_err(), "search must time out");
    });
}

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
            allowed_skills: None,
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
fn root_and_subagent_contexts_share_global_and_agent_type_prompt_once() {
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
                    allowed_skills: None,
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
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let (mut subagent, _subagent_ui_rx) = factory
            .create(
                &profile,
                "specialist",
                Role::Subagent(SubagentRoleConfig {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    session_id: None,
                    briefing: Some("subagent briefing".to_string()),
                    parent_cancel: CancellationToken::new(),
                    subagent_ui_tx,
                    finish_tx,
                }),
            )
            .expect("subagent construction succeeds");
        let subagent_handle = subagent.run();
        subagent
            .send_command(crate::agent::AgentCommand::Prompt(
                "subagent task".to_string(),
            ))
            .await;
        let subagent_context = tokio::time::timeout(Duration::from_secs(2), subagent_handle)
            .await
            .expect("subagent run completes")
            .expect("subagent task does not panic")
            .expect("subagent task succeeds");
        let subagent_system_messages: Vec<_> = subagent_context
            .messages()
            .iter()
            .filter_map(|message| match message {
                Message::System { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            subagent_system_messages.first().copied(),
            Some("global soul\n\nspecialist persona")
        );
        assert_eq!(
            subagent_system_messages
                .iter()
                .filter(|content| content.contains("global soul"))
                .count(),
            1
        );
        assert!(subagent_system_messages.contains(&"subagent briefing"));
        assert!(subagent_system_messages
            .iter()
            .any(|content| content.contains("tool-use")));
    });
}

#[test]
fn create_honors_per_agent_type_allowed_tools_override() {
    // Regression test for Part A: `profile.agents.<agent_type>.allowed_tools`
    // must be honored per `agent_type`, not silently ignored in favor of
    // the factory-wide default list — so a distinct `agent_type` (e.g. a
    // restricted subagent role) can genuinely be limited to a different tool
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
            allowed_skills: None,
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
        crate::config::ToolConfig {
            rules: Some(vec![crate::sandbox::SandboxRuleConfig::Named {
                rule: crate::sandbox::NamedSandboxRule::OutsideWorkspace,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            }]),
            ..crate::config::ToolConfig::default()
        },
    );
    profile.agents.insert(
        "scratch-writer".to_string(),
        AgentConfig {
            engine: None,
            compaction: None,
            description: None,
            allowed_tools: None,
            allowed_skills: None,
            timeout_secs: None,
            max_tool_iterations: None,
            persona: None,
            can_spawn: false,
            tools: HashMap::from([(
                "write".to_string(),
                crate::config::ToolConfig {
                    rules: Some(vec![
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
                    ]),
                    ..crate::config::ToolConfig::default()
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
        default_write.needs_approval(&write_args).verdict,
        crate::sandbox::ApprovalVerdict::UserApprovalRequired,
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
        scratch_write.needs_approval(&write_args).verdict,
        crate::sandbox::ApprovalVerdict::AutoApproved,
        "the scratch-writer agent type's own tools.write table must win \
             over the profile-level table for that tool"
    );
}
