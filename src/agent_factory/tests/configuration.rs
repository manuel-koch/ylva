use super::*;

#[test]
fn create_returns_ok_for_root_role_with_known_engine() {
    let (factory, profile) = make_factory();
    let result = factory.create(&profile, "default", Role::Root);
    assert!(
        result.is_ok(),
        "expected Ok for Role::Root with known engine"
    );
    let (_agent_loop, ui_rx) = result.unwrap();
    // The channel should still be open (sender not dropped).
    assert!(!ui_rx.is_closed(), "UiUpdate receiver should be open");
}

#[test]
fn running_agent_preserves_compaction_engine_display_metadata() {
    crate::test_support::run_locked(async {
        let (factory, profile) = make_factory();
        let (mut agent, _ui_rx) = factory
            .create(&profile, "default", Role::Root)
            .expect("factory creates the root agent");
        let before_run = agent.engine_display_info();
        assert!(
            before_run
                .compaction_engine
                .as_ref()
                .is_some_and(|compaction| compaction.follows_conversation),
            "factory-created agents use their selected engine for compaction"
        );
        assert_eq!(
            before_run
                .compaction_engine
                .as_ref()
                .expect("compaction engine display")
                .context_length,
            agent.engine().effective_context_length()
        );

        let handle = agent.run();
        let after_run = agent.engine_display_info();
        assert!(
            after_run
                .compaction_engine
                .as_ref()
                .is_some_and(|compaction| compaction.follows_conversation),
            "starting the agent must not drop its compaction display metadata"
        );

        handle.abort();
        let _ = handle.await;
    });
}

#[test]
fn selected_root_runtime_uses_configured_engines_without_mutating_old_runtime() {
    crate::test_support::run_locked(async {
        let profile = two_engine_profile();
        let factory = factory_for_two_engine_profile(&profile);
        let (old_root, _) = factory.create(&profile, "default", Role::Root).unwrap();
        let old_engine = old_root.engine();
        old_engine.switch_model("live-model").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Store::create(
            directory.path(),
            directory.path(),
            uuid::Uuid::new_v4(),
            "mock",
            "persisted-model",
            Some("default"),
        )
        .unwrap();
        let (new_root, _) = factory
            .without_store()
            .create_root_for_store(&profile, "default", store)
            .unwrap();
        assert_eq!(new_root.engine().config().model, "main-model");
        assert_eq!(old_engine.config().model, "live-model");
        new_root.engine().switch_model("target-live-model").unwrap();
        assert_eq!(old_engine.config().model, "live-model");
        assert_eq!(
            factory
                .engine(&profile.default_engine)
                .unwrap()
                .config()
                .model,
            "live-model"
        );
    });
}

