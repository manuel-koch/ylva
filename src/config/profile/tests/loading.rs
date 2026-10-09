use super::*;

#[test]
fn a_toml_fixture_loads_into_the_expected_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "work",
        r#"
default_engine = "fast"
default_model = "gemma4:e4b-mlx"
theme = "light"
input_history_limit = 25

[compaction]
engine = "cheap"
timeout_sec = 180

[keybindings]
submit = "enter"

[engines.fast]
provider = "ollama"
api_base_url = "http://localhost:11434"
temperature = 0.2

[engines.cheap]
provider = "openrouter"
model = "some/cheap-model"
api_base_url = "https://openrouter.ai"
context_length = 32000

[engines.thinking]
provider = "openrouter"
model = "some/thinking-model"
api_base_url = "https://openrouter.ai"
context_length = "265kb"
reasoning = "xhigh"

[agents.default]
engine = "fast"
description = "General-purpose agent for ordinary tasks."
allowed_tools = ["read", "glob"]
can_spawn = true

[agents.explorer]
timeout_secs = 120
persona = "You explore code."
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "work").expect("the fixture loads");

    assert_eq!(profile.default_engine, "fast");
    assert_eq!(profile.compaction_engine_name(), "cheap");
    assert_eq!(profile.compaction.timeout_sec, 180);
    assert_eq!(profile.theme, ThemeName::Light);
    assert_eq!(profile.input_history_limit, 25);
    assert_eq!(
        profile.keybindings.get("submit").map(String::as_str),
        Some("enter")
    );

    let fast = profile.engines.get("fast").expect("the fast engine");
    assert_eq!(fast.provider_connection, "ollama");
    assert_eq!(
        fast.model, "gemma4:e4b-mlx",
        "an engine naming no model inherits default_model"
    );
    assert_eq!(fast.temperature, Some(0.2));
    assert_eq!(
        fast.context_length, None,
        "an unset context_length in TOML stays None; it is resolved lazily \
             against the provider catalog and only then the built-in default"
    );

    let cheap = profile.engines.get("cheap").expect("the cheap engine");
    assert_eq!(cheap.model, "some/cheap-model", "an explicit model wins");
    assert_eq!(cheap.context_length, Some(32_000));

    let thinking = profile
        .engines
        .get("thinking")
        .expect("the thinking engine");
    assert_eq!(
        thinking.context_length,
        Some(265 * 1024),
        "a 'kb'-suffixed context_length expands using 1024-based units"
    );
    assert_eq!(
        thinking.reasoning.as_deref(),
        Some("xhigh"),
        "an explicit reasoning field seeds the engine's initial reasoning effort"
    );

    let default_agent = profile.agents.get("default").expect("the default agent");
    assert_eq!(default_agent.engine.as_deref(), Some("fast"));
    assert_eq!(
        default_agent.description.as_deref(),
        Some("General-purpose agent for ordinary tasks.")
    );
    assert!(default_agent.can_spawn);
    let explorer = profile.agents.get("explorer").expect("the explorer agent");
    assert_eq!(explorer.timeout_secs, Some(120));
    assert!(
        !explorer.can_spawn,
        "can_spawn defaults to false when the table omits it"
    );
}

#[test]
fn named_provider_connections_and_environment_interpolation_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "multiple",
        r#"
default_engine = "ollama/local"

[provider_connections.ollama-local]
provider = "ollama"
api_base_url = "${OLLAMA_LOCAL_URL}"

[provider_connections.ollama-cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${OLLAMA_CLOUD_API_KEY}"

[engines."ollama/local"]
provider_connection = "ollama-local"
model = "local-model"

[engines."ollama/cloud"]
provider_connection = "ollama-cloud"
model = "cloud-model"
"#,
    );
    let environment = HashMap::from([
        (
            "OLLAMA_LOCAL_URL".to_string(),
            "http://localhost:11434".to_string(),
        ),
        (
            "OLLAMA_CLOUD_API_KEY".to_string(),
            "cloud-secret".to_string(),
        ),
    ]);

    let profile = ProfileConfig::load_with_environment(temp.path(), "multiple", &environment)
        .expect("the multiple-provider profile loads");

    assert_eq!(profile.provider_connections.len(), 2);
    assert_eq!(
        profile.provider_connections["ollama-local"].provider,
        "ollama"
    );
    assert_eq!(
        profile.provider_connections["ollama-local"]
            .api_base_url
            .as_deref(),
        Some("http://localhost:11434")
    );
    assert_eq!(
        profile.provider_connections["ollama-cloud"]
            .api_key
            .as_deref(),
        Some("cloud-secret")
    );
    assert_eq!(
        profile.engines["ollama/local"].provider_connection,
        "ollama-local"
    );
}

