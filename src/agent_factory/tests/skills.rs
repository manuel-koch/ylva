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

fn catalog(directory: &std::path::Path) -> Arc<crate::skills::SkillCatalog> {
    for name in ["alpha", "beta"] {
        let source = directory.join(".ylva/skills").join(name);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), format!("---\nname: {name}\ndescription: {name} description\nallowed-tools: [bash, write]\n---\n{name} PRIVATE BODY")).unwrap();
    }
    Arc::new(crate::skills::SkillCatalog::discover(None, directory, None, None).unwrap())
}

#[test]
fn skill_advertisement_preserves_the_library_default_identity() {
    let directory = tempfile::tempdir().unwrap();
    let snapshot = catalog(directory.path());
    let profile = ProfileConfig::default();
    let callable = crate::skills::policy::CallableSkills::resolve(
        &snapshot,
        &profile,
        "default",
        &["load_skill".into()],
        false,
        Arc::new(std::sync::RwLock::new(Sandbox::with_root(
            directory.path().to_owned(),
        ))),
    )
    .unwrap();
    let prompt = compose_agent_system_prompt(None, None, &callable).unwrap();
    assert!(prompt.starts_with(crate::DEFAULT_SOUL));
    assert!(prompt.contains("alpha description"));
    assert!(!prompt.contains("PRIVATE BODY"));
}

#[test]
fn factory_validates_unused_types_and_library_defaults_never_discover() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let (factory, mut profile) = make_factory();
        assert!(factory
            .create(&profile, "default", Role::Root)
            .unwrap()
            .0
            .registry()
            .get("load_skill")
            .is_none());
        let snapshot = catalog(directory.path());
        let factory = factory.with_skill_catalog(snapshot, false);
        let mut unused = profile.agents["default"].clone();
        unused.allowed_skills = Some(vec!["unknown".into()]);
        profile.agents.insert("unused".into(), unused);
        let error = factory
            .validate_profile_skills(&profile)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unused") && error.contains("unknown"));
        assert!(factory.create(&profile, "default", Role::Root).is_err());
    });
}

#[test]
fn root_child_and_storeless_factory_copies_keep_independent_catalogs() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let (factory, mut profile) = make_factory();
        profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec!["alpha".into()]);
        let mut specialist = profile.agents["default"].clone();
        specialist.allowed_skills = Some(vec!["beta".into()]);
        specialist.allowed_tools = Some(vec!["read".into(), "load_skill".into()]);
        specialist.can_spawn = false;
        profile.agents.insert("specialist".into(), specialist);
        let factory = factory
            .with_skill_catalog(catalog(directory.path()), true)
            .with_system_prompt("Global");
        let storeless = factory.without_store();
        let (mut root, _updates) = storeless.create(&profile, "default", Role::Root).unwrap();
        let loader = root.registry().get("load_skill").unwrap();
        assert_eq!(
            loader.needs_approval(&serde_json::json!({"name":"alpha"})),
            crate::ApprovalCheck::AutoApproved
        );
        assert!(matches!(
            loader.needs_approval(&serde_json::json!({"name":"beta"})),
            crate::ApprovalCheck::AutoDenied { .. }
        ));
        let handle = root.run();
        root.send_command(crate::AgentCommand::ClearContext).await;
        drop(root);
        let context = handle.await.unwrap().unwrap();
        let system = match &context.messages()[0] {
            Message::System { content } => content,
            _ => panic!("system prompt"),
        };
        assert!(system.contains("\"alpha\": \"alpha description\""));
        assert!(!system.contains("beta description"));
        assert!(!system.contains("PRIVATE BODY"));

        let (finish_tx, _finish_rx) = mpsc::channel(1);
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let (child, _updates) = storeless
            .create(
                &profile,
                "specialist",
                Role::Child(ChildRoleConfig {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    session_id: None,
                    briefing: None,
                    parent_cancel: CancellationToken::new(),
                    subagent_ui_tx,
                    finish_tx,
                }),
            )
            .unwrap();
        let loader = child.registry().get("load_skill").unwrap();
        assert_eq!(
            loader.needs_approval(&serde_json::json!({"name":"beta"})),
            crate::ApprovalCheck::AutoApproved
        );
        assert!(matches!(
            loader.needs_approval(&serde_json::json!({"name":"alpha"})),
            crate::ApprovalCheck::AutoDenied { .. }
        ));
        assert!(child.registry().get("bash").is_none());
        assert!(child.registry().get("write").is_none());

        let store = Store::create(
            &directory.path().join("chats"),
            directory.path(),
            Uuid::new_v4(),
            "mock",
            "model",
            None,
        )
        .unwrap();
        std::fs::remove_file(directory.path().join(".ylva/skills/beta/SKILL.md")).unwrap();
        let (switched, _updates) = storeless
            .create_root_for_store(&profile, "specialist", store)
            .unwrap();
        let loaded = switched
            .registry()
            .get("load_skill")
            .unwrap()
            .create_call(Uuid::new_v4(), serde_json::json!({"name":"beta"}))
            .run()
            .await
            .unwrap();
        assert!(loaded.text().contains("beta PRIVATE BODY"));
    });
}