#[test]
fn from_profile_builds_every_configured_engine_and_honors_agent_overrides() {
    let profile = two_engine_profile();
    let factory = AgentFactory::from_profile(&profile).expect("the profile builds");

    let (root, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("the default agent type resolves to default_engine");
    assert_eq!(root.engine().config().model, "main-model");

    let (quick, _ui_rx) = factory
        .create(&profile, "quick", Role::Root)
        .expect("the overriding agent type resolves to its own engine");
    assert_eq!(quick.engine().config().model, "fast-model");
}

#[test]
fn implicit_summarizer_follows_root_named_engine_activation() {
    let profile = two_engine_profile();
    let factory = factory_for_two_engine_profile(&profile);
    let (root, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("root agent builds");
    let engine = root.engine();

    assert!(
        root.engine_display_info()
            .compaction_engine
            .expect("implicit summarizer display")
            .follows_conversation
    );
    let named_engine = engine
        .named_engine_info()
        .into_iter()
        .find(|named_engine| named_engine.name == "fast")
        .expect("fast shortcut");
    engine
        .select_model(&crate::llm::engine::ModelSelection::NamedEngine {
            engine_name: named_engine.name,
            option: crate::llm::ProviderModelOption {
                provider_connection: named_engine.config.provider_connection,
                model: crate::llm::ModelInfo {
                    id: named_engine.config.model,
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: named_engine.reasoning_effort,
            },
            reasoning_effort: crate::llm::engine::ReasoningEffortChange::Unchanged,
        })
        .expect("activate named engine");

    assert_eq!(engine.config().model, "fast-model");
    assert_eq!(
        root.compaction_engine_config()
            .expect("implicit summarizer remains enabled")
            .model,
        "fast-model",
        "an implicit summarizer must share the conversation router"
    );
    let display = root.engine_display_info();
    assert_eq!(
        display.compaction_engine.as_ref().unwrap().engine_name,
        "fast"
    );
    assert!(
        display
            .compaction_engine
            .as_ref()
            .unwrap()
            .follows_conversation
    );
    assert!(
        display
            .compaction_engine
            .as_ref()
            .unwrap()
            .shares_conversation
    );
    engine
        .switch_model_option(&crate::llm::ProviderModelOption {
            provider_connection: "mock".to_string(),
            model: crate::llm::ModelInfo {
                id: "fast-catalog-model".to_string(),
                context_length: Some(24_000),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        })
        .expect("raw model selection on the active connection");
    assert_eq!(
        root.compaction_engine_config()
            .expect("implicit summarizer remains enabled")
            .model,
        "fast-catalog-model",
        "implicit summaries must follow raw model changes too"
    );
}

#[test]
fn explicit_summarizer_stays_bound_when_conversation_engine_changes() {
    let mut profile = two_engine_profile();
    profile.compaction.engine = Some("main".to_string());
    profile
        .engines
        .get_mut("main")
        .expect("main engine")
        .context_length = Some(98_304);
    let factory = factory_for_two_engine_profile(&profile);
    let (root, _ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("root agent builds");
    let engine = root.engine();
    assert!(
        !root
            .engine_display_info()
            .compaction_engine
            .as_ref()
            .expect("explicit summarizer display")
            .shares_conversation
    );
    let named_engine = engine
        .named_engine_info()
        .into_iter()
        .find(|named_engine| named_engine.name == "fast")
        .expect("fast shortcut");
    engine
        .select_model(&crate::llm::engine::ModelSelection::NamedEngine {
            engine_name: named_engine.name,
            option: crate::llm::ProviderModelOption {
                provider_connection: named_engine.config.provider_connection,
                model: crate::llm::ModelInfo {
                    id: named_engine.config.model,
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: named_engine.reasoning_effort,
            },
            reasoning_effort: crate::llm::engine::ReasoningEffortChange::Unchanged,
        })
        .expect("activate named engine");

    assert_eq!(engine.config().model, "fast-model");
    assert_eq!(
        root.compaction_engine_config()
            .expect("explicit summarizer remains enabled")
            .model,
        "main-model"
    );
    let display = root.engine_display_info();
    let compaction = display
        .compaction_engine
        .as_ref()
        .expect("explicit summarizer display");
    assert_eq!(compaction.engine_name, "main");
    assert!(!compaction.follows_conversation);
    assert!(!compaction.shares_conversation);

    let main_engine = engine
        .named_engine_info()
        .into_iter()
        .find(|named_engine| named_engine.name == "main")
        .expect("main shortcut");
    engine
        .select_model(&crate::llm::engine::ModelSelection::NamedEngine {
            engine_name: main_engine.name,
            option: crate::llm::ProviderModelOption {
                provider_connection: main_engine.config.provider_connection,
                model: crate::llm::ModelInfo {
                    id: main_engine.config.model,
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: main_engine.reasoning_effort,
            },
            reasoning_effort: crate::llm::engine::ReasoningEffortChange::Unchanged,
        })
        .expect("reactivate the explicit summarizer engine");
    engine
        .switch_model_option(&crate::llm::ProviderModelOption {
            provider_connection: "mock".to_string(),
            model: crate::llm::ModelInfo {
                id: "main-catalog-model".to_string(),
                context_length: Some(32_000),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        })
        .expect("mutate the named explicit summarizer");
    assert_eq!(
        root.compaction_engine_config()
            .expect("explicit summarizer remains enabled")
            .model,
        "main-model",
        "an explicit summarizer keeps its configured model despite conversation mutations"
    );
    assert_eq!(engine.effective_context_length(), 32_000);
    assert_eq!(
        root.engine_display_info()
            .compaction_engine
            .as_ref()
            .expect("summarizer display")
            .context_length,
        98_304,
    );
    assert!(
        !root
            .engine_display_info()
            .compaction_engine
            .expect("summarizer display")
            .shares_conversation
    );
}

#[test]
fn per_agent_compaction_engine_and_timeout_are_applied_to_child_loops() {
    let mut profile = two_engine_profile();
    profile.engines.insert(
        "summary".to_string(),
        EngineConfig {
            model: "summary-model".to_string(),
            ..test_engine_config()
        },
    );
    profile.compaction.engine = Some("main".to_string());
    profile.compaction.timeout_sec = 90;
    profile
        .agents
        .get_mut("quick")
        .expect("quick agent")
        .compaction = Some(crate::config::AgentCompactionConfig {
        engine: Some("summary".to_string()),
        timeout_sec: Some(240),
    });
    let factory = AgentFactory::from_profile(&profile).expect("profile engines validate");
    let child_role = Role::Child(ChildRoleConfig {
        run_id: Uuid::new_v4(),
        call_id: Uuid::new_v4(),
        session_id: None,
        briefing: None,
        parent_cancel: CancellationToken::new(),
        subagent_ui_tx: mpsc::channel(4).0,
        finish_tx: mpsc::channel(4).0,
    });

    let (child, _ui_rx) = factory
        .create(&profile, "quick", child_role)
        .expect("child agent builds with its compaction overrides");

    assert_eq!(child.engine().config().model, "fast-model");
    assert_eq!(
        child
            .compaction_engine_config()
            .expect("child compactor")
            .model,
        "summary-model"
    );
    assert_eq!(child.compaction_timeout(), Duration::from_secs(240));
}

#[test]
fn startup_checks_unused_agent_types_with_their_independent_compaction_engines() {
    crate::test_support::run_locked(async {
        for dedicated_summarizer in [false, true] {
            let mut profile = two_engine_profile();
            profile.compaction.engine = Some("main".to_string());
            profile.engines.get_mut("main").unwrap().context_length = Some(32_768);
            profile.engines.get_mut("fast").unwrap().context_length = Some(98_304);
            if dedicated_summarizer {
                profile.agents.get_mut("quick").unwrap().compaction =
                    Some(crate::config::AgentCompactionConfig {
                        engine: Some("fast".to_string()),
                        timeout_sec: None,
                    });
            }
            let factory = AgentFactory::from_profile(&profile).expect("profile engines validate");
            let (mut root, mut updates) = factory
                .create(&profile, "default", Role::Root)
                .expect("root builds");
            let handle = root.run();
            if dedicated_summarizer {
                tokio::time::sleep(Duration::from_millis(50)).await;
                while let Ok(update) = updates.try_recv() {
                    assert!(
                        !matches!(update, UiUpdate::Notice { .. }),
                        "a dedicated sufficiently large agent summarizer must not warn, got {update:?}",
                    );
                }
            } else {
                let update = tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        match updates.recv().await {
                            Some(UiUpdate::ContextUsage { .. }) => continue,
                            other => break other,
                        }
                    }
                })
                .await
                .expect("an unused agent must be checked without any prompt")
                .expect("startup notice");
                assert!(matches!(update, UiUpdate::Notice { text, run_id: None }
                            if text.contains("Agent type 'quick'")
                                && text.contains("68812 tokens")
                                && text.contains("32768 tokens")),);
            }
            drop(root);
            handle.await.expect("loop joins").expect("loop shuts down");
        }
    });
}

#[test]
fn startup_groups_equivalent_capacity_warnings_without_merging_different_budgets() {
    crate::test_support::run_locked(async {
        for (root_shares_engine, different_window, different_threshold, different_summary_window) in [
            (false, false, false, false),
            (true, false, false, false),
            (true, true, false, false),
            (true, false, true, false),
            (true, false, false, true),
        ] {
            let mut profile = two_engine_profile();
            profile.compaction.engine = Some("main".to_string());
            for (engine_name, context_length) in [("main", 32_768), ("fast", 131_072)] {
                let config = profile.engines.get_mut(engine_name).unwrap();
                config.model = "shared-model".to_string();
                config.context_length = Some(context_length);
            }
            let mut research_agent = profile.agents["quick"].clone();
            if root_shares_engine {
                profile.agents.get_mut("default").unwrap().engine = Some("fast".to_string());
            }
            if different_window || different_threshold {
                let mut different_engine = profile.engines["fast"].clone();
                if different_window {
                    different_engine.context_length = Some(65_536);
                }
                if different_threshold {
                    different_engine.soft_compaction_threshold = 0.6;
                }
                profile
                    .engines
                    .insert("different".to_string(), different_engine);
                research_agent.engine = Some("different".to_string());
            }
            if different_summary_window {
                let mut smaller_summary_engine = profile.engines["main"].clone();
                smaller_summary_engine.context_length = Some(16_384);
                profile
                    .engines
                    .insert("smaller-summary".to_string(), smaller_summary_engine);
                research_agent.compaction = Some(crate::config::AgentCompactionConfig {
                    engine: Some("smaller-summary".to_string()),
                    timeout_sec: None,
                });
            }
            profile
                .agents
                .insert("research-and-discover".to_string(), research_agent);
            let factory = AgentFactory::from_profile(&profile).expect("profile validates");
            let (mut root, mut updates) = factory
                .create(&profile, "default", Role::Root)
                .expect("root builds");
            let handle = root.run();
            let first_update = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    match updates.recv().await {
                        Some(UiUpdate::ContextUsage { .. }) => continue,
                        other => break other,
                    }
                }
            })
            .await
            .expect("startup warning arrives before any prompt")
            .expect("startup notice");
            let mut notices = Vec::new();
            if let UiUpdate::Notice { text, run_id: None } = first_update {
                notices.push(text);
            } else {
                panic!("expected a root startup notice, got {first_update:?}");
            }
            root.send_command(crate::agent::AgentCommand::Prompt("hello".to_string()))
                .await;
            tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(update) = updates.recv().await {
                    match update {
                        UiUpdate::Notice { text, run_id: None } => notices.push(text),
                        UiUpdate::StreamDone => return,
                        _ => {}
                    }
                }
                panic!("UI channel closed before the first turn completed");
            })
            .await
            .expect("first turn completes after all startup checks");
            drop(root);
            handle.await.expect("loop joins").expect("loop shuts down");
            let different_budget =
                different_window || different_threshold || different_summary_window;
            assert_eq!(
                notices.len(),
                if different_budget { 2 } else { 1 },
                "equivalent startup warnings must share one notice: {notices:?}",
            );
            assert!(notices[0].contains("Agent type 'quick'"), "{notices:?}");
            assert_eq!(
                notices[0].contains("Agent type 'default'"),
                root_shares_engine,
                "{notices:?}",
            );
            if different_budget {
                assert!(!notices[0].contains("research-and-discover"), "{notices:?}");
                assert!(notices[1].contains("research-and-discover"), "{notices:?}");
                if different_window {
                    assert!(notices[1].contains("65536"), "{notices:?}");
                }
                if different_threshold {
                    assert!(notices[1].contains("0.6"), "{notices:?}");
                }
                if different_summary_window {
                    assert!(notices[1].contains("16384"), "{notices:?}");
                }
            } else {
                assert!(notices[0].contains("research-and-discover"), "{notices:?}");
            }
        }
    });
}