#[test]
fn numeric_and_boolean_environment_interpolation_loads() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "typed-values",
        r#"
input_history_limit = "${INPUT_HISTORY_LIMIT}"

[engines.local]
provider = "ollama"
temperature = "${TEMPERATURE}"
context_length = "${CONTEXT_LENGTH}"

[agents.worker]
timeout_secs = "${TIMEOUT_SECONDS}"
can_spawn = "${CAN_SPAWN}"
"#,
    );
    let environment = HashMap::from([
        ("TEMPERATURE".to_string(), "0.2".to_string()),
        ("CONTEXT_LENGTH".to_string(), "128kb".to_string()),
        ("INPUT_HISTORY_LIMIT".to_string(), "17".to_string()),
        ("TIMEOUT_SECONDS".to_string(), "120".to_string()),
        ("CAN_SPAWN".to_string(), "true".to_string()),
    ]);

    let profile = ProfileConfig::load_with_environment(temp.path(), "typed-values", &environment)
        .expect("the typed-value profile loads");

    assert_eq!(profile.engines["local"].temperature, Some(0.2));
    assert_eq!(profile.engines["local"].context_length, Some(128 * 1024));
    assert_eq!(profile.input_history_limit, 17);
    assert_eq!(profile.agents["worker"].timeout_secs, Some(120));
    assert!(profile.agents["worker"].can_spawn);
}

#[test]
fn missing_environment_interpolation_is_a_profile_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "missing-variable",
        r#"
[provider_connections.cloud]
provider = "ollama"
api_base_url = "https://ollama.com"
api_key = "${MISSING_API_KEY}"
"#,
    );

    let error =
        ProfileConfig::load_with_environment(temp.path(), "missing-variable", &HashMap::new())
            .expect_err("missing variables must fail profile loading");

    assert!(error.to_string().contains("MISSING_API_KEY"), "{error}");
}

#[test]
fn a_missing_config_file_falls_back_to_the_default_profile_only_via_load_or_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = ProfileConfig::load_or_default(temp.path(), "never-written")
        .expect("a missing file is not an error for the implicit default profile");
    assert_eq!(profile.default_engine, DEFAULT_PROFILE_NAME);
    assert!(profile.engines.is_empty());
    assert_eq!(profile.theme, ThemeName::Dark);
    assert!(
        profile
            .agents
            .get(DEFAULT_PROFILE_NAME)
            .expect("a default agent type")
            .can_spawn
    );
}

#[test]
fn the_code_default_profile_is_the_implicit_floor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
    let profile = ProfileConfig::load_or_default_with_environment_at_workspace_root(
        &profiles_root,
        DEFAULT_PROFILE_NAME,
        &HashMap::new(),
        None,
    )
    .expect("the code default profile is used");

    assert_eq!(profile.default_engine, DEFAULT_PROFILE_NAME);
    assert!(profile.engines.is_empty());
    assert_eq!(profile.theme, ThemeName::Dark);
    assert_eq!(profile.input_history_limit, 50);
}

#[test]
fn project_profile_replaces_the_home_profile_for_the_same_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home_profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    write_profile_config(
        &home_profiles_root,
        DEFAULT_PROFILE_NAME,
        "theme = \"light\"\n",
    );
    let project_config = workspace_root
        .join(".ylva")
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME);
    std::fs::create_dir_all(project_config.parent().expect("project config parent"))
        .expect("mkdir");
    std::fs::write(&project_config, "theme = \"dark\"\n").expect("write project config");

    let profile = ProfileConfig::load_with_environment_at_workspace_root(
        &home_profiles_root,
        DEFAULT_PROFILE_NAME,
        &HashMap::new(),
        Some(&workspace_root),
    )
    .expect("the project profile loads");

    assert_eq!(profile.theme, ThemeName::Dark);
}

#[test]
fn global_agent_type_file_is_loaded_for_a_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
    write_agent_type_file(
        &home,
        "agents/research.md",
        "+++\ncan_spawn = false\n+++\nInvestigate evidence.\n",
    );

    let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
    let research = profile.agents.get("research").expect("global agent type");
    assert!(!research.can_spawn);
    assert_eq!(research.persona.as_deref(), Some("Investigate evidence."));
}

