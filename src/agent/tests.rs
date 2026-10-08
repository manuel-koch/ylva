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

use futures::future::BoxFuture;

use std::path::PathBuf;

use crate::test_support::capture_logs;

use crate::{
    agent::tools::{clarify::ClarifyTool, write::WriteTool},
    llm::engine::{Engine, EngineConfig, EngineImpl},
    llm::{MockEvent, MockProvider, ModelInfo, Provider, ToolSchema},
    sandbox::{Sandbox, SandboxContext},
};

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
            Err(anyhow::anyhow!(
                "provider returned HTTP 404: model 'invalid' not found"
            ))
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

pub(in crate::agent) fn test_engine() -> Arc<dyn Engine> {
    Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
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
    ))
}

fn test_engine_with_context_length(context_length: usize) -> Arc<dyn Engine> {
    Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
        EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(context_length),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.4,
            soft_compaction_threshold: 0.5,
            hard_compaction_threshold: 0.8,
        },
    ))
}

struct ToolResultTrackingProvider {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    request_received: Arc<tokio::sync::Notify>,
    reproduce_unknown_tool_overlap: bool,
}

impl Provider for ToolResultTrackingProvider {
    fn name(&self) -> &str {
        "tool-result-tracking"
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let request_index = {
            let mut requests = self
                .requests
                .lock()
                .expect("request tracking lock is not poisoned");
            let request_index = requests.len();
            requests.push(messages);
            request_index
        };
        self.request_received.notify_one();
        let reproduce_unknown_tool_overlap = self.reproduce_unknown_tool_overlap;
        Box::pin(async move {
            let (stream_tx, stream_rx) = mpsc::channel(4);
            tokio::spawn(async move {
                if request_index == 0 {
                    if reproduce_unknown_tool_overlap {
                        for tool_call_index in [1, 2] {
                            let _ = stream_tx
                                .send(StreamEvent::Delta(DeltaChunk {
                                    tool_call_index,
                                    tool_call_id: Some(format!("unknown-grep-{tool_call_index}")),
                                    function_name: Some("grep".to_string()),
                                    arguments_fragment: Some("{}".to_string()),
                                }))
                                .await;
                        }
                    }
                    let _ = stream_tx
                        .send(StreamEvent::Delta(DeltaChunk {
                            tool_call_index: 0,
                            tool_call_id: Some("read-large-context".to_string()),
                            function_name: Some("read".to_string()),
                            arguments_fragment: Some(
                                if reproduce_unknown_tool_overlap {
                                    r#"{"path":"src/agent.rs","mode":"lines","limit":1}"#
                                } else {
                                    r#"{"path":"src/agent/compaction.rs","mode":"lines","limit":1200}"#
                                }
                                .to_string(),
                            ),
                        }))
                        .await;
                } else if request_index == 1 && reproduce_unknown_tool_overlap {
                    let _ = stream_tx
                        .send(StreamEvent::Delta(DeltaChunk {
                            tool_call_index: 0,
                            tool_call_id: Some("approved-bash".to_string()),
                            function_name: Some("bash".to_string()),
                            arguments_fragment: Some(
                                r#"{"command":"printf regression-approved"}"#.to_string(),
                            ),
                        }))
                        .await;
                } else {
                    let _ = stream_tx
                        .send(StreamEvent::Text("Read result incorporated.".to_string()))
                        .await;
                }
                let _ = stream_tx.send(StreamEvent::Done).await;
            });
            Ok(stream_rx)
        })
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        _session_id: Option<Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let estimator = crate::llm::TokenEstimator::Tiktoken;
        let estimated_input_tokens = messages
            .iter()
            .map(|message| estimate_message_tokens(estimator, message))
            .sum::<usize>()
            + tool_schema_token_count(&tools);
        let context_length = 10_000;
        Box::pin(async move {
            on_estimate(estimated_input_tokens, context_length).await;
            self.chat(messages, tools, cancel).await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct GatedReadTool {
    inner: tools::read::ReadTool,
    release: Arc<tokio::sync::Notify>,
}

impl tools::Tool for GatedReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Gated read regression fixture"
    }
    fn input_schema(&self) -> schemars::schema::RootSchema {
        tools::Tool::input_schema(&self.inner)
    }
    fn needs_approval(&self, _: &serde_json::Value) -> crate::sandbox::ApprovalCheck {
        crate::sandbox::ApprovalCheck::AutoApproved
    }
    fn timeout_secs(&self) -> u64 {
        10
    }
    fn non_default_redacted_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        tools::Tool::non_default_redacted_arguments(&self.inner, arguments)
    }
    fn create_call(&self, id: Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(GatedReadCall {
            inner: tools::Tool::create_call(&self.inner, id, arguments),
            release: Arc::clone(&self.release),
        })
    }
}

struct GatedReadCall {
    inner: Box<dyn ToolCall>,
    release: Arc<tokio::sync::Notify>,
}

impl ToolCall for GatedReadCall {
    fn id(&self) -> Uuid {
        self.inner.id()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn arguments(&self) -> &serde_json::Value {
        self.inner.arguments()
    }
    fn timeout_secs(&self) -> u64 {
        10
    }
    fn argument_order(&self, key: &str) -> usize {
        self.inner.argument_order(key)
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        self.inner.non_default_redacted_arguments()
    }
    fn run(
        self: Box<Self>,
    ) -> BoxFuture<'static, Result<Box<dyn tools::ToolCallResult>, tools::ToolError>> {
        Box::pin(async move {
            self.release.notified().await;
            self.inner.run().await
        })
    }
}

/// Engine double that reports one pending notice, for asserting the
/// agent loop forwards engine notices ahead of the stream.
struct NoticeEngine {
    inner: EngineImpl,
    pending: std::sync::Mutex<Vec<String>>,
}

impl Engine for NoticeEngine {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn config(&self) -> EngineConfig {
        self.inner.config()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.inner.list_models()
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.inner.switch_model(model)
    }

