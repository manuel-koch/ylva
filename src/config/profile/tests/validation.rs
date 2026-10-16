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
fn compaction_thresholds_default_and_parse_per_engine() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n",
    );

    let profile = ProfileConfig::load(&profiles_root, "work").expect("the fixture loads");
    assert_eq!(
        profile.compaction.timeout_sec,
        DEFAULT_COMPACTION_TIMEOUT_SECS
    );
    let fast = profile.engines.get("fast").expect("the fast engine");
    assert_eq!(fast.compact_target_ratio, 0.5, "target defaults to half");
    assert_eq!(
        fast.soft_compaction_threshold,
        DEFAULT_SOFT_COMPACTION_THRESHOLD
    );
    assert_eq!(
        fast.hard_compaction_threshold,
        DEFAULT_HARD_COMPACTION_THRESHOLD
    );

    write_profile_config(
        &profiles_root,
        "work",
        "\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n\
             compact_target_ratio = 0.4\n\
             soft_compaction_threshold = 0.8\n\
             hard_compaction_threshold = 0.95\n",
    );
    let profile = ProfileConfig::load(&profiles_root, "work").expect("the fixture loads");
    let fast = profile.engines.get("fast").expect("the fast engine");
    assert_eq!(fast.compact_target_ratio, 0.4);
    assert_eq!(fast.soft_compaction_threshold, 0.8);
    assert_eq!(fast.hard_compaction_threshold, 0.95);
}

#[test]
fn compaction_thresholds_must_be_ordered_target_below_soft_below_hard() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);

    for (knobs, expected_fragment) in [
        (
            "compact_target_ratio = 1.5\n",
            "compact_target_ratio must be > 0 and <= 1",
        ),
        (
            "soft_compaction_threshold = 0.0\n",
            "soft_compaction_threshold must be > 0 and <= 1",
        ),
        (
            "hard_compaction_threshold = 1.5\n",
            "hard_compaction_threshold must be > 0 and <= 1",
        ),
        (
            "compact_target_ratio = 0.8\nsoft_compaction_threshold = 0.7\n",
            "must be below soft_compaction_threshold",
        ),
        (
            "soft_compaction_threshold = 0.9\nhard_compaction_threshold = 0.9\n",
            "must be below hard_compaction_threshold",
        ),
    ] {
        write_profile_config(
            &profiles_root,
            "work",
            &format!("\n[engines.fast]\nprovider = \"mock\"\nmodel = \"test-model\"\n{knobs}"),
        );

        let error = ProfileConfig::load(&profiles_root, "work")
            .expect_err("an unordered compaction knob must be rejected");
        assert!(
            matches!(error, ConfigError::InvalidCompactionSetting { .. }),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains(expected_fragment), "{message}");
        assert!(message.contains("fast"), "{message}");
    }
}

#[test]
fn compaction_timeout_must_be_positive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profiles_root = temp.path().join(PROFILES_DIRECTORY_NAME);
    write_profile_config(
        &profiles_root,
        "work",
        "default_engine = \"chat\"\n[compaction]\nengine = \"summarizer\"\ntimeout_sec = 0\n",
    );

    let error = ProfileConfig::load(&profiles_root, "work")
        .expect_err("zero timeout cannot define a useful summary deadline");
    assert!(error.to_string().contains("summarizer"), "{error}");
    assert!(error.to_string().contains("greater than 0"), "{error}");
}

#[test]
fn agent_compaction_settings_parse_and_override_independently() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "agent-compaction",
        r#"
default_engine = "chat"

[compaction]
engine = "profile-summary"
timeout_sec = 90

[agents.my-agent]
engine = "agent-chat"

[agents.my-agent.compaction]
engine = "agent-summary"
timeout_sec = "240"