#[test]
fn malformed_markdown_agent_type_file_is_a_named_load_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
    write_agent_type_file(
        &home,
        "agents/research.md",
        "This file is missing frontmatter.\n",
    );

    let error =
        ProfileConfig::load(&profiles_root, "work").expect_err("malformed agent file fails");
    let message = error.to_string();
    assert!(message.contains("malformed agent-type file"), "{message}");
    assert!(message.contains("research.md"), "{message}");
}

#[test]
fn profile_agent_type_file_shadows_global_only_for_that_profile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(&profiles_root, "work", "theme = \"dark\"\n");
    write_profile_config(&profiles_root, "other", "theme = \"dark\"\n");
    write_agent_type_file(
        &home,
        "agents/research.toml",
        "persona = \"global\"\ncan_spawn = true\n",
    );
    write_agent_type_file(
        &home,
        "profiles/work/agents/research.toml",
        "persona = \"profile\"\ncan_spawn = false\n",
    );

    let work = ProfileConfig::load(&profiles_root, "work").expect("work profile loads");
    let other = ProfileConfig::load(&profiles_root, "other").expect("other profile loads");
    assert_eq!(work.agents["research"].persona.as_deref(), Some("profile"));
    assert!(!work.agents["research"].can_spawn);
    assert_eq!(other.agents["research"].persona.as_deref(), Some("global"));
    assert!(other.agents["research"].can_spawn);
}

#[test]
fn project_agent_type_file_shadows_profile_and_global_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    write_profile_config(&profiles_root, DEFAULT_PROFILE_NAME, "theme = \"dark\"\n");
    let project_config = workspace_root
        .join(YLVA_DIR_NAME)
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME);
    std::fs::create_dir_all(project_config.parent().expect("project config parent"))
        .expect("mkdir");
    std::fs::write(&project_config, "theme = \"dark\"\n").expect("write project config");
    write_agent_type_file(&home, "agents/research.toml", "persona = \"global\"\n");
    write_agent_type_file(
        &home,
        "profiles/default/agents/research.toml",
        "persona = \"profile\"\n",
    );
    write_agent_type_file(
        &workspace_root,
        ".ylva/agents/research.toml",
        "persona = \"project\"\n",
    );

    let profile = ProfileConfig::load_with_environment_at_workspace_root(
        &profiles_root,
        DEFAULT_PROFILE_NAME,
        &HashMap::new(),
        Some(&workspace_root),
    )
    .expect("project profile loads");
    assert_eq!(
        profile.agents["research"].persona.as_deref(),
        Some("project")
    );
}

#[test]
fn project_environment_is_used_for_project_profile_interpolation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    write_profile_config(&profiles_root, DEFAULT_PROFILE_NAME, "theme = \"light\"\n");

    let project_config = workspace_root
        .join(YLVA_DIR_NAME)
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME);
    std::fs::create_dir_all(project_config.parent().expect("project config parent"))
        .expect("mkdir");
    std::fs::write(
        &project_config,
        r#"
[provider_connections.project]
provider = "ollama"
api_base_url = "${YLVA_PHASE10_2_PROJECT_URL}"
"#,
    )
    .expect("write project config");

    let project_environment = workspace_root
        .join(YLVA_DIR_NAME)
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_ENV_FILE_NAME);
    std::fs::create_dir_all(project_environment.parent().expect("project env parent"))
        .expect("mkdir");
    std::fs::write(
        &project_environment,
        "YLVA_PHASE10_2_PROJECT_URL=http://project.example\n",
    )
    .expect("write project environment");

    let environment = load_profile_environment_at_workspace_root(
        Some(&profile_env_path(&profiles_root, DEFAULT_PROFILE_NAME)),
        Some(&workspace_root),
        DEFAULT_PROFILE_NAME,
    )
    .expect("load project environment");
    let profile = ProfileConfig::load_with_environment_at_workspace_root(
        &profiles_root,
        DEFAULT_PROFILE_NAME,
        &environment,
        Some(&workspace_root),
    )
    .expect("project profile loads");

    assert_eq!(
        profile.provider_connections["project"]
            .api_base_url
            .as_deref(),
        Some("http://project.example")
    );
}

#[test]
fn inline_agent_fields_override_a_file_definition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        r#"
[agents.research]
persona = "Be concise."
timeout_secs = 30