    fn take_pending_notices(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .expect("the engine notice lock is poisoned"),
        )
    }
}

fn test_sandbox() -> std::sync::Arc<std::sync::RwLock<Sandbox>> {
    std::sync::Arc::new(std::sync::RwLock::new(Sandbox::with_root(PathBuf::from(
        env!("CARGO_MANIFEST_DIR"),
    ))))
}

#[test]
fn child_finish_notice_is_appended_as_implicit_user_message_not_a_second_tool_result() {
    // Regression test: `spawn_subagent`'s two-stage protocol (an
    // immediate "running"/"queued" `ToolResult`, later finalized once
    // the child actually finishes) must never leave two `role:"tool"`
    // messages tied to the same `tool_call_id` in the wire history —
    // that violates the one-call/one-response tool-calling protocol and
    // visibly confuses models. The fix is to never touch the original
    // `ToolResult` again; the child's real outcome is appended as a
    // brand-new implicit `user` message instead (see
    // `push_async_harness_notice` — not `role:"system"`, which some
    // providers/models reject mid-conversation), even with an assistant
    // message (the root's "running" reply) already in between.
    let mut context = Context::new();
    let id = Uuid::new_v4();
    context.push_tool_result(ToolResult {
        id,
        name: "spawn_subagent".into(),
        result: "{\"status\":\"spawned\"}".into(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: std::time::Duration::ZERO,
    });
    context.push_assistant(
        "I have spawned the child, standing by.".into(),
        AssistantStatus::Complete,
    );
    context.push_async_harness_notice(format!(
        "The subagent spawned via `spawn_subagent` (tool_call_id={id}) has finished. \
             Its final result:\n{{\"status\":\"success\",\"result\":\"done\"}}"
    ));

    let tool_result_count = context
        .messages()
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::ToolResult {
                    value: ToolResult {
                        id: message_id, ..
                    },
                } if *message_id == id
            )
        })
        .count();
    assert_eq!(
        tool_result_count,
        1,
        "the original provisional ToolResult must remain untouched, got: {:?}",
        context.messages()
    );
    assert_eq!(
        context.messages()[2],
        Message::ToolResult {
            value: ToolResult {
                id,
                name: "spawn_subagent".into(),
                result: "{\"status\":\"spawned\"}".into(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: std::time::Duration::ZERO,
            },
        },
        "the provisional result must never be rewritten"
    );
    assert!(
        matches!(
            context.messages().last(),
            Some(Message::User { content, kind: UserMessageKind::ImplicitToolNote })
                if content.contains(HARNESS_NOTICE_MARKER) && content.contains("has finished")
        ),
        "the child's real outcome must be appended as a trailing implicit user message \
             carrying the harness-notice marker, got: {:?}",
        context.messages()
    );
}