#[test]
fn unknown_agent_compaction_engine_is_rejected_at_factory_creation() {
    let mut profile = two_engine_profile();
    profile
        .agents
        .get_mut("quick")
        .expect("quick agent")
        .compaction = Some(crate::config::AgentCompactionConfig {
        engine: Some("missing-summary".to_string()),
        timeout_sec: None,
    });

    let error = match AgentFactory::from_profile(&profile) {
        Ok(_) => panic!("per-agent compaction engine must exist"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("missing-summary"), "{error}");
    assert!(error.to_string().contains("quick"), "{error}");
}

#[test]
fn profile_compaction_settings_apply_to_child_without_agent_override() {
    let mut profile = two_engine_profile();
    profile.compaction.engine = Some("fast".to_string());
    profile.compaction.timeout_sec = 180;
    let factory = AgentFactory::new(HashMap::from([
        (
            "main".to_string(),
            Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(vec![], Duration::ZERO)),
                test_engine_config(),
            )) as Arc<dyn Engine>,
        ),
        (
            "fast".to_string(),
            Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(vec![], Duration::ZERO)),
                test_engine_config(),
            )) as Arc<dyn Engine>,
        ),
    ]));
    let child_role = Role::Child(ChildRoleConfig {
        run_id: Uuid::new_v4(),
        call_id: Uuid::new_v4(),
        session_id: None,
        briefing: None,
        parent_cancel: CancellationToken::new(),
        subagent_ui_tx: mpsc::channel(4).0,
        finish_tx: mpsc::channel(4).0,
    });

    let (child, _ui_rx) = factory
        .create(&profile, "quick", child_role)
        .expect("child inherits profile compaction configuration");
    assert_eq!(
        child.compaction_engine_config().unwrap().model,
        "test-model"
    );
    assert_eq!(child.compaction_timeout(), Duration::from_secs(180));
}

