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

mod common;

use base64::Engine as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ylva::{
    default_registry, AgentCommand, AgentLoop, ApprovalCheck, AssistantStatus, AutoApproveTool,
    DeltaChunk, EngineImpl, Message, MockEvent, MockProvider, Provider, Sandbox, StreamEvent, Tool,
    ToolAttachment, ToolCall, ToolCallResult, ToolError, ToolRegistry, ToolRegistryConfig,
    ToolSchema, UiUpdate, UserMessageKind, DEFAULT_MAX_TOOL_USE_ITERATIONS,
};

/// Build a `MockEvent::Delta` carrying a complete tool call for `index`.
fn call(name: &str, index: usize, args: Value) -> MockEvent {
    MockEvent::Delta(DeltaChunk {
        tool_call_index: index,
        tool_call_id: None,
        function_name: Some(name.to_string()),
        arguments_fragment: Some(args.to_string()),
    })
}

/// Collect updates, advancing fake time between receives so a paused mock can
/// push its stream (interval-sleep gated) out, until `predicate` matches.
async fn collect_until_advancing<F>(
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    interval: Duration,
    mut predicate: F,
) -> Vec<UiUpdate>
where
    F: FnMut(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        if let Ok(update) = ui_rx.try_recv() {
            collected.push(update.clone());
            if predicate(&update) {
                return collected;
            }
        } else {
            tokio::time::advance(interval).await;
            tokio::task::yield_now().await;
        }
        if collected.len() > 500 {
            panic!("collect_until_advancing budget exceeded");
        }
    }
}

/// Like [`collect_until_advancing`], but auto-responds to every `ToolPending`
/// with the CLI-only driver policy (approve read/glob, deny write).
async fn drive_until<F>(
    agent: &AgentLoop,
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    registry: &ToolRegistry,
    interval: Duration,
    predicate: F,
) -> Vec<UiUpdate>
where
    F: Fn(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        if let Ok(update) = ui_rx.try_recv() {
            if let UiUpdate::ToolPending {
                id,
                name,
                arguments,
                ..
            } = &update
            {
                let approval_check = registry
                    .get(name)
                    .map(|tool| tool.needs_approval(arguments))
                    .unwrap_or(ApprovalCheck::UserApprovalRequired);
                let command = if matches!(approval_check, ApprovalCheck::AutoApproved) {
                    AgentCommand::ApproveTool { id: *id }
                } else {
                    AgentCommand::DenyTool {
                        id: *id,
                        reason: "denied in test".into(),
                    }
                };
                agent.send_command(command).await;
            }
            collected.push(update.clone());
            if predicate(&update) {
                return collected;
            }
        } else {
            tokio::time::advance(interval).await;
            tokio::task::yield_now().await;
        }
        if collected.len() > 500 {
            panic!("drive_until budget exceeded");
        }
    }
}

/// Advance fake time a few steps, then assert nothing new arrived (used to
/// check the loop is paused / back to idle).
async fn assert_quiescent(ui_rx: &mut mpsc::Receiver<UiUpdate>, interval: Duration) {
    for _ in 0..5 {
        tokio::time::advance(interval).await;
        tokio::task::yield_now().await;
    }
    assert!(ui_rx.try_recv().is_err(), "expected no further updates");
}

fn scratch_file(dir: &TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("scratch write");
    path.display().to_string()
}

fn make_sandbox() -> std::sync::Arc<std::sync::RwLock<Sandbox>> {
    std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()))
}

struct SequencedMockProvider {
    phases: Vec<Vec<MockEvent>>,
    interval: Duration,
    calls: AtomicUsize,
    recorded: Mutex<Vec<Vec<Message>>>,
}

impl SequencedMockProvider {
    fn new(phases: Vec<Vec<MockEvent>>, interval: Duration) -> Self {
        Self {
            phases,
            interval,
            calls: AtomicUsize::new(0),
            recorded: Mutex::new(Vec::new()),
        }
    }
}

