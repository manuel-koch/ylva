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

use std::sync::atomic::{AtomicUsize, Ordering};

struct FailingProvider;

impl Provider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async {
            let (sender, receiver) = mpsc::channel(1);
            let _ = sender
                .send(StreamEvent::Error(anyhow::anyhow!(
                    "provider returned HTTP 404: model 'invalid' not found"
                )))
                .await;
            Ok(receiver)
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[derive(Default)]
struct ChildCompactionProvider {
    requests: AtomicUsize,
}

impl Provider for ChildCompactionProvider {
    fn name(&self) -> &str {
        "child-compaction"
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let request_number = self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let (sender, receiver) = mpsc::channel(4);
            tokio::spawn(async move {
                if request_number == 0 {
                    let _ = sender
                        .send(StreamEvent::Text("work completed ".repeat(500)))
                        .await;
                } else if request_number >= 2 {
                    let _ = sender
                        .send(StreamEvent::Delta(crate::agent::DeltaChunk {
                            tool_call_index: 0,
                            tool_call_id: Some("finish-child".to_string()),
                            function_name: Some("agent_finish".to_string()),
                            arguments_fragment: Some(
                                r#"{"status":"success","result":"finished"}"#.to_string(),
                            ),
                        }))
                        .await;
                } else {
                    let _ = sender
                        .send(StreamEvent::Text("work completed ".repeat(500)))
                        .await;
                }
                let _ = sender.send(StreamEvent::Done).await;
            });
            Ok(receiver)
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[derive(Default, Clone)]
struct ChildSummaryProvider {
    summary_requests: Arc<AtomicUsize>,
}

impl Provider for ChildSummaryProvider {
    fn with_runtime_config(
        &self,
        _runtime_config: crate::llm::engine::EngineRuntimeConfigHandle,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        Ok(Arc::new(self.clone()))
    }

    fn name(&self) -> &str {
        "child-summary"
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.summary_requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            let (sender, receiver) = mpsc::channel(2);
            tokio::spawn(async move {
                let _ = sender
                    .send(StreamEvent::Text("summarized the child work".to_string()))
                    .await;
                let _ = sender.send(StreamEvent::Done).await;
            });
            Ok(receiver)
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

use super::*;

#[test]
fn child_with_over_context_task_compacts_and_finishes() {
    crate::test_support::run_locked_paused(async {
        let mut profile = two_engine_profile();
        profile.compaction.engine = Some("main".to_string());
        profile.compaction.timeout_sec = 30;
        profile
            .agents
            .get_mut("quick")
            .expect("quick agent")
            .allowed_tools = Some(vec!["agent_finish".to_string()]);

        let provider = Arc::new(ChildCompactionProvider::default());
        let compact_provider = Arc::new(ChildSummaryProvider::default());
        let mut child_engine_config = test_engine_config();
        child_engine_config.model = "child-model".to_string();
        child_engine_config.context_length = Some(3000);
        child_engine_config.compact_target_ratio = 0.2;
        child_engine_config.soft_compaction_threshold = 0.3;
        child_engine_config.hard_compaction_threshold = 0.9;
        let summary_engine_config = EngineConfig {
            model: "summary-model".to_string(),
            context_length: Some(3000),
            ..test_engine_config()
        };
        let factory = AgentFactory::new(HashMap::from([
            (
                "main".to_string(),
                Arc::new(EngineImpl::new(
                    compact_provider.clone(),
                    summary_engine_config,
                )) as Arc<dyn Engine>,
            ),
            (
                "fast".to_string(),
                Arc::new(EngineImpl::new(provider.clone(), child_engine_config)) as Arc<dyn Engine>,
            ),
        ]));
        let (finish_tx, mut finish_rx) = mpsc::unbounded_channel();
        let (child_finish_tx, child_finish_rx) = mpsc::channel(1);
        let (forward_ui_tx, mut forwarded_updates) = mpsc::unbounded_channel();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let child_role = Role::Child(ChildRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: None,
            parent_cancel: CancellationToken::new(),
            subagent_ui_tx: forward_ui_tx,
            finish_tx: child_finish_tx,
        });

        let (child_loop, child_ui_rx) = factory
            .create(&profile, "quick", child_role)
            .expect("child factory builds");
        let transcript_target = ChildTranscriptTarget {
            parent_store: None,
            engine_config: Some(child_loop.engine().config()),
            utility_engine: None,
        };
        let task = format!(
            "Inspect this material and finish: {}",
            "detail ".repeat(700)
        );
        let drive_task = tokio::spawn(drive_child_to_completion(
            child_loop,
            child_ui_rx,
            child_finish_rx,
            task,
            run_id,
            call_id,
            CancellationToken::new(),
            finish_tx,
            mpsc::unbounded_channel::<UiUpdate>().0,
            transcript_target,
        ));

        let finish = tokio::time::timeout(Duration::from_secs(5), finish_rx.recv())
            .await
            .expect("child finishes after compaction")
            .expect("finish event is sent");
        assert_eq!(
            finish.status,
            "success",
            "finish={finish:?}, summary_requests={}, child_requests={}",
            compact_provider.summary_requests.load(Ordering::SeqCst),
            provider.requests.load(Ordering::SeqCst)
        );
        assert!(
            compact_provider.summary_requests.load(Ordering::SeqCst) >= 2,
            "child can compact more than once while continuing its task"
        );
        assert!(
            provider.requests.load(Ordering::SeqCst) >= 3,
            "child continues after repeated compaction and sends a finishing request"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), async {
                while let Some(update) = forwarded_updates.recv().await {
                    if matches!(update, UiUpdate::Notice { text, run_id: Some(id) }
                        if id == run_id && text.contains("compaction"))
                    {
                        return;
                    }
                }
            })
            .await
            .is_ok(),
            "child compaction notices are forwarded with the child run id"
        );
        drive_task.await.expect("child driver task joins");
    });
}

#[derive(Default)]
struct FinishOnlyChildProvider;

impl Provider for FinishOnlyChildProvider {
    fn name(&self) -> &str {
        "child-finish-only"
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async {
            let (sender, receiver) = mpsc::channel(4);
            tokio::spawn(async move {
                let _ = sender
                    .send(StreamEvent::Delta(crate::agent::DeltaChunk {
                        tool_call_index: 0,
                        tool_call_id: Some("finish-child".to_string()),
                        function_name: Some("agent_finish".to_string()),
                        arguments_fragment: Some(
                            r#"{"status":"success","result":"finished"}"#.to_string(),
                        ),
                    }))
                    .await;
                let _ = sender.send(StreamEvent::Done).await;
            });
            Ok(receiver)
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[test]
fn terminal_finish_is_received_only_after_the_child_agent_finish_tool_done_is_forwarded() {
    crate::test_support::run_locked_paused(async {
        let mut profile = two_engine_profile();
        profile
            .agents
            .get_mut("quick")
            .expect("quick agent")
            .allowed_tools = Some(vec!["agent_finish".to_string()]);

        let provider = Arc::new(FinishOnlyChildProvider);
        let factory = AgentFactory::new(HashMap::from([(
            "fast".to_string(),
            Arc::new(EngineImpl::new(provider, test_engine_config())) as Arc<dyn Engine>,
        )]));
        let (finish_tx, mut finish_rx) = mpsc::unbounded_channel();
        let (child_finish_tx, child_finish_rx) = mpsc::channel(1);
        let (forward_ui_tx, mut forwarded_updates) = mpsc::unbounded_channel();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let child_role = Role::Child(ChildRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: None,
            parent_cancel: CancellationToken::new(),
            subagent_ui_tx: mpsc::unbounded_channel().0,
            finish_tx: child_finish_tx,
        });

        let (child_loop, child_ui_rx) = factory
            .create(&profile, "quick", child_role)
            .expect("child factory builds");
        let transcript_target = ChildTranscriptTarget {
            parent_store: None,
            engine_config: Some(child_loop.engine().config()),
            utility_engine: None,
        };
        let drive_task = tokio::spawn(drive_child_to_completion(
            child_loop,
            child_ui_rx,
            child_finish_rx,
            "finish now".to_string(),
            run_id,
            call_id,
            CancellationToken::new(),
            finish_tx,
            forward_ui_tx,
            transcript_target,
        ));

        let finish = tokio::time::timeout(Duration::from_secs(5), finish_rx.recv())
            .await
            .expect("child finishes")
            .expect("finish event is sent");
        assert_eq!(finish.status, "success");

        let agent_finish_tool_done_forwarded =
            std::iter::from_fn(|| forwarded_updates.try_recv().ok()).any(|update| {
                matches!(update, UiUpdate::ToolDone { ref name, .. }
                    if name == crate::agent::tools::agent_finish::AgentFinishTool::NAME)
            });
        assert!(
            agent_finish_tool_done_forwarded,
            "the terminal finish must not overtake the child's agent_finish ToolDone"
        );
        drive_task.await.expect("child driver task joins");
    });
}

#[test]
fn child_context_called_finish_detects_finish_tool_result() {
    let mut context = Context::new();
    context.push_tool_result(crate::agent::ToolResult {
        id: Uuid::new_v4(),
        name: crate::agent::tools::agent_finish::AgentFinishTool::NAME.to_string(),
        result: "{}".to_string(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: std::time::Duration::ZERO,
    });
    assert!(child_context_called_finish(&context));
}

#[test]
fn child_context_called_finish_is_false_without_finish_tool_result() {
    let mut context = Context::new();
    context.push_tool_result(crate::agent::ToolResult {
        id: Uuid::new_v4(),
        name: "read".to_string(),
        result: "ok".to_string(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: std::time::Duration::ZERO,
    });
    assert!(!child_context_called_finish(&context));
}

#[test]
fn extract_last_assistant_text_returns_most_recent_non_empty_text() {
    let mut context = Context::new();
    context.push_assistant("first".to_string(), AssistantStatus::Complete);
    context.push_assistant(String::new(), AssistantStatus::Complete);
    context.push_assistant("last".to_string(), AssistantStatus::Complete);
    assert_eq!(extract_last_assistant_text(&context), "last");
}

#[test]
fn extract_last_assistant_text_defaults_to_empty_string_when_none_present() {
    let context = Context::new();
    assert_eq!(extract_last_assistant_text(&context), "");
}

#[test]
fn subagent_logs_and_transcripts_use_the_replacement_parent_session() {
    for format in [
        crate::logging::LogFormat::Text,
        crate::logging::LogFormat::Json,
    ] {
        let identities = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_identities = identities.clone();
        let logs = crate::test_support::capture_logs_with_format(format, async move {
            let directory = tempfile::tempdir().unwrap();
            // Other Store tests can first-register the session callsites without a subscriber.
            let priming_store = Store::create(
                &directory.path().join("prime"),
                directory.path(),
                Uuid::new_v4(),
                "mock",
                "mock",
                None,
            )
            .unwrap();
            priming_store
                .set_title("Prime callsite registration".into())
                .await
                .unwrap();
            tracing::callsite::rebuild_interest_cache();
            let root = Store::create(
                directory.path(),
                directory.path(),
                Uuid::new_v4(),
                "mock",
                "mock",
                None,
            )
            .unwrap();
            let replacement = root.start_new_session("mock", "mock", None, None).unwrap();
            let current_store = Arc::new(std::sync::Mutex::new(Some(root.clone())));
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(
                    vec!["PRIVATE CHILD ANSWER".into()],
                    Duration::ZERO,
                )),
                test_engine_config(),
            ));
            let mut manager = AgentFactory::build_child_agent_manager(
                SkillRuntime::default(),
                HashMap::from([
                    ("main".into(), engine.clone()),
                    ("fast".into(), engine.clone()),
                ]),
                vec![],
                HashSet::new(),
                two_engine_profile(),
                current_store.clone(),
                engine,
                "main".into(),
                None,
            );
            *current_store.lock().unwrap() = Some(replacement.clone());
            manager.manager.set_store(Some(replacement.clone()));
            let run_id = replacement.span().in_scope(|| {
                manager
                    .manager
                    .spawn(
                        "PRIVATE CHILD TASK".into(),
                        None,
                        "quick".into(),
                        "label".into(),
                        Uuid::new_v4(),
                    )
                    .run_id()
            });
            let finish = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        finish = manager.finish_rx.recv() => break finish.unwrap(),
                        _ = manager.subagent_ui_rx.recv() => {}
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(finish.run_id, run_id);
            replacement
                .span()
                .in_scope(|| manager.manager.record_child_result(&finish));
            replacement.flush().await;
            let child_path =
                crate::store::session_path(directory.path(), replacement.project_key(), run_id);
            assert_eq!(
                Store::read_session_meta(&child_path)
                    .unwrap()
                    .unwrap()
                    .parent_session,
                Some(replacement.session_id())
            );
            assert!(Store::read_child_runs(root.path()).unwrap().is_empty());
            captured_identities
                .lock()
                .unwrap()
                .extend([replacement.session_id(), run_id]);
            drop(manager);
            drop(root);
            drop(replacement);
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        assert!(!logs.contains("PRIVATE CHILD TASK"));
        assert!(!logs.contains("PRIVATE CHILD ANSWER"));
        let identities = identities.lock().unwrap();
        let persisted_line = logs
            .lines()
            .find(|line| line.contains("persisted child agent transcript"))
            .unwrap_or_else(|| {
                panic!("missing child transcript event in {format:?} logs:\n{logs}")
            });
        assert!(
            persisted_line.contains(&identities[0].to_string()),
            "{persisted_line}"
        );
        assert!(
            persisted_line.contains(&identities[1].to_string()),
            "{persisted_line}"
        );
        if format == crate::logging::LogFormat::Json {
            let event: serde_json::Value = serde_json::from_str(persisted_line).unwrap();
            assert_eq!(event["spans"][0]["session_id"], identities[0].to_string());
            assert_eq!(event["spans"][1]["run_id"], identities[1].to_string());
            assert_eq!(
                event["spans"][1]["parent_session_id"],
                identities[0].to_string()
            );
        }
    }
}

#[test]
fn tool_only_child_outcomes_receive_generated_session_titles() {
    crate::test_support::run_locked(async {
        use crate::agent::tools::agent_finish::AgentFinishTool;
        use crate::agent::tools::Tool;

        for arguments in [
            serde_json::json!({"status": "success", "result": "The module exposes three commands."}),
            serde_json::json!({"status": "failure", "error": "The module could not be read."}),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let chats_root = directory.path().join("chats");
            let parent_store = Store::create(
                &chats_root,
                directory.path(),
                Uuid::new_v4(),
                "mock",
                "mock",
                None,
            )
            .unwrap();
            let run_id = Uuid::new_v4();
            let tool_call_id = Uuid::new_v4();
            let (finish_tx, mut finish_rx) = mpsc::channel(1);
            let finish_tool = AgentFinishTool::new(AgentFinishToolConfig {
                finish_tx,
                run_id,
                call_id: Uuid::new_v4(),
            });
            let acknowledgement = finish_tool
                .create_call(tool_call_id, arguments.clone())
                .run()
                .await
                .unwrap()
                .text();
            assert!(finish_rx.recv().await.is_some());
            let mut context = Context::new();
            context.push_user("Summarize the module.".into());
            context.push_assistant_tool_call(
                String::new(),
                AssistantStatus::Complete,
                vec![crate::agent::ToolCallRequest {
                    id: tool_call_id,
                    name: AgentFinishTool::NAME.into(),
                    arguments,
                    persisted_title_arguments: Vec::new(),
                }],
            );
            context.push_tool_result(crate::agent::ToolResult {
                id: tool_call_id,
                name: AgentFinishTool::NAME.into(),
                result: acknowledgement,
                attachments: Vec::new(),
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: Vec::new(),
                content_sections: Vec::new(),
                duration: Duration::ZERO,
            });
            let utility_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(
                    vec!["Structured Task Summary".into()],
                    Duration::ZERO,
                )),
                test_engine_config(),
            ));
            write_child_transcript(
                &ChildTranscriptTarget {
                    parent_store: Some(parent_store.clone()),
                    engine_config: Some(test_engine_config()),
                    utility_engine: Some(utility_engine),
                },
                run_id,
                &context,
            )
            .await;
            let child_path =
                crate::store::session_path(&chats_root, parent_store.project_key(), run_id);
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if Store::read_session_meta(&child_path)
                        .unwrap()
                        .unwrap()
                        .title
                        == "Structured Task Summary"
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("a completed tool-only child receives a utility-generated title");
            assert_eq!(
                Store::read_messages(&child_path).unwrap(),
                context.messages()
            );
        }
    });
}

#[test]
fn a_finished_child_transcript_is_written_to_its_own_session_file() {
    crate::test_support::run_locked(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let parent_store = crate::store::Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("parent store created");
        let mut child_context = Context::new();
        child_context.push_user("summarize the module".to_string());
        child_context.push_assistant("the child's answer".to_string(), AssistantStatus::Complete);
        let utility_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(
                vec!["Child Module Summary".to_string()],
                Duration::ZERO,
            )),
            test_engine_config(),
        ));

