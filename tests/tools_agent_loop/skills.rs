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

#[tokio::test]
async fn skills_execute_through_normal_tools_and_persist_without_reopening_sources() {
    let directory = tempfile::tempdir().unwrap();
    let source_directory = directory.path().join(".ylva/skills/example");
    std::fs::create_dir_all(&source_directory).unwrap();
    let source = source_directory.join("SKILL.md");
    std::fs::write(&source, "---\nname: example\ndescription: Helpful instructions\nallowed-tools: [bash, write]\n---\n# Exact body\nUse sidecars only through read.\n").unwrap();
    let snapshot =
        Arc::new(ylva::skills::SkillCatalog::discover(None, directory.path(), None, None).unwrap());
    let provider = Arc::new(SequencedMockProvider::new(
        vec![
            vec![call("load_skill", 0, serde_json::json!({"name":"example"}))],
            vec![call("load_skill", 0, serde_json::json!({"name":"example"}))],
            vec![call(
                "load_skill",
                0,
                serde_json::json!({"name":"forbidden"}),
            )],
            vec![MockEvent::Text("Done".into())],
        ],
        Duration::ZERO,
    ));
    let engine: Arc<dyn ylva::Engine> = Arc::new(EngineImpl::new(
        provider.clone(),
        common::test_engine_config(),
    ));
    let mut profile = ylva::ProfileConfig::default();
    profile.agents.get_mut("default").unwrap().allowed_tools =
        Some(vec!["read".into(), "load_skill".into()]);
    profile.agents.get_mut("default").unwrap().can_spawn = false;
    let store = ylva::Store::create(
        &directory.path().join("chats"),
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "model",
        None,
    )
    .unwrap();
    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([(
        "default".into(),
        engine,
    )]))
    .with_skill_catalog(snapshot, false)
    .with_system_prompt("Global instructions")
    .with_store(store.clone());
    std::fs::remove_file(&source).unwrap();
    let (mut agent, mut updates) = factory
        .create(&profile, "default", ylva::Role::Root)
        .unwrap();
    let handle = agent.run();
    agent
        .send_command(AgentCommand::Prompt("Use example twice".into()))
        .await;
    let mut successful_loads = 0;
    let mut denied_loads = 0;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(update) = updates.recv().await {
            match update {
                UiUpdate::ToolDone { name, .. } if name == "load_skill" => successful_loads += 1,
                UiUpdate::ToolAutoDenied { name, .. } if name == "load_skill" => denied_loads += 1,
                UiUpdate::StreamDone => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    drop(agent);
    let context = handle.await.unwrap().unwrap();
    assert_eq!((successful_loads, denied_loads), (2, 1));
    let results: Vec<_> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { value } if value.name == "load_skill" => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].result, results[1].result);
    assert!(results[0]
        .result
        .starts_with("# Exact body\nUse sidecars only through read.\n"));
    assert_eq!(
        results[0].result_metadata.as_ref().unwrap()["name"],
        "example"
    );
    assert!(results[0].result_metadata_schema.is_some());
    assert!(!results[0].result.contains("allowed-tools"));
    for request in provider.recorded.lock().unwrap().iter() {
        assert!(!request.iter().any(|message| matches!(message, Message::System { content } if content.contains("# Exact body"))));
    }
    store.set_title("barrier".into()).await.unwrap();
    let replayed = ylva::Store::read_messages(store.path()).unwrap();
    let replayed_results: Vec<_> = replayed
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(replayed_results, results);
    let session_id = store.session_id();
    let chats = directory.path().join("chats");
    drop(results);
    drop(context);
    drop(factory);
    drop(store);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let resumed = ylva::Store::resume(&chats, directory.path(), session_id).unwrap();
    let mut resumed_context = ylva::Context::new();
    resumed_context.set_store(resumed.clone());
    assert!(resumed_context.messages().iter().any(|message| matches!(message, Message::ToolResult { value } if value.result.contains("# Exact body"))));
    std::fs::write(
        &source,
        "---\nname: example\ndescription: Updated\n---\nNew process instructions",
    )
    .unwrap();
    let new_snapshot =
        ylva::skills::SkillCatalog::discover(None, directory.path(), None, None).unwrap();
    assert_eq!(
        new_snapshot.skills["example"].body,
        "New process instructions"
    );
    let original_messages = resumed_context.messages().to_vec();
    let first_result_index = resumed_context
        .messages()
        .iter()
        .position(|message| matches!(message, Message::ToolResult { .. }))
        .unwrap();
    assert!(
        resumed_context.set_compaction(ylva::agent::compaction::CompactionSnapshot {
            // A requested cut before a result must advance past its matching outcome.
            boundary: first_result_index,
            summary: "Loaded example instructions earlier.".into(),
        })
    );
    let boundary = resumed_context.compaction().unwrap().boundary;
    assert!(boundary > first_result_index);
    assert!(
        ylva::agent::compaction::safe_boundaries_from(resumed_context.messages(), 0)
            .contains(&boundary)
    );
    assert_eq!(resumed_context.messages(), original_messages);
    let reloaded = ylva::agent::tools::load_skill::LoadSkillTool::new(
        Arc::new(
            ylva::skills::policy::CallableSkills::resolve(
                &new_snapshot,
                &profile,
                "default",
                &["load_skill".into()],
                false,
                Arc::new(std::sync::RwLock::new(ylva::Sandbox::with_root(
                    directory.path().to_owned(),
                ))),
            )
            .unwrap(),
        ),
        ylva::skills::policy::loader_context(
            Arc::new(std::sync::RwLock::new(ylva::Sandbox::with_root(
                directory.path().to_owned(),
            ))),
            &profile,
            "default",
        ),
    )
    .create_call(uuid::Uuid::new_v4(), serde_json::json!({"name":"example"}))
    .run()
    .await
    .unwrap();
    let reload_id = uuid::Uuid::new_v4();
    resumed_context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ylva::ToolCallRequest {
            id: reload_id,
            name: "load_skill".into(),
            arguments: serde_json::json!({"name":"example"}),
            persisted_title_arguments: vec![],
        }],
    );
    resumed_context.push_tool_result(ylva::ToolResult {
        id: reload_id,
        name: "load_skill".into(),
        result: reloaded.text(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: reloaded.metadata(),
        result_metadata_schema: reloaded
            .metadata_schema()
            .map(|schema| serde_json::to_value(schema).unwrap()),
        summary: vec![],
        content_sections: reloaded.content_sections(),
        duration: Duration::ZERO,
    });
    assert!(resumed_context.model_messages().iter().any(|message| matches!(message, Message::ToolResult { value } if value.id == reload_id && value.result.contains("New process instructions"))));
    resumed.set_title("clone barrier".into()).await.unwrap();
    drop(resumed_context);
    drop(resumed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cloned = ylva::Store::clone_session(&chats, directory.path(), session_id, None).unwrap();
    let mut clone_context = ylva::Context::new();
    clone_context.set_store(cloned);
    assert!(clone_context.messages().iter().any(|message| matches!(message, Message::ToolResult { value } if value.result.contains("# Exact body"))));
}