impl Provider for SequencedMockProvider {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let phase_index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.recorded
            .lock()
            .expect("recorded requests")
            .push(messages);
        let script = self
            .phases
            .get(phase_index)
            .or_else(|| self.phases.last())
            .cloned()
            .unwrap_or_default();
        let interval = self.interval;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Thinking(text) => StreamEvent::Thinking(text),
                        MockEvent::Delta(delta) => StreamEvent::Delta(delta),
                    };
                    if tx.send(stream_event).await.is_err() {
                        return;
                    }
                    if index == last_index {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            return;
                        }
                        _ = tokio::time::sleep(interval) => {}
                    }
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }

    fn name(&self) -> &str {
        "sequenced-mock"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ylva::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[derive(JsonSchema, Deserialize)]
struct SleepArgs {}

/// Minimal [`ToolCallResult`] wrapper for a plain string, used in test tools that
/// do not need to expose domain properties.
#[derive(Debug)]
struct SimpleStringOutput(String);

impl ToolCallResult for SimpleStringOutput {
    fn text(&self) -> String {
        self.0.clone()
    }
}

struct GatedOutputTool {
    execution_order: Arc<Mutex<Vec<usize>>>,
    release: Arc<tokio::sync::Semaphore>,
}

struct GatedOutputCall {
    id: uuid::Uuid,
    arguments: Value,
    execution_order: Arc<Mutex<Vec<usize>>>,
    release: Arc<tokio::sync::Semaphore>,
}

impl ToolCall for GatedOutputCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn arguments(&self) -> &Value {
        &self.arguments
    }
    fn timeout_secs(&self) -> u64 {
        30
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
    fn argument_order(&self, _key: &str) -> usize {
        usize::MAX
    }
    fn run(self: Box<Self>) -> BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        Box::pin(async move {
            let index = self.arguments["index"].as_u64().expect("test index") as usize;
            self.execution_order
                .lock()
                .expect("execution order")
                .push(index);
            self.release.acquire().await.expect("release gate").forget();
            let result: Box<dyn ToolCallResult> =
                Box::new(SimpleStringOutput(format!("page {index} ").repeat(3_000)));
            Ok(result)
        })
    }
}

impl Tool for GatedOutputTool {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch one test page, recording its exactly-once side effect."
    }
    fn input_schema(&self) -> RootSchema {
        schema_for!(SleepArgs)
    }
    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        ApprovalCheck::AutoApproved
    }
    fn timeout_secs(&self) -> u64 {
        30
    }
    fn non_default_redacted_arguments(&self, _arguments: &Value) -> Vec<(String, Value)> {
        Vec::new()
    }
    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(GatedOutputCall {
            id,
            arguments,
            execution_order: self.execution_order.clone(),
            release: self.release.clone(),
        })
    }
}