        write_child_transcript(
            &ChildTranscriptTarget {
                parent_store: Some(parent_store.clone()),
                engine_config: Some(test_engine_config()),
                utility_engine: Some(utility_engine),
            },
            Uuid::new_v4(),
            &child_context,
        )
        .await;

        let child_summary = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let sessions =
                    crate::store::Store::list_sessions(&chats_root, parent_store.project_key())
                        .expect("sessions listable");
                if let Some(summary) = sessions.into_iter().find(|summary| {
                    summary.session_id != parent_store.session_id()
                        && summary.title == "Child Module Summary"
                }) {
                    break summary;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the child title is generated by the utility engine");
        assert_eq!(
            child_summary.parent_session,
            Some(parent_store.session_id()),
            "the listed child summary names its parent session"
        );
        let transcript = std::fs::read_to_string(&child_summary.path).expect("transcript readable");
        assert!(
            transcript.contains("the child's answer"),
            "the child's messages are persisted: {transcript}"
        );
        assert!(
            transcript.contains(r#""provider_connection":"mock""#)
                && transcript.contains(r#""model":"test-model""#),
            "the child's own initial provider/model is recorded: {transcript}"
        );
    });
}

#[test]
fn a_finished_child_transcript_is_skipped_when_persistence_is_disabled() {
    crate::test_support::run_locked(async {
        let mut child_context = Context::new();
        child_context.push_assistant("unpersisted".to_string(), AssistantStatus::Complete);
        write_child_transcript(
            &ChildTranscriptTarget {
                parent_store: None,
                engine_config: Some(test_engine_config()),
                utility_engine: None,
            },
            Uuid::new_v4(),
            &child_context,
        )
        .await;
    });
}

#[test]
fn run_child_sends_unstructured_finish_result_when_finish_never_called() {
    crate::test_support::run_locked(async {
        // The child's mock script produces only plain assistant text (no
        // tool call), so the child loop goes idle without ever calling
        // `agent_finish`; `run_child` must synthesize an "unstructured"
        // `FinishResult` from the child's last assistant text.
        let provider = Arc::new(MockProvider::new(
            vec!["child's final answer".to_string()],
            Duration::from_millis(0),
        ));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let engines = HashMap::from([("default".to_string(), engine)]);
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compaction: crate::config::CompactionConfig::default(),
            agents: HashMap::from([(
                "default".to_string(),
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
                    tools: HashMap::new(),
                },
            )]),
            ..ProfileConfig::default()
        };

        let (finish_tx, mut finish_rx) = mpsc::unbounded_channel::<FinishResult>();
        let (forward_ui_tx, mut forward_ui_rx) = mpsc::unbounded_channel::<UiUpdate>();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let (child_finish_tx, child_finish_rx) = mpsc::channel::<FinishResult>(1);
        let request = SpawnRequest {
            run_id,
            call_id,
            task: "do the child task".to_string(),
            briefing: None,
            agent_type: "default".to_string(),
            label: "label".to_string(),
        };
        let cancel = CancellationToken::new();

        let child_role_config = ChildRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: request.briefing.clone(),
            parent_cancel: cancel.clone(),
            subagent_ui_tx: forward_ui_tx.clone(),
            finish_tx: child_finish_tx,
        };
        let (child_loop, child_ui_rx) = AgentFactory::build(
            &SkillRuntime::default(),
            &engines,
            &[],
            &HashSet::new(),
            &profile,
            &request.agent_type,
            Role::Child(child_role_config),
            None,
            None,
            None,
        )
        .expect("child agent should construct successfully");