[agents.timeout-only.compaction]
timeout_sec = 180
"#,
    );

    let profile = ProfileConfig::load(temp.path(), "agent-compaction")
        .expect("per-agent compaction settings load");
    let agent_compaction = profile.agents["my-agent"]
        .compaction
        .as_ref()
        .expect("agent compaction settings");
    assert_eq!(agent_compaction.engine.as_deref(), Some("agent-summary"));
    assert_eq!(agent_compaction.timeout_sec, Some(240));
    let timeout_only_compaction = profile.agents["timeout-only"]
        .compaction
        .as_ref()
        .expect("timeout-only override");
    assert_eq!(timeout_only_compaction.engine, None);
    assert_eq!(timeout_only_compaction.timeout_sec, Some(180));
    assert_eq!(
        profile.compaction.engine.as_deref(),
        Some("profile-summary")
    );
    assert_eq!(profile.compaction.timeout_sec, 90);
}

#[test]
fn zero_agent_compaction_timeout_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_profile_config(
        temp.path(),
        "invalid-agent-compaction",
        "default_engine = \"chat\"\n[agents.my-agent.compaction]\ntimeout_sec = 0\n",
    );

    let error = ProfileConfig::load(temp.path(), "invalid-agent-compaction")
        .expect_err("zero per-agent compaction timeout is invalid");
    assert!(error.to_string().contains("my-agent"), "{error}");
    assert!(error.to_string().contains("greater than 0"), "{error}");
}

#[test]
fn compaction_engine_falls_back_to_the_default_engine() {
    let profile = ProfileConfig {
        default_engine: "main".to_string(),
        compaction: CompactionConfig::default(),
        ..ProfileConfig::default()
    };
    assert_eq!(profile.compaction_engine_name(), "main");
}

#[test]
fn compaction_engine_selection_distinguishes_explicit_binding_from_fallback() {
    let mut profile = ProfileConfig {
        default_engine: "main".to_string(),
        compaction: CompactionConfig::default(),
        agents: HashMap::from([(
            "review".to_string(),
            AgentConfig {
                engine: Some("fast".to_string()),
                compaction: None,
                description: None,
                allowed_tools: None,
                allowed_skills: None,
                timeout_secs: None,
                max_tool_iterations: None,
                persona: None,
                can_spawn: false,
                tools: HashMap::new(),
            },
        )]),
        ..ProfileConfig::default()
    };

    assert_eq!(profile.explicit_compaction_engine_name_for("review"), None);
    assert_eq!(profile.compaction_engine_name_for("review"), "fast");
    profile.agents.get_mut("review").unwrap().compaction = Some(AgentCompactionConfig {
        engine: None,
        timeout_sec: Some(45),
    });
    assert_eq!(
        profile.explicit_compaction_engine_name_for("review"),
        None,
        "a timeout-only agent override must remain an implicit engine selection"
    );
    assert_eq!(profile.compaction_timeout_secs_for("review"), 45);

    profile.compaction.engine = Some("main".to_string());
    assert_eq!(
        profile.explicit_compaction_engine_name_for("review"),
        Some("main"),
        "an explicit binding to default_engine remains explicit"
    );
    profile.compaction.engine = Some("default".to_string());
    assert_eq!(
        profile.explicit_compaction_engine_name_for("review"),
        Some("default"),
        "an engine literally named default remains an explicit binding"
    );
    profile.agents.get_mut("review").unwrap().compaction = Some(AgentCompactionConfig {
        engine: Some("agent-summary".to_string()),
        timeout_sec: Some(120),
    });
    assert_eq!(
        profile.explicit_compaction_engine_name_for("review"),
        Some("agent-summary"),
        "the agent's explicit compaction engine takes precedence"
    );
}

#[test]
fn utility_engine_falls_back_to_default_and_honors_explicit_selection() {
    let mut profile = ProfileConfig {
        default_engine: "main".to_string(),
        compaction: CompactionConfig::default(),
        utility_engine: None,
        ..ProfileConfig::default()
    };
    assert_eq!(profile.utility_engine_name(), "main");
    profile.utility_engine = Some("cheap".to_string());
    assert_eq!(profile.utility_engine_name(), "cheap");
}
