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
fn stream_failure_emits_one_user_visible_error() {
    crate::test_support::run_locked(async {
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(FailingProvider),
            EngineConfig {
                provider_connection: "failing".to_string(),
                model: "invalid".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("trigger failure".to_string()))
            .await;

        let mut error_messages = Vec::new();
        while let Ok(Some(update)) =
            tokio::time::timeout(Duration::from_millis(100), ui_rx.recv()).await
        {
            if let UiUpdate::Error { message, .. } = update {
                error_messages.push(message);
            }
        }

        assert_eq!(
            error_messages,
            vec!["provider returned HTTP 404: model 'invalid' not found".to_string()],
            "a stream failure must be rendered once even though the provider and \
                 agent-loop layers both observe it"
        );

        drop(agent);
        handle
            .await
            .expect("agent task should not panic")
            .expect("agent should stop cleanly after the channel closes");
    });
}

#[test]
fn clear_context_drops_stale_stream_event_from_a_superseded_generation() {
    crate::test_support::run_locked(async {
        // Regression test: `AgentCommand::ClearContext` must not let an
        // `AgentEvent` produced by the pre-clear stream leak into the
        // brand-new `Context` it installs. Cancellation is cooperative —
        // the old stream's producer task only notices `current_cancel`
        // was fired the next time it's polled, so it can still emit a
        // `StreamCancelled`/`StreamDone` after the clear has already run.
        // Without the `generation` guard, `StreamCancelled`'s existing
        // `current_cancel.is_none()` check (designed to reattach a
        // genuinely interrupted partial to the *same* conversation, as
        // `AgentCommand::Stop` does) mistakes this stale event for a
        // fresh interrupt and pushes the old partial into the new,
        // unrelated context.
        let script = vec![
            MockEvent::Text("Pending: partial from before clear".to_string()),
            MockEvent::Text(" ...".to_string()),
        ];
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::with_script(script, Duration::from_millis(80))),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));

        let (mut agent_loop, mut ui_rx) = AgentLoop::new(
            engine,
            Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            )),
        );
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("do the task".into()))
            .await;

        // Let the first stream start (mode -> Streaming, current_cancel ->
        // Some) before clearing.
        tokio::time::sleep(Duration::from_millis(20)).await;
        agent_loop.send_command(AgentCommand::ClearContext).await;

        // Give the old stream's producer task time to notice the
        // cancellation and emit its (now stale) `StreamCancelled`.
        tokio::time::sleep(Duration::from_millis(300)).await;

        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("run() should return promptly after being dropped")
            .expect("run() task should not panic")
            .expect("run() should not return an error");

        assert!(
                !context.messages().iter().any(|message| matches!(
                    message,
                    Message::Assistant { content, .. } if content.contains("Pending: partial from before clear")
                )),
                "a stale stream event from before ClearContext must not be \
                 committed into the fresh context; got {:?}",
                context.messages()
            );

        while ui_rx.try_recv().is_ok() {}
    });
}

#[test]
fn clear_context_records_the_root_agent_type_in_the_replacement_session() {
    crate::test_support::run_locked(async {
        // `/clear` starts a brand-new session file; its `meta` header must
        // carry the root agent type this loop runs under, so a cleared
        // conversation keeps advertising the type instead of degrading to
        // a header with no type at all.
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let chats_root = temporary_directory.path().join("chats");
        let original_session_id = Uuid::new_v4();
        let store = crate::store::Store::create(
            &chats_root,
            temporary_directory.path(),
            original_session_id,
            "mock",
            "test-model",
            None,
        )
        .expect("store created");

        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::with_script(
                vec![MockEvent::Text("ok".to_string())],
                Duration::from_millis(10),
            )),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let (agent_loop, mut ui_rx) = AgentLoop::new(
            engine,
            Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            )),
        );
        let mut agent_loop = agent_loop.with_agent_type("researcher").with_store(store);

        let handle = agent_loop.run();
        agent_loop.send_command(AgentCommand::ClearContext).await;

        let replacement = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let sessions = crate::store::Store::list_sessions(
                    &chats_root,
                    &crate::store::project_key(temporary_directory.path()),
                )
                .expect("sessions are listable");
                if let Some(session) = sessions
                    .into_iter()
                    .find(|session| session.session_id != original_session_id)
                {
                    break session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the replacement session appears on disk");

        let events = crate::store::Store::read_events(&replacement.path).expect("events readable");
        let crate::store::StoreEvent::Meta(meta) = &events[0] else {
            panic!("first line must be the meta header, got {:?}", events[0]);
        };
        assert_eq!(
            meta.agent_type.as_deref(),
            Some("researcher"),
            "the replacement session records the loop's root agent type"
        );
        assert_eq!(
            meta.parent_session, None,
            "a /clear rollover is not a subagent run"
        );

        drop(agent_loop);
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        while ui_rx.try_recv().is_ok() {}
    });
}