async fn assert_ten_page_batch_continues(summary_succeeds: bool) {
    let interval = Duration::from_millis(1);
    let provider = Arc::new(SequencedMockProvider::new(
        vec![
            vec![MockEvent::Text("older recorded findings ".repeat(300))],
            (0..10)
                .map(|index| call("web_fetch", index, serde_json::json!({"index": index})))
                .collect(),
            vec![MockEvent::Text("The task continues.".to_string())],
        ],
        interval,
    ));
    let summary_provider = Arc::new(SequencedMockProvider::new(
        vec![vec![MockEvent::Text(
            if summary_succeeds {
                "Earlier findings are retained."
            } else {
                ""
            }
            .to_string(),
        )]],
        interval,
    ));
    let mut config = common::test_engine_config();
    config.context_length = Some(6_000);
    config.soft_compaction_threshold = 0.4;
    config.compact_target_ratio = 0.25;
    let engine = Arc::new(EngineImpl::new(provider.clone(), config.clone()));
    let summary_engine = Arc::new(EngineImpl::new(summary_provider.clone(), config));
    let execution_order = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(GatedOutputTool {
        execution_order: execution_order.clone(),
        release: release.clone(),
    }));
    let schemas = registry.schemas();
    let (mut agent, mut updates) = AgentLoop::new(engine.clone(), Arc::new(registry));
    agent = agent.with_compaction_engine(summary_engine);
    let handle = agent.run();
    agent
        .send_command(AgentCommand::Prompt("Prepare prior context.".to_string()))
        .await;
    collect_until_advancing(&mut updates, interval, |update| {
        matches!(update, UiUpdate::StreamDone)
    })
    .await;
    let task = "Summarize these ten pages and keep the evidence.";
    agent
        .send_command(AgentCommand::Prompt(task.to_string()))
        .await;
    let mut starts = 0;
    let mut skipped = 0;
    let mut summary_started_before_last_call = false;
    let mut finished = false;
    for _ in 0..20_000 {
        if let Ok(update) = updates.try_recv() {
            match update {
                UiUpdate::ToolStart { .. } => {
                    starts += 1;
                    release.add_permits(1);
                }
                UiUpdate::ToolAutoDenied { reason, .. } => {
                    assert!(reason.contains("not executed"));
                    skipped += 1;
                }
                UiUpdate::Notice { text, .. } if text.contains("Started background") => {
                    summary_started_before_last_call |= starts < 6;
                }
                UiUpdate::Error { message, .. } => panic!("unexpected failure: {message}"),
                UiUpdate::AppendText(text) if text == "The task continues." => {
                    finished = true;
                    break;
                }
                _ => {}
            }
        } else {
            tokio::time::advance(interval).await;
            tokio::task::yield_now().await;
        }
    }
    assert!(finished, "batch continuation must complete");
    assert_eq!(starts, ylva::agent::MAX_EXECUTED_TOOL_CALLS_PER_BATCH);
    assert_eq!(skipped, 10 - starts);
    assert_eq!(
        *execution_order.lock().expect("execution order"),
        (0..starts).collect::<Vec<_>>()
    );
    assert!(
        summary_started_before_last_call,
        "soft pressure schedules an immutable prefix before the queue drains"
    );
    let request = provider
        .recorded
        .lock()
        .expect("requests")
        .last()
        .expect("continuation")
        .clone();
    assert!(request.iter().any(|message| matches!(message,
            Message::User { content, kind: UserMessageKind::Explicit } if content == task)));
    let mut open = std::collections::HashSet::new();
    for message in &request {
        match message {
            Message::Assistant { tool_calls, .. } => {
                open.extend(tool_calls.iter().map(|call| call.id))
            }
            Message::ToolResult { value } => assert!(open.remove(&value.id), "orphaned result"),
            Message::ToolError { id, .. } => assert!(open.remove(id), "orphaned error"),
            _ => {}
        }
    }
    assert!(open.is_empty(), "unanswered calls in continuation");
    let mut projected = ylva::Context::default();
    projected.hydrate(request);
    let ceiling = 6_000 - 600 - 300 - 512;
    assert!(projected.estimated_model_tokens(&schemas, engine.as_ref()) <= ceiling);
    drop(agent);
    let context = handle.await.expect("loop task").expect("loop result");
    let canonical_results: Vec<_> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(canonical_results.len(), starts);
    assert!(
        canonical_results
            .iter()
            .all(|value| value.result.len() >= 18_000),
        "source output must not be shortened to the model allowance"
    );
    assert!(
        context.approximate_token_count() > 6_000 * 9 / 10,
        "canonical batch reproduces the below-soft to above-hard incident"
    );
}

struct SleepToolCall {
    id: uuid::Uuid,
    arguments: serde_json::Value,
}

impl ToolCall for SleepToolCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "sleep"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
    fn timeout_secs(&self) -> u64 {
        600
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        vec![]
    }
    fn argument_order(&self, _key: &str) -> usize {
        usize::MAX
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            let output: Box<dyn ToolCallResult> = Box::new(SimpleStringOutput("done".to_string()));
            Ok(output)
        })
    }
}

/// A tool that never finishes, used to exercise kill-on-Stop.
struct SleepTool;

impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }
    fn description(&self) -> &str {
        "sleeps almost forever"
    }
    fn input_schema(&self) -> RootSchema {
        schema_for!(SleepArgs)
    }
    fn needs_approval(&self, _args: &serde_json::Value) -> ApprovalCheck {
        ApprovalCheck::AutoApproved
    }
    fn timeout_secs(&self) -> u64 {
        600
    }
    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }
    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(SleepToolCall { id, arguments })
    }
}

fn is_stream_started(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::StreamStarted)
}

fn is_tool_pending(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::ToolPending { .. })
}

fn is_tool_error(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::ToolError { .. })
}

#[path = "tools_agent_loop/cancellation.rs"]
mod cancellation;
#[path = "tools_agent_loop/queue_approvals.rs"]
mod queue_approvals;
#[path = "tools_agent_loop/subagents.rs"]
mod subagents;