[agents.research.compaction]
engine = "summary"
timeout_sec = 240
"#,
    );
    write_agent_type_file(
        &temp.path().join("home"),
        "agents/research.md",
        "+++\ncan_spawn = true\n+++\nInvestigate evidence.\n",
    );

    let profile = ProfileConfig::load(&profiles_root, "work").expect("profile loads");
    let research = profile.agents.get("research").expect("research agent type");
    assert_eq!(
        research.persona.as_deref(),
        Some("Be concise."),
        "the inline persona replaces the file-sourced persona"
    );
    assert_eq!(research.timeout_secs, Some(30));
    assert_eq!(
        research
            .compaction
            .as_ref()
            .and_then(|settings| settings.engine.as_deref()),
        Some("summary")
    );
    assert_eq!(
        research
            .compaction
            .as_ref()
            .and_then(|settings| settings.timeout_sec),
        Some(240)
    );
    assert!(research.can_spawn);
}

#[test]
fn same_scope_project_agent_file_and_inline_table_are_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    let project_config = workspace_root
        .join(YLVA_DIR_NAME)
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME);
    std::fs::create_dir_all(project_config.parent().expect("project config parent"))
        .expect("mkdir");
    std::fs::write(&project_config, "[agents.research]\npersona = \"inline\"\n")
        .expect("write project config");
    write_agent_type_file(
        &workspace_root,
        ".ylva/agents/research.md",
        "+++\n+++\nfile body\n",
    );

    let error = ProfileConfig::load_with_environment_at_workspace_root(
        &profiles_root,
        DEFAULT_PROFILE_NAME,
        &HashMap::new(),
        Some(&workspace_root),
    )
    .expect_err("same-scope project definitions must conflict");
    let message = error.to_string();
    assert!(message.contains("research"), "{message}");
    assert!(message.contains("project"), "{message}");
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("research.md"), "{message}");
}

#[test]
fn same_scope_profile_agent_file_and_inline_table_are_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "[agents.research]\npersona = \"inline\"\n",
    );
    write_agent_type_file(
        &home,
        "profiles/work/agents/research.md",
        "+++\n+++\nfile body\n",
    );

    let error = ProfileConfig::load(&profiles_root, "work")
        .expect_err("same-scope profile definitions must conflict");
    let message = error.to_string();
    assert!(message.contains("research"), "{message}");
    assert!(message.contains("profile"), "{message}");
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("research.md"), "{message}");
}

#[test]
fn inline_profile_override_of_a_global_agent_type_is_allowed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let profiles_root = home.join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "[agents.research]\npersona = \"Use concise findings.\"\n",
    );
    write_agent_type_file(&home, "agents/research.md", "+++\n+++\nglobal body\n");

    let profile = ProfileConfig::load(&profiles_root, "work")
        .expect("cross-scope inline adaptation is valid");
    assert_eq!(
        profile.agents["research"].persona.as_deref(),
        Some("Use concise findings."),
        "the inline persona replaces the file-sourced persona"
    );
}

#[test]
fn missing_config_fallback_still_loads_project_agent_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    write_agent_type_file(
        &workspace_root,
        ".ylva/agents/research.toml",
        "persona = \"project body\"\n",
    );

    let profile = ProfileConfig::load_or_default_with_environment_at_workspace_root(
        &profiles_root,
        DEFAULT_PROFILE_NAME,
        &HashMap::new(),
        Some(&workspace_root),
    )
    .expect("missing default config falls back");
    assert_eq!(
        profile.agents["research"].persona.as_deref(),
        Some("project body")
    );
    assert!(
        profile.agents[DEFAULT_PROFILE_NAME].can_spawn,
        "the code-owned default remains available"
    );
}

#[test]
fn profile_config_source_path_matches_profile_layer_precedence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home_profiles_root = temp.path().join("home").join(PROFILES_DIRECTORY_NAME);
    let workspace_root = temp.path().join("project");
    write_profile_config(
        &home_profiles_root,
        DEFAULT_PROFILE_NAME,
        "theme = \"light\"\n",
    );
    let workspace_config = workspace_root
        .join(YLVA_DIR_NAME)
        .join(PROFILES_DIRECTORY_NAME)
        .join(DEFAULT_PROFILE_NAME)
        .join(PROFILE_CONFIG_FILE_NAME);
    std::fs::create_dir_all(workspace_config.parent().expect("workspace config parent"))
        .expect("mkdir");
    std::fs::write(&workspace_config, "theme = \"dark\"\n").expect("write workspace config");

    assert_eq!(
        profile_config_source_path(
            &home_profiles_root,
            DEFAULT_PROFILE_NAME,
            Some(&workspace_root)
        )
        .expect("resolve workspace profile path"),
        Some(workspace_config.clone())
    );

    std::fs::remove_file(&workspace_config).expect("remove workspace config");
    assert_eq!(
        profile_config_source_path(
            &home_profiles_root,
            DEFAULT_PROFILE_NAME,
            Some(&workspace_root)
        )
        .expect("resolve home profile path"),
        Some(
            home_profiles_root
                .join(DEFAULT_PROFILE_NAME)
                .join(PROFILE_CONFIG_FILE_NAME)
        )
    );
}