        drive_child_to_completion(
            child_loop,
            child_ui_rx,
            child_finish_rx,
            request.task,
            run_id,
            call_id,
            cancel,
            finish_tx,
            forward_ui_tx,
            ChildTranscriptTarget {
                parent_store: None,
                engine_config: None,
                utility_engine: None,
            },
        )
        .await;

        // Drain (and ignore) any forwarded UI updates from the child's run.
        while forward_ui_rx.try_recv().is_ok() {}

        let finish_result = tokio::time::timeout(Duration::from_secs(1), finish_rx.recv())
            .await
            .expect("finish_rx should receive promptly")
            .expect("finish_tx should not be dropped without sending");
        assert_eq!(finish_result.run_id, run_id);
        assert_eq!(finish_result.call_id, call_id);
        assert_eq!(finish_result.status, "unstructured");
        assert_eq!(
            finish_result.result.as_deref(),
            Some("child's final answer")
        );
    });
}

#[test]
fn run_child_reports_stream_error_as_failure_instead_of_unstructured_result() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(FailingProvider);
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, test_engine_config()));
        let engines = HashMap::from([("default".to_string(), engine)]);
        let profile = ProfileConfig {
            default_engine: "default".to_string(),
            compaction: crate::config::CompactionConfig::default(),
            agents: HashMap::from([(
                "default".to_string(),
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
                    tools: HashMap::new(),
                },
            )]),
            ..ProfileConfig::default()
        };

        let (finish_tx, mut finish_rx) = mpsc::unbounded_channel::<FinishResult>();
        let (forward_ui_tx, mut forward_ui_rx) = mpsc::unbounded_channel::<UiUpdate>();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let cancel = CancellationToken::new();
        let (child_finish_tx, child_finish_rx) = mpsc::channel::<FinishResult>(1);
        let child_role_config = ChildRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: None,
            parent_cancel: cancel.clone(),
            subagent_ui_tx: forward_ui_tx.clone(),
            finish_tx: child_finish_tx,
        };
        let (child_loop, child_ui_rx) = AgentFactory::build(
            &SkillRuntime::default(),
            &engines,
            &[],
            &HashSet::new(),
            &profile,
            "default",
            Role::Child(child_role_config),
            None,
            None,
            None,
        )
        .expect("child agent should construct successfully");

        drive_child_to_completion(
            child_loop,
            child_ui_rx,
            child_finish_rx,
            "do the child task".to_string(),
            run_id,
            call_id,
            cancel,
            finish_tx,
            forward_ui_tx,
            ChildTranscriptTarget {
                parent_store: None,
                engine_config: None,
                utility_engine: None,
            },
        )
        .await;

        while forward_ui_rx.try_recv().is_ok() {}

        let finish_result = tokio::time::timeout(Duration::from_secs(1), finish_rx.recv())
            .await
            .expect("finish_rx should receive promptly")
            .expect("finish_tx should not be dropped without sending");
        assert_eq!(finish_result.status, "failure");
        assert_eq!(finish_result.origin, FinishResultOrigin::Runtime);
        assert_eq!(finish_result.result, None);
        assert_eq!(
            finish_result.error.as_deref(),
            Some("provider returned HTTP 404: model 'invalid' not found")
        );
    });
}

