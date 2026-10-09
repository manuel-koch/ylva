use super::*;

/// A profile exposing the given agent types on top of the in-code
/// default (`default` is always present).
fn profile_with_agent_types(names: &[&str]) -> ProfileConfig {
    let mut profile = ProfileConfig::default();
    for name in names {
        if *name == crate::DEFAULT_PROFILE_NAME {
            continue;
        }
        profile.agents.insert(
            name.to_string(),
            crate::AgentConfig {
                engine: None,
                compaction: None,
                description: None,
                allowed_tools: None,
                tools: HashMap::new(),
                timeout_secs: None,
                max_tool_iterations: None,
                persona: None,
                can_spawn: true,
            },
        );
    }
    profile
}

#[test]
fn a_fresh_session_without_flags_selects_the_default_agent_type() {
    let profile = profile_with_agent_types(&["researcher"]);

    let selection = select_root_agent_type(None, None, false, &profile).expect("default resolves");

    assert_eq!(selection.agent_type, "default");
    assert!(!selection.override_changes_active_type);
}

#[test]
fn an_explicit_agent_type_wins_over_the_stored_type_on_a_resume() {
    let profile = profile_with_agent_types(&["researcher"]);

    let selection = select_root_agent_type(Some("researcher"), Some("default"), true, &profile)
        .expect("the configured type resolves");

    assert_eq!(selection.agent_type, "researcher");
    assert!(
        selection.override_changes_active_type,
        "an explicit flag replacing the stored type must be recorded"
    );
}

#[test]
fn an_explicit_agent_type_matching_the_stored_type_is_not_an_override() {
    let profile = profile_with_agent_types(&["researcher"]);

    let selection = select_root_agent_type(Some("researcher"), Some("researcher"), true, &profile)
        .expect("the configured type resolves");

    assert_eq!(selection.agent_type, "researcher");
    assert!(!selection.override_changes_active_type);
}

#[test]
fn an_explicit_agent_type_on_a_fresh_session_is_never_an_override_record() {
    let profile = profile_with_agent_types(&["researcher"]);

    let selection = select_root_agent_type(Some("researcher"), None, false, &profile)
        .expect("the configured type resolves");

    assert_eq!(selection.agent_type, "researcher");
    assert!(
        !selection.override_changes_active_type,
        "a fresh session already carries its type in `meta`"
    );
}

#[test]
fn a_resume_without_an_explicit_flag_restores_the_stored_type_without_a_record() {
    let profile = profile_with_agent_types(&["researcher"]);

    let selection = select_root_agent_type(None, Some("researcher"), true, &profile)
        .expect("the stored type resolves");

    assert_eq!(selection.agent_type, "researcher");
    assert!(!selection.override_changes_active_type);
}

#[test]
fn an_unknown_explicit_agent_type_is_an_error_listing_the_available_types() {
    let profile = profile_with_agent_types(&["researcher"]);

    let error = select_root_agent_type(Some("nope"), None, false, &profile)
        .expect_err("an unconfigured type must not start");

    assert!(
        error.contains("unknown agent type 'nope'"),
        "the error names the flag's value: {error}"
    );
    assert!(
        error.contains("default") && error.contains("researcher"),
        "the error lists the configured types: {error}"
    );
}

#[test]
fn a_stale_stored_agent_type_is_an_error_pointing_at_the_flag() {
    let profile = profile_with_agent_types(&["researcher"]);

    let error = select_root_agent_type(None, Some("retired"), true, &profile)
        .expect_err("a stored type no longer in the profile must not start");

    assert!(
        error.contains("'retired'"),
        "the error names the stale stored type: {error}"
    );
    assert!(
        error.contains("--agent"),
        "the error tells the user how to resolve it: {error}"
    );
}