#[test]
fn from_profile_resolves_multiple_connections_using_one_provider_kind() {
    let profile = ProfileConfig {
        default_engine: "ollama/local".to_string(),
        provider_connections: HashMap::from([
            (
                "ollama-local".to_string(),
                crate::ProviderConnectionConfig {
                    provider: "ollama".to_string(),
                    api_base_url: Some("http://localhost:11434".to_string()),
                    api_key: None,
                },
            ),
            (
                "ollama-cloud".to_string(),
                crate::ProviderConnectionConfig {
                    provider: "ollama".to_string(),
                    api_base_url: Some("https://ollama.com".to_string()),
                    api_key: Some("cloud-secret".to_string()),
                },
            ),
        ]),
        engines: HashMap::from([
            (
                "ollama/local".to_string(),
                EngineConfig {
                    provider_connection: "ollama-local".to_string(),
                    model: "local-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                },
            ),
            (
                "ollama/cloud".to_string(),
                EngineConfig {
                    provider_connection: "ollama-cloud".to_string(),
                    model: "cloud-model".to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
                },
            ),
        ]),
        ..ProfileConfig::default()
    };

    let factory = AgentFactory::from_profile(&profile).expect("the profile builds");
    let local = factory
        .engine_config("ollama/local")
        .expect("the local engine exists");
    let cloud = factory
        .engine_config("ollama/cloud")
        .expect("the cloud engine exists");

    assert_eq!(local.provider_connection, "ollama-local");
    assert_eq!(
        local.api_base_url.as_deref(),
        Some("http://localhost:11434")
    );
    assert_eq!(cloud.provider_connection, "ollama-cloud");
    assert_eq!(cloud.api_base_url.as_deref(), Some("https://ollama.com"));
}

#[test]
fn from_profile_rejects_a_default_engine_no_table_defines() {
    let mut profile = two_engine_profile();
    profile.default_engine = "missing".to_string();
    let error = AgentFactory::from_profile(&profile)
        .err()
        .expect("an undefined default_engine must fail loudly");
    assert!(error.to_string().contains("missing"), "{error}");
}

#[test]
fn from_profile_rejects_an_undefined_utility_engine() {
    let mut profile = two_engine_profile();
    profile.utility_engine = Some("missing-utility".to_string());
    let error = AgentFactory::from_profile(&profile)
        .err()
        .expect("an undefined utility engine must fail loudly");
    assert!(error.to_string().contains("missing-utility"), "{error}");
}

#[test]
fn from_profile_requires_an_api_base_url_for_an_http_provider() {
    let profile = ProfileConfig {
        default_engine: "remote".to_string(),
        engines: HashMap::from([(
            "remote".to_string(),
            EngineConfig {
                provider_connection: "ollama".to_string(),
                model: "some-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        )]),
        ..ProfileConfig::default()
    };
    let error = AgentFactory::from_profile(&profile)
        .err()
        .expect("an http provider without a base url must fail loudly");
    assert!(error.to_string().contains("api_base_url"), "{error}");
}

#[test]
fn create_returns_err_for_unknown_engine() {
    let (factory, _) = make_factory();
    let profile = ProfileConfig {
        default_engine: "nonexistent".to_string(),
        agents: HashMap::new(),
        ..ProfileConfig::default()
    };
    let result = factory.create(&profile, "default", Role::Root);
    assert!(result.is_err(), "expected Err for unknown engine name");
    let error_message = result.err().unwrap().to_string();
    assert!(
        error_message.contains("nonexistent"),
        "error should mention the unknown engine name"
    );
}

#[test]
fn validate_profile_agent_engines_rejects_unknown_profile_level_tool_name() {
    let (factory, mut profile) = make_factory();
    profile.tools.insert(
        "not-a-real-tool".to_string(),
        crate::config::ToolSandboxConfig { rules: vec![] },
    );
    let error = factory
        .validate_profile_agent_engines(&profile)
        .expect_err("an unknown profile-level tool name must fail startup validation");
    let message = error.to_string();
    assert!(message.contains("not-a-real-tool"), "{message}");
    assert!(
        message.contains("profile-level") || message.contains("[tools."),
        "{message}"
    );
}

#[test]
fn validate_profile_agent_engines_rejects_unknown_agent_type_scoped_tool_name() {
    let (factory, mut profile) = make_factory();
    profile.agents.insert(
        "custom".to_string(),
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
                "not-a-real-tool".to_string(),
                crate::config::ToolSandboxConfig { rules: vec![] },
            )]),
        },
    );
    let error = factory
        .validate_profile_agent_engines(&profile)
        .expect_err("an unknown agent-type-scoped tool name must fail startup validation");
    let message = error.to_string();
    assert!(message.contains("not-a-real-tool"), "{message}");
    assert!(message.contains("custom"), "{message}");
}

#[test]
fn create_returns_ok_for_child_role_with_known_engine() {
    let (factory, profile) = make_factory();
    let (finish_tx, _finish_rx) = mpsc::channel(1);
    let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(1);
    let child_role_config = ChildRoleConfig {
        run_id: uuid::Uuid::new_v4(),
        call_id: uuid::Uuid::new_v4(),
        session_id: None,
        briefing: Some("briefing text".to_string()),
        parent_cancel: CancellationToken::new(),
        subagent_ui_tx,
        finish_tx,
    };
    let result = factory.create(&profile, "default", Role::Child(child_role_config));
    assert!(
        result.is_ok(),
        "expected Ok for Role::Child with known engine"
    );
    let (_agent_loop, ui_rx) = result.unwrap();
    assert!(!ui_rx.is_closed(), "UiUpdate receiver should be open");
}

#[test]
fn create_uses_agent_config_engine_when_present() {
    let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
    let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
    let factory = AgentFactory::new(HashMap::from([("custom".to_string(), engine)]));
    let profile = ProfileConfig {
        default_engine: "nonexistent".to_string(),
        compaction: crate::config::CompactionConfig::default(),
        agents: HashMap::from([(
            "myagent".to_string(),
            AgentConfig {
                engine: Some("custom".to_string()),
                compaction: Some(crate::config::AgentCompactionConfig {
                    engine: None,
                    timeout_sec: Some(123),
                }),
                description: None,
                allowed_tools: None,
                timeout_secs: None,
                max_tool_iterations: None,
                persona: None,
                can_spawn: false,
                tools: HashMap::new(),
            },
        )]),
        ..ProfileConfig::default()
    };
    let (agent, _ui_rx) = factory
        .create(&profile, "myagent", Role::Root)
        .expect("should resolve engine from AgentConfig.engine override");
    let display = agent.engine_display_info();
    assert_eq!(display.engine_name, "custom");
    assert_eq!(
        display
            .compaction_engine
            .as_ref()
            .expect("implicit summarizer display")
            .engine_name,
        "custom",
        "an implicit summarizer follows a root agent's engine override"
    );
    assert!(
        display
            .compaction_engine
            .as_ref()
            .expect("implicit summarizer display")
            .follows_conversation
    );
    assert_eq!(
        agent.compaction_timeout(),
        Duration::from_secs(123),
        "an independent timeout override applies without binding a summarizer engine"
    );
}