#[test]
fn interleaved_child_streams_keep_their_own_run_id() {
    let first_run_id = uuid::Uuid::new_v4();
    let second_run_id = uuid::Uuid::new_v4();
    let interleaved = [
        (first_run_id, UiUpdate::AppendText("alpha ".to_string())),
        (second_run_id, UiUpdate::AppendText("beta ".to_string())),
        (
            first_run_id,
            UiUpdate::AppendThinking("first-thought".to_string()),
        ),
        (second_run_id, UiUpdate::AppendText("gamma".to_string())),
    ];

    let attributed: Vec<UiUpdate> = interleaved
        .into_iter()
        .map(|(run_id, update)| attribute_child_update(run_id, update))
        .collect();

    assert!(matches!(
        &attributed[0],
        UiUpdate::ChildAgentToken { run_id, text, is_thinking: false }
            if *run_id == first_run_id && text == "alpha "
    ));
    assert!(matches!(
        &attributed[1],
        UiUpdate::ChildAgentToken { run_id, text, is_thinking: false }
            if *run_id == second_run_id && text == "beta "
    ));
    assert!(matches!(
        &attributed[2],
        UiUpdate::ChildAgentToken { run_id, text, is_thinking: true }
            if *run_id == first_run_id && text == "first-thought"
    ));
    assert!(matches!(
        &attributed[3],
        UiUpdate::ChildAgentToken { run_id, text, is_thinking: false }
            if *run_id == second_run_id && text == "gamma"
    ));
}

#[test]
fn non_text_child_updates_pass_through_unchanged() {
    let run_id = uuid::Uuid::new_v4();
    let passed = attribute_child_update(run_id, UiUpdate::StreamDone);
    assert!(matches!(passed, UiUpdate::StreamDone));
}