#[test]
fn agent_command_dispatch_logs_every_command_variant() {
    let log = capture_logs(async {
        tracing::callsite::rebuild_interest_cache();
        let registry = Arc::new(ToolRegistry::new());
        let (mut agent_loop, _ui_rx) = AgentLoop::new(test_engine(), registry);
        let handle = agent_loop.run();
        let tool_call_id = Uuid::nil();
        let subagent_run_id = Uuid::nil();

        agent_loop
            .send_command(AgentCommand::Prompt(String::new()))
            .await;
        agent_loop
            .send_command(AgentCommand::ApproveTool { id: tool_call_id })
            .await;
        agent_loop
            .send_command(AgentCommand::DenyTool {
                id: tool_call_id,
                reason: "private denial reason".to_string(),
            })
            .await;
        agent_loop
            .send_command(AgentCommand::AnswerClarify("private answer".to_string()))
            .await;
        agent_loop.send_command(AgentCommand::AbortClarify).await;
        agent_loop
            .send_command(AgentCommand::Stop {
                reason: "private stop reason".to_string(),
            })
            .await;
        agent_loop.send_command(AgentCommand::ClearContext).await;
        agent_loop
            .send_command(AgentCommand::RecordModelChanged {
                provider_connection: "private connection".to_string(),
                model: "private model".to_string(),
            })
            .await;
        agent_loop
            .send_command(AgentCommand::ForSubagent {
                run_id: subagent_run_id,
                command: Box::new(AgentCommand::AbortClarify),
            })
            .await;

        drop(agent_loop);
        handle
            .await
            .expect("agent command logging task should not panic")
            .expect("agent command logging loop should shut down cleanly");
    });

    for command_name in [
        "prompt",
        "approve_tool",
        "deny_tool",
        "answer_clarify",
        "abort_clarify",
        "stop",
        "clear_context",
        "record_model_changed",
        "for_subagent",
    ] {
        assert!(
            log.contains(&format!("command=\"{command_name}\"")),
            "missing receipt log for {command_name}: {log}"
        );
    }
    assert_eq!(
        log.matches("agent command received").count(),
        9,
        "each command should produce exactly one receipt event: {log}"
    );
    assert!(
        !log.contains("private"),
        "command receipt logs must not include private payloads: {log}"
    );
}

/// Engine double whose context window can change in place, like a `/models`
/// switch to a model with a different window.
struct ResizableContextEngine {
    inner: Arc<dyn Engine>,
    context_length: std::sync::Mutex<usize>,
}

impl Engine for ResizableContextEngine {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn config(&self) -> EngineConfig {
        let mut config = self.inner.config();
        config.context_length = Some(*self.context_length.lock().expect("lock"));
        config
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.inner.list_models()
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.inner.switch_model(model)
    }
}

#[test]
fn model_change_re_reports_context_usage_against_the_new_window() {
    crate::test_support::run_locked(async {
        let engine = Arc::new(ResizableContextEngine {
            inner: test_engine_with_context_length(32_000),
            context_length: std::sync::Mutex::new(32_000),
        });
        let registry = Arc::new(ToolRegistry::new());
        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine.clone(), registry);
        let handle = agent_loop.run();
        let baseline = ui_rx.recv().await;
        assert!(matches!(
            baseline,
            Some(UiUpdate::ContextUsage {
                context_length: 32_000,
                ..
            })
        ));

        *engine.context_length.lock().expect("lock") = 128_000;
        agent_loop
            .send_command(AgentCommand::RecordModelChanged {
                provider_connection: "mock".to_string(),
                model: "bigger".to_string(),
            })
            .await;

        let reported = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(UiUpdate::ContextUsage {
                    context_length,
                    source: ContextUsageSource::ModelView,
                    ..
                }) = ui_rx.recv().await
                {
                    break context_length;
                }
            }
        })
        .await
        .expect("model switch re-reports context usage");
        assert_eq!(reported, 128_000);

        drop(agent_loop);
        handle.await.expect("loop joins").expect("loop shuts down");
    });
}