/// Compact-engine stand-in that records every request it receives and
/// either answers with a canned summary or fails the stream.
struct ScriptedSummaryProvider {
    recorded: Arc<Mutex<Vec<Vec<Message>>>>,
    fail: bool,
}

impl crate::llm::Provider for ScriptedSummaryProvider {
    fn name(&self) -> &str {
        "scripted-summary"
    }

    fn list_models(
        &self,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        _tools: Vec<crate::llm::ToolSchema>,
        _cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.recorded
            .lock()
            .expect("summary recording mutex is not poisoned")
            .push(messages);
        let fail = self.fail;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(async move {
                if fail {
                    let _ = tx
                        .send(StreamEvent::Error(anyhow::anyhow!(
                            "compact engine unavailable"
                        )))
                        .await;
                } else {
                    let _ = tx
                        .send(StreamEvent::Text(
                            "Decided to keep the transcript in the store.".to_string(),
                        ))
                        .await;
                    let _ = tx.send(StreamEvent::Done).await;
                }
            });
            Ok(rx)
        })
    }
}

/// Drain `ui_rx` until `predicate` matches, then return everything read.
///
/// Draining is mandatory (see `docs/known-test-issues.md`): the loop parks
/// on `send().await` once the bounded update channel fills, which would
/// stall both the turn and the compaction it is meant to trigger.
async fn collect_updates_until<F>(
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    mut predicate: F,
) -> Vec<UiUpdate>
where
    F: FnMut(&UiUpdate) -> bool,
{
    let mut collected: Vec<UiUpdate> = Vec::new();
    for _ in 0..400 {
        while let Ok(update) = ui_rx.try_recv() {
            let matched = predicate(&update);
            collected.push(update);
            if matched {
                return collected;
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not reached; collected {collected:?}");
}

/// Let already-spawned background tasks (the summary request and the
/// loop's handling of its outcome) run to completion under paused time.
async fn settle_background_tasks() {
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(in crate::agent) fn compaction_test_tools() -> Vec<ToolSchema> {
    tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default()).schemas()
}

/// Build the first turn of a conversation and pick a context window in
/// which it sits inside the soft-threshold band (`≈80%`), with the
/// rebuilt system prompts alone well under the compaction target so the
/// drop fallback has somewhere to go.
fn first_turn_window(prompt: &str, target_usage: f64) -> usize {
    let mut probe = Context::new();
    probe.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
    let tools = compaction_test_tools();
    let engine = test_engine();
    let system_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
    probe.push_user(prompt.to_string());
    let total_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
    assert!(
        total_tokens * 10 >= system_tokens * 19,
        "the prompt must carry enough tokens that the system prompts \
             alone fit the compaction target: system={system_tokens} \
             total={total_tokens}"
    );
    let context_length = (total_tokens as f64 / target_usage) as usize;
    assert!(
        (system_tokens as f64) <= 0.5 * context_length as f64,
        "the system prompts alone must fit the compaction target"
    );
    context_length
}

/// Grow a prompt until it carries enough tokens for
/// [`first_turn_window`]'s budget assertions.
fn sized_prompt() -> String {
    let tools = compaction_test_tools();
    let engine = test_engine();
    let mut prompt = "note ".repeat(64);
    loop {
        let mut probe = Context::new();
        probe.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
        let system_tokens = probe.estimated_model_tokens(&tools, engine.as_ref());
        probe.push_user(prompt.clone());
        if probe.estimated_model_tokens(&tools, engine.as_ref()) * 10 >= system_tokens * 19 {
            return prompt;
        }
        prompt = prompt.repeat(2);
        assert!(prompt.len() < 4_000_000, "prompt sizing must converge");
    }
}

/// Tool-call ids in `messages` whose result or error is not present —
/// the pairing invariant every model view must satisfy.
pub(in crate::agent) fn orphaned_tool_call_ids(messages: &[Message]) -> Vec<Uuid> {
    let mut open: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for message in messages {
        match message {
            Message::Assistant { tool_calls, .. } => {
                open.extend(tool_calls.iter().map(|tool_call| tool_call.id));
            }
            Message::ToolResult { value } => {
                open.remove(&value.id);
            }
            Message::ToolError { id, .. } => {
                open.remove(id);
            }
            _ => {}
        }
    }
    open.into_iter().collect()
}

#[test]
fn root_reprompts_after_child_finish_arrives_mid_stream() {
    crate::test_support::run_locked(async {
        // Regression test: a `spawn_subagent` child's `FinishResult` landing
        // while the root is mid-`Streaming` (not `Idle`) must force a
        // re-prompt once the root's current tool-free stream completes,
        // instead of silently going `Idle` and stranding the child's result
        // in `context` unseen by the model. Without the
        // `pending_child_finish_reprompt` flag this reproduces the bug where
        // the root's last streamed text claims it's "waiting for subagents"
        // and the conversation ends right there.
        use crate::agent::tools::{default_registry, ToolRegistryConfig};
        use crate::child_agent_manager::{ChildAgentManager, FinishResult};

        // Two text tokens with a real gap between them so there is a window
        // to inject the `FinishResult` while `mode == Streaming`, i.e. before
        // this turn's `StreamDone`.
        let script = vec![
            MockEvent::Text("Pending: waiting for subagents".to_string()),
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

        let registry = Arc::new(default_registry(
            test_sandbox(),
            ToolRegistryConfig::default(),
        ));

        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(8);
        let manager = ChildAgentManager::new(
            subagent_ui_tx,
            Box::new(
                |_request, _cancel, _ui_tx| crate::child_agent_manager::SpawnOutcome {
                    command_sender: None,
                    handle: tokio::spawn(async {}),
                },
            ),
        );
        let (finish_tx, finish_rx) = mpsc::channel(1);
        let (parent_subagent_ui_tx, parent_subagent_ui_rx) = mpsc::channel(8);
        let _ = parent_subagent_ui_tx; // kept alive only to avoid closing the channel early

        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
        agent_loop = agent_loop.with_child_agent_manager(manager, finish_rx, parent_subagent_ui_rx);

        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("do the task".into()))
            .await;

        // Let the first stream start (mode -> Streaming) before injecting.
        tokio::time::sleep(Duration::from_millis(20)).await;
        finish_tx
            .send(FinishResult {
                run_id: Uuid::new_v4(),
                call_id: Uuid::new_v4(),
                origin: crate::child_agent_manager::FinishResultOrigin::Agent,
                status: "success".to_string(),
                result: Some("child done".to_string()),
                error: None,
                sources: vec![],
            })
            .await
            .expect("finish_rx must still be open");

        // Give the loop time to finish the first stream, observe the
        // deferred finish while not idle, and (if fixed) start a second,
        // tool-free re-prompt.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Dropping the loop closes `cmd_tx`, which is how `run()` returns the
        // final committed history (see `cli_driver.rs`'s identical pattern).
        drop(agent_loop);

        let context = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("run() should return promptly after being dropped")
            .expect("run() task should not panic")
            .expect("run() should not return an error");

        let assistant_turns = context
            .messages()
            .iter()
            .filter(|message| matches!(message, Message::Assistant { .. }))
            .count();
        assert!(
            assistant_turns >= 2,
            "root must be re-prompted after the child's result arrives mid-stream \
             instead of going idle after a single turn; got {assistant_turns} assistant turn(s)"
        );
        assert!(
            context.messages().iter().any(
                |message| matches!(message, Message::User { content, kind: UserMessageKind::ImplicitToolNote } if content.contains("Subagent completion event") && content.contains("\"event\":\"subagent_completed\""))
            ),
            "the child's finish result must be recorded in context as an implicit user message"
        );
        let completion_notice = context
            .model_messages()
            .into_iter()
            .find_map(|message| match message {
                Message::User {
                    content,
                    kind: UserMessageKind::ImplicitToolNote,
                } if content.contains("Subagent completion event") => Some(content),
                _ => None,
            })
            .expect("completion guidance reaches the root model");
        assert!(completion_notice.contains("self-contained answer"));
        assert!(completion_notice.contains("Preserve the user's requested format"));
        assert!(completion_notice
            .contains("Do not assume tool results or subagent responses are visible to the user"));
        assert!(!completion_notice.contains("bubbles"));
        assert!(!completion_notice.contains("do not repeat or quote it"));

        // Drain the ui updates; just ensure the channel didn't panic/drop.
        while ui_rx.try_recv().is_ok() {}
    });
}

struct ResubmittingBatchProvider {
    operation_count: usize,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    skipped_write_path: Option<String>,
}

struct RejectingRequestProvider {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    failures: usize,
    context_rejection: bool,
    emit_partial: bool,
}

impl Provider for RejectingRequestProvider {
    fn name(&self) -> &str {
        "rejecting-request"
    }
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let index = {
            let mut requests = self.requests.lock().expect("requests");
            let index = requests.len();
            requests.push(messages);
            index
        };
        let fails = index < self.failures;
        let emit_partial = self.emit_partial;
        let reason = if self.context_rejection {
            "context_length_exceeded"
        } else {
            "connection failed"
        };
        Box::pin(async move {
            if fails && !emit_partial {
                anyhow::bail!(reason);
            }
            let (sender, receiver) = mpsc::channel(4);
            tokio::spawn(async move {
                if fails {
                    sender
                        .send(StreamEvent::Text("partial".to_string()))
                        .await
                        .expect("partial");
                    sender
                        .send(StreamEvent::Error(anyhow::anyhow!(reason)))
                        .await
                        .expect("error");
                } else {
                    sender
                        .send(StreamEvent::Text("complete".to_string()))
                        .await
                        .expect("complete");
                    let _ = sender.send(StreamEvent::Done).await;
                }
            });
            Ok(receiver)
        })
    }
}

impl Provider for ResubmittingBatchProvider {
    fn name(&self) -> &str {
        "resubmitting-batch"
    }
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let mut operations = HashMap::new();
        let mut completed = std::collections::HashSet::new();
        for message in &messages {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    for call in tool_calls {
                        if let Some(operation) = call.arguments["operation"].as_u64() {
                            operations.insert(call.id, operation as usize);
                        }
                    }
                }
                Message::ToolResult { value } => {
                    if let Some(operation) = operations.get(&value.id) {
                        completed.insert(*operation);
                    }
                }
                _ => {}
            }
        }
        let request_index = {
            let mut requests = self.requests.lock().expect("batch requests");
            let index = requests.len();
            requests.push(messages);
            index
        };
        let skipped_write_path = self.skipped_write_path.clone();
        let remaining: Vec<usize> = (0..self.operation_count)
            .filter(|operation| !completed.contains(operation))
            .filter(|_| skipped_write_path.is_none() || request_index == 0)
            .collect();
        Box::pin(async move {
            let (sender, receiver) = mpsc::channel(16);
            tokio::spawn(async move {
                if remaining.is_empty() {
                    sender
                        .send(StreamEvent::Text("complete".to_string()))
                        .await
                        .expect("text");
                } else {
                    for (index, operation) in remaining.into_iter().enumerate() {
                        let (name, arguments) = if let Some(path) = &skipped_write_path {
                            match operation {
                                0 | 6 => ("unknown", serde_json::json!({ "operation": operation })),
                                1 => (
                                    "read",
                                    serde_json::json!({ "path": 12, "operation": operation }),
                                ),
                                2 => (
                                    "bash",
                                    serde_json::json!({ "command": "printf denied", "operation": operation }),
                                ),
                                7 => (
                                    "write",
                                    serde_json::json!({ "path": path, "content": "must not be written", "operation": operation }),
                                ),
                                _ => (
                                    "read",
                                    serde_json::json!({ "path": "Cargo.toml", "mode": "lines", "limit": 1, "operation": operation }),
                                ),
                            }
                        } else {
                            (
                                "read",
                                serde_json::json!({ "path": "Cargo.toml", "mode": "lines", "limit": 1, "operation": operation }),
                            )
                        };
                        sender
                            .send(StreamEvent::Delta(DeltaChunk {
                                tool_call_index: index,
                                tool_call_id: None,
                                function_name: Some(name.to_string()),
                                arguments_fragment: Some(arguments.to_string()),
                            }))
                            .await
                            .expect("call");
                    }
                }
                let _ = sender.send(StreamEvent::Done).await;
            });
            Ok(receiver)
        })
    }
}

mod compaction_updates;
mod request;
mod runtime;
mod subagents;
mod tool_execution;
mod tool_queue;