#[test]
fn an_explicitly_named_missing_profile_is_a_hard_error_via_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    let error = ProfileConfig::load(temp.path(), "never-written").expect_err(
        "an explicitly requested profile that does not exist must not silently fall back",
    );
    assert!(
        matches!(&error, ConfigError::ProfileNotFound { profile_name, .. } if profile_name == "never-written"),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("never-written"), "{message}");
    assert!(message.contains("not found"), "{message}");
}

#[test]
fn a_malformed_config_file_is_a_clear_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(temp.path(), "broken", "default_engine = [not toml");
    let error = ProfileConfig::load(temp.path(), "broken")
        .expect_err("malformed TOML must not be silently ignored");
    let message = error.to_string();
    assert!(message.contains("malformed profile config"), "{message}");
    assert!(message.contains("config.toml"), "{message}");
}

#[test]
fn a_config_file_without_agents_still_gets_the_default_agent_type() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(temp.path(), "bare", "theme = \"dark\"\n");
    let profile = ProfileConfig::load(temp.path(), "bare").expect("the fixture loads");
    assert!(profile.agents.contains_key(DEFAULT_PROFILE_NAME));
}

#[test]
fn a_config_file_with_specialized_agents_still_gets_a_spawn_capable_default_agent() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "specialized",
        r#"
[agents.local-discovery]
engine = "ollama/local"
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "specialized").expect("the fixture loads");
    let default_agent = profile
        .agents
        .get(DEFAULT_PROFILE_NAME)
        .expect("the implicit default agent type");
    assert!(default_agent.can_spawn);
    assert_eq!(
        profile
            .agents
            .get("local-discovery")
            .and_then(|agent| agent.engine.as_deref()),
        Some("ollama/local")
    );
}

#[test]
fn an_explicit_default_agent_config_is_not_replaced_by_implicit_defaults() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "custom-default",
        r#"
[agents.default]
can_spawn = false

[agents.local-discovery]
engine = "ollama/local"
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "custom-default").expect("the fixture loads");
    assert!(
        !profile
            .agents
            .get(DEFAULT_PROFILE_NAME)
            .expect("the configured default agent type")
            .can_spawn
    );
}

#[test]
fn profile_config_field_access() {
    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        compaction: CompactionConfig::default(),
        agents: HashMap::from([(
            "main".to_string(),
            AgentConfig {
                engine: Some("fast".to_string()),
                compaction: None,
                description: None,
                allowed_tools: Some(vec!["bash".to_string()]),
                timeout_secs: Some(120),
                max_tool_iterations: None,
                persona: Some("You are a helpful assistant.".to_string()),
                can_spawn: false,
                tools: HashMap::new(),
            },
        )]),
        ..ProfileConfig::default()
    };

    assert_eq!(profile.default_engine, "default");
    assert!(profile.compaction.engine.is_none());

    let agent_config = profile.agents.get("main").unwrap();
    assert_eq!(agent_config.engine.as_deref(), Some("fast"));
    assert_eq!(
        agent_config.allowed_tools.as_deref(),
        Some(["bash".to_string()].as_slice())
    );
    assert_eq!(agent_config.timeout_secs, Some(120));
    assert_eq!(
        agent_config.persona.as_deref(),
        Some("You are a helpful assistant.")
    );
    assert!(!agent_config.can_spawn);
}

#[test]
fn agent_config_defaults_to_no_engine_override() {
    let agent_config = AgentConfig {
        engine: None,
        compaction: None,
        description: None,
        allowed_tools: None,
        timeout_secs: None,
        max_tool_iterations: None,
        persona: None,
        can_spawn: true,
        tools: HashMap::new(),
    };

    assert!(agent_config.engine.is_none());
    assert!(agent_config.allowed_tools.is_none());
    assert!(agent_config.timeout_secs.is_none());
    assert!(agent_config.persona.is_none());
    assert!(agent_config.can_spawn);
}
