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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::llm::{Provider, ToolEnvelope, ToolSchema};

pub mod tools;

use tools::{ToolError, ToolRegistry};

/// Maximum number of LLM requests the ReAct loop starts within a single user
/// turn before the guard stops re-prompting. Per-engine configurability is a
/// later (Phase 5) concern; this module-level default is the Phase 3 SSOT.
pub const MAX_ITERATIONS: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Idle,
    Streaming,
    ToolPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    Stop,
    ApproveTool { id: Uuid },
    DenyTool { id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    StreamDone {
        partial: String,
    },
    StreamCancelled {
        partial: String,
    },
    StreamError(String),
    ToolCall {
        calls: Vec<AssembledToolCall>,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result: String,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
    },
    ToolKilled {
        id: Uuid,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiUpdate {
    AppendText(String),
    StreamStarted,
    StreamDone,
    StreamCancelled,
    Error(String),
    ToolStart {
        name: String,
        id: Uuid,
        arguments: Value,
    },
    ToolPending {
        name: String,
        id: Uuid,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result: String,
    },
    ToolError {
        id: Uuid,
        error: String,
    },
    ToolKilled {
        id: Uuid,
    },
}

/// A raw tool-call fragment emitted by a provider, to be assembled by the
/// agent's inner streaming task, never by the provider (blueprint §4A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaChunk {
    pub tool_call_index: usize,
    pub tool_call_id: Option<String>,
    pub function_name: Option<String>,
    pub arguments_fragment: Option<String>,
}

#[derive(Debug)]
pub enum StreamEvent {
    Text(String),
    Delta(DeltaChunk),
    Done,
    Cancelled,
    Error(anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantStatus {
    Complete,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRequest {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
        status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResult {
        id: Uuid,
        name: String,
        result: String,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
    },
}

/// Single source of truth for the default system prompt in Phase 1. A later
/// config phase (Phase 7 §4G layering) may source this from a SOUL.md file
/// without changing call sites.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Ylva, a helpful agent with tool capabilities.";

/// System-prompt snippet, injected immediately after the persona/initial
/// system prompt, that documents the uniform tool-response envelope a model
/// must parse. The described JSON matches the schema derived from
/// [`crate::llm::ToolEnvelope`] — the very struct used to serialize every
/// `role:"tool"` message — so the instruction can never drift from the wire
/// shape (blueprint §4D: schema from typed data, never hand-written). It is a
/// `LazyLock` because the schema is generated at first use.
pub static TOOL_PROTOCOL_SYSTEM_PROMPT: LazyLock<String> = LazyLock::new(|| {
    let mut schema_json =
        serde_json::to_value(schemars::schema_for!(ToolEnvelope)).unwrap_or_default();
    // The struct's doc comment is the document: schemars mirrors it into the
    // schema's `description`, which we lift out as the leading phrase, then
    // drop so the embedded schema stays a pure shape (no comment text inside).
    let lead = schema_json
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(object) = schema_json.as_object_mut() {
        object.remove("description");
    }
    let shape = serde_json::to_string_pretty(&schema_json).unwrap_or_default();
    format!("{lead}\n{shape}")
});

#[derive(Debug, Default)]
pub struct Context {
    messages: Vec<Message>,
}

impl Context {
    pub fn new() -> Self {
        Context {
            messages: vec![
                Message::System {
                    content: DEFAULT_SYSTEM_PROMPT.to_string(),
                },
                Message::System {
                    content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
                },
            ],
        }
    }

    pub fn push_user(&mut self, text: String) {
        self.messages.push(Message::User { content: text });
    }

    pub fn push_assistant(&mut self, content: String, status: AssistantStatus) {
        self.messages.push(Message::Assistant {
            content,
            status,
            tool_calls: Vec::new(),
        });
    }

    pub fn push_assistant_tool_call(
        &mut self,
        content: String,
        status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    ) {
        self.messages.push(Message::Assistant {
            content,
            status,
            tool_calls,
        });
    }

    pub fn push_tool_result(&mut self, id: Uuid, name: String, result: String) {
        self.messages.push(Message::ToolResult { id, name, result });
    }

    pub fn push_tool_error(&mut self, id: Uuid, name: String, error: String) {
        self.messages.push(Message::ToolError { id, name, error });
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// A fully assembled, validated tool call, ready for the outer loop to queue
/// for serial (one-at-a-time) execution (blueprint §4A/§4B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledToolCall {
    pub id: Uuid,
    pub name: String,
    pub arguments: Value,
}

/// A queued tool call awaiting approval, with its tool-defined timeout.
///
/// Once approved, [`PendingTool::execute`] runs the tool in its own task and
/// reports completion through `agent_tx` so the loop never blocks on a tool.
#[derive(Debug)]
pub struct PendingTool {
    pub call: AssembledToolCall,
    pub timeout_secs: Option<u64>,
}

/// Default execution timeout for file-system tools (blueprint §4D: 5s).
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 5;

impl PendingTool {
    /// Spawn the tool in its own task, bounded by `timeout_secs` (or the
    /// default) and a per-tool `CancellationToken`. Completion is reported via
    /// `agent_tx` (`ToolDone`/`ToolError`/`ToolKilled`); returns the token the
    /// outer loop stores as the running tool's cancel.
    pub fn execute(
        self,
        registry: Arc<ToolRegistry>,
        agent_tx: mpsc::Sender<AgentEvent>,
    ) -> CancellationToken {
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let name = self.call.name.clone();
        let id = self.call.id;
        let timeout_secs = self.timeout_secs.unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS);
        tokio::spawn(async move {
            let outcome = tokio::select! {
                biased;
                _ = run_cancel.cancelled() => {
                    let _ = agent_tx
                        .send(AgentEvent::ToolKilled {
                            id,
                            name: name.clone(),
                        })
                        .await;
                    return;
                }
                result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
                    match registry.get(&name) {
                        Some(tool) => tool.run(self.call.arguments).await,
                        None => Err(ToolError::failed(
                            &name,
                            format!("unknown tool '{name}'"),
                        )),
                    }
                }) => result
            };
            match outcome {
                Ok(Ok(result)) => {
                    let _ = agent_tx
                        .send(AgentEvent::ToolDone { id, name, result })
                        .await;
                }
                Ok(Err(error)) => {
                    let _ = agent_tx
                        .send(AgentEvent::ToolError {
                            id,
                            name,
                            error: error.to_string(),
                        })
                        .await;
                }
                Err(_elapsed) => {
                    let message = format!("tool '{name}' timed out after {timeout_secs}s");
                    let _ = agent_tx
                        .send(AgentEvent::ToolError {
                            id,
                            name,
                            error: message,
                        })
                        .await;
                }
            }
        });
        cancel
    }
}

/// Non-blocking agent loop (blueprint §4B).
///
/// State machine: `Idle` receives a `Prompt` and starts a stream; further
/// `Prompt`s while busy are queued as **steers** and injected at the next
/// natural point — they never abort a live stream. A stream that assembles
/// tool calls transitions to `ToolPending`, drains the pending queue
/// serially (each call awaiting `ApproveTool`/`DenyTool`), and re-prompts the
/// model once the queue empties. Only `Stop` aborts, via the current request's
/// or current tool's `CancellationToken`.
///
/// Approval protocol is uniform across all tools: every call pauses at
/// `ToolPending` and waits for an approve/deny command — the loop holds no
/// approval policy, the driver decides (blueprint §4E, §4B).
///
/// The outer loop task is the sole writer of `Context` (`single-writer`
/// rule): no `Arc<Mutex<Context>>` anywhere. `run()` takes the loop's channel
/// receivers out of `self`; the caller keeps `self` (and thus `cmd_tx`) alive
/// and shuts the loop down by **dropping the `AgentLoop`**, after which the
/// task returns the final committed `Context` from `handle.await`.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    cmd_rx: Option<mpsc::Receiver<AgentCommand>>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
    agent_rx: Option<mpsc::Receiver<AgentEvent>>,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
    ) -> (Self, mpsc::Receiver<UiUpdate>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(10);
        let (ui_tx, ui_rx) = mpsc::channel::<UiUpdate>(100);
        let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>(10);
        let agent = AgentLoop {
            provider,
            registry,
            cmd_tx,
            cmd_rx: Some(cmd_rx),
            ui_tx,
            agent_tx,
            agent_rx: Some(agent_rx),
        };
        (agent, ui_rx)
    }

    pub async fn send_command(&self, cmd: AgentCommand) {
        let _ = self.cmd_tx.send(cmd).await;
    }

    pub fn run(&mut self) -> tokio::task::JoinHandle<anyhow::Result<Context>> {
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        let ui_tx = self.ui_tx.clone();
        let agent_tx = self.agent_tx.clone();
        let mut cmd_rx = self
            .cmd_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");
        let mut agent_rx = self
            .agent_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");

        tokio::spawn(async move {
            let mut context = Context::new();
            let mut mode = AgentMode::Idle;
            let mut current_cancel: Option<CancellationToken> = None;
            let mut current_tool_cancel: Option<CancellationToken> = None;
            let mut pending_steers: VecDeque<String> = VecDeque::new();
            let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();
            let mut iterations = 0usize;

            loop {
                tokio::select! {
                    biased;
                    Some(event) = agent_rx.recv() => {
                        match event {
                            AgentEvent::StreamDone { partial } => {
                                context.push_assistant(partial, AssistantStatus::Complete);
                                if inject_pending_steers(&mut pending_steers, &mut context) {
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        provider.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                    ));
                                } else {
                                    mode = AgentMode::Idle;
                                }
                            }
                            AgentEvent::StreamCancelled { partial } => {
                                // Only commit the partial when this was a genuine
                                // interrupt; ignore stale cancellation from a replaced
                                // stream (current_cancel already Some again).
                                if current_cancel.is_none() {
                                    context.push_assistant(partial, AssistantStatus::Interrupted);
                                    mode = AgentMode::Idle;
                                }
                            }
                            AgentEvent::StreamError(error) => {
                                pending_steers.clear();
                                mode = AgentMode::Idle;
                                let _ = ui_tx.send(UiUpdate::Error(error)).await;
                            }
                            AgentEvent::ToolCall { calls } => {
                                let requests = calls
                                    .iter()
                                    .map(|call| ToolCallRequest {
                                        id: call.id,
                                        name: call.name.clone(),
                                        arguments: call.arguments.clone(),
                                    })
                                    .collect();
                                context.push_assistant_tool_call(
                                    String::new(),
                                    AssistantStatus::Complete,
                                    requests,
                                );
                                for call in calls {
                                    let timeout_secs =
                                        registry.get(&call.name).map(|tool| tool.timeout_secs());
                                    pending_tools
                                        .push_back(PendingTool { call, timeout_secs });
                                }
                                mode = AgentMode::ToolPending;
                                if let Some(first) = pending_tools.front() {
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolPending {
                                            name: first.call.name.clone(),
                                            id: first.call.id,
                                        })
                                        .await;
                                }
                            }
                            AgentEvent::ToolDone { id, name, result } => {
                                current_tool_cancel = None;
                                context.push_tool_result(id, name.clone(), result.clone());
                                let _ = ui_tx
                                    .send(UiUpdate::ToolDone {
                                        id,
                                        name,
                                        result,
                                    })
                                    .await;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut iterations,
                                    provider.clone(),
                                    registry.schemas(),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                )
                                .await;
                            }
                            AgentEvent::ToolError { id, name, error } => {
                                current_tool_cancel = None;
                                context.push_tool_error(id, name, error.clone());
                                let _ = ui_tx.send(UiUpdate::ToolError { id, error }).await;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut iterations,
                                    provider.clone(),
                                    registry.schemas(),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                )
                                .await;
                            }
                            AgentEvent::ToolKilled { id, name: _ } => {
                                current_tool_cancel = None;
                                pending_tools.clear();
                                let _ = ui_tx.send(UiUpdate::ToolKilled { id }).await;
                                mode = AgentMode::Idle;
                            }
                        }
                    }
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else {
                            // AgentLoop dropped → cmd_tx closed → clean shutdown:
                            // return the final committed Context (read-only history).
                            return Ok(context);
                        };
                        match cmd {
                            AgentCommand::Prompt(text) => {
                                if text.is_empty() {
                                    continue;
                                }
                                if mode == AgentMode::Idle {
                                    iterations = 1;
                                    context.push_user(text);
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        provider.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                    ));
                                } else {
                                    // Cooperative steering: never cancel the live
                                    // stream. Queued from ToolPending too; injection
                                    // is deferred (Phase 4 adds descope_pending_tools).
                                    pending_steers.push_back(text);
                                }
                            }
                            AgentCommand::ApproveTool { id } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id == id);
                                if is_front {
                                    let pending =
                                        pending_tools.pop_front().expect("checked front");
                                    let name = pending.call.name.clone();
                                    let call_id = pending.call.id;
                                    let arguments = pending.call.arguments.clone();
                                    current_tool_cancel = Some(PendingTool::execute(
                                        pending,
                                        registry.clone(),
                                        agent_tx.clone(),
                                    ));
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolStart {
                                            name,
                                            id: call_id,
                                            arguments,
                                        })
                                        .await;
                                }
                            }
                            AgentCommand::DenyTool { id } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id == id);
                                if is_front {
                                    let denied =
                                        pending_tools.pop_front().expect("checked front");
                                    let name = denied.call.name;
                                    let call_id = denied.call.id;
                                    let error = format!(
                                        "tool '{name}' was denied (no auto-approve configured)"
                                    );
                                    context.push_tool_error(call_id, name, error.clone());
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolError { id: call_id, error })
                                        .await;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut iterations,
                                        provider.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::Stop => {
                                // Interrupt: hard-abort the current round.
                                pending_steers.clear();
                                if let Some(cancel_token) = current_cancel.take() {
                                    cancel_token.cancel();
                                }
                                if let Some(tool_cancel) = current_tool_cancel.take() {
                                    tool_cancel.cancel();
                                }
                                if mode == AgentMode::ToolPending {
                                    pending_tools.clear();
                                    mode = AgentMode::Idle;
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Advance the pending-tool queue after one call completes (executed, errored,
/// or denied). If a call remains, stay in `ToolPending` and surface the next
/// approval request; otherwise re-prompt the model — the internal deferred
/// re-prompt (blueprint §7 Phase 3 Q26) — unless the iteration guard trips.
#[allow(clippy::too_many_arguments)]
async fn advance_tool_queue(
    pending_tools: &mut VecDeque<PendingTool>,
    context: &mut Context,
    mode: &mut AgentMode,
    current_cancel: &mut Option<CancellationToken>,
    iterations: &mut usize,
    provider: Arc<dyn Provider>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) {
    if let Some(next) = pending_tools.front() {
        let _ = ui_tx
            .send(UiUpdate::ToolPending {
                name: next.call.name.clone(),
                id: next.call.id,
            })
            .await;
        return;
    }
    *iterations += 1;
    if *iterations > MAX_ITERATIONS {
        let _ = ui_tx
            .send(UiUpdate::Error("max ReAct iterations exceeded".to_string()))
            .await;
        *mode = AgentMode::Idle;
        return;
    }
    *mode = AgentMode::Streaming;
    *current_cancel = Some(start_request(context, provider, tools, ui_tx, agent_tx));
}

/// Spawns the inner streaming task for one request, built from an immutable
/// snapshot of the conversation history. Returns the `CancellationToken` that
/// the outer loop uses to abort this request.
///
/// This task owns the [`ToolCallAccumulator`]s and assembles raw
/// [`StreamEvent::Delta`] fragments into a batch of [`AssembledToolCall`]s
/// (blueprint §4A refinement); the outer loop stays the sole writer of
/// `Context`, and this task never writes it.
fn start_request(
    context: &Context,
    provider: Arc<dyn Provider>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let messages = context.messages().to_vec();
    let inner_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut stream = match provider.chat(messages, tools, inner_cancel).await {
            Ok(stream) => stream,
            Err(error) => {
                let message = format!("{error}");
                let _ = ui_tx.send(UiUpdate::Error(message.clone())).await;
                let _ = agent_tx.send(AgentEvent::StreamError(message)).await;
                return;
            }
        };
        let _ = ui_tx.send(UiUpdate::StreamStarted).await;
        let mut partial = String::new();
        let mut accumulators: HashMap<usize, ToolCallAccumulator> = HashMap::new();
        while let Some(event) = stream.recv().await {
            match event {
                StreamEvent::Text(text) => {
                    partial.push_str(&text);
                    let _ = ui_tx.send(UiUpdate::AppendText(text)).await;
                }
                StreamEvent::Delta(delta) => {
                    let accumulator =
                        accumulators
                            .entry(delta.tool_call_index)
                            .or_insert_with(|| ToolCallAccumulator {
                                id: Uuid::new_v4(),
                                name: None,
                                arguments: String::new(),
                            });
                    if let Some(name) = delta.function_name {
                        if accumulator.name.is_none() {
                            accumulator.name = Some(name);
                        }
                    }
                    if let Some(fragment) = delta.arguments_fragment {
                        accumulator.arguments.push_str(&fragment);
                    }
                }
                StreamEvent::Done => {
                    if accumulators.is_empty() {
                        let _ = ui_tx.send(UiUpdate::StreamDone).await;
                        let _ = agent_tx.send(AgentEvent::StreamDone { partial }).await;
                    } else {
                        let mut calls = Vec::new();
                        let mut error: Option<String> = None;
                        for (_, accumulator) in accumulators {
                            match accumulator.into_tool_call_checked() {
                                Ok(call) => calls.push(call),
                                Err(raw) => {
                                    error = Some(format!(
                                        "stream ended with incomplete tool-call JSON: {raw}"
                                    ));
                                    break;
                                }
                            }
                        }
                        if let Some(error) = error {
                            let _ = ui_tx.send(UiUpdate::Error(error.clone())).await;
                            let _ = agent_tx.send(AgentEvent::StreamError(error)).await;
                        } else {
                            let _ = agent_tx.send(AgentEvent::ToolCall { calls }).await;
                        }
                    }
                    break;
                }
                StreamEvent::Cancelled => {
                    if accumulators.is_empty() {
                        let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                    }
                    let _ = agent_tx.send(AgentEvent::StreamCancelled { partial }).await;
                    break;
                }
                StreamEvent::Error(error) => {
                    let message = format!("{error}");
                    let _ = ui_tx.send(UiUpdate::Error(message.clone())).await;
                    let _ = agent_tx.send(AgentEvent::StreamError(message)).await;
                    break;
                }
            }
        }
    });
    cancel
}

/// Buffers the fragmented pieces of one tool call (blueprint §4A).
///
/// Raw `Delta` fragments for a given `tool_call_index` are concatenated in
/// order; the name is captured from the first fragment that carries it and the
/// accumulated `arguments` JSON is only parsed once complete.
#[derive(Debug, Clone)]
struct ToolCallAccumulator {
    id: Uuid,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    /// Finalize this accumulator into a validated tool call, or return the raw
    /// argument fragment when the JSON is incomplete/unparseable.
    fn into_tool_call_checked(self) -> Result<AssembledToolCall, String> {
        let name = self.name.unwrap_or_default();
        if self.arguments.trim().is_empty() {
            return Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments: Value::Null,
            });
        }
        match serde_json::from_str(&self.arguments) {
            Ok(arguments) => Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments,
            }),
            Err(_) => Err(self.arguments),
        }
    }
}

/// Drains queued steers into context as user messages; returns true if any
/// were injected. Injecting is the only way a steer reaches the LLM — it
/// never cancels a live stream.
fn inject_pending_steers(pending_steers: &mut VecDeque<String>, context: &mut Context) -> bool {
    if pending_steers.is_empty() {
        return false;
    }
    while let Some(message) = pending_steers.pop_front() {
        context.push_user(message);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_message_variants() {
        let system = Message::System {
            content: "system".into(),
        };
        let user = Message::User {
            content: "user".into(),
        };
        let assistant = Message::Assistant {
            content: "assistant".into(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        };
        let tool_result = Message::ToolResult {
            id: Uuid::new_v4(),
            name: "tool".into(),
            result: "ok".into(),
        };
        let tool_error = Message::ToolError {
            id: Uuid::new_v4(),
            name: "tool".into(),
            error: "err".into(),
        };
        assert!(matches!(system, Message::System { .. }));
        assert!(matches!(user, Message::User { .. }));
        assert!(matches!(assistant, Message::Assistant { .. }));
        assert!(matches!(tool_result, Message::ToolResult { .. }));
        assert!(matches!(tool_error, Message::ToolError { .. }));
    }

    #[test]
    fn tool_call_request_uses_uuid_id() {
        let id = Uuid::new_v4();
        let request = ToolCallRequest {
            id,
            name: "search".into(),
            arguments: serde_json::json!({ "query": "rust" }),
        };
        assert_eq!(request.id, id);
        assert_eq!(request.name, "search");
        assert_eq!(request.arguments, serde_json::json!({ "query": "rust" }));
    }

    #[test]
    fn session_begins_with_seeded_system_messages() {
        let context = Context::new();
        assert_eq!(
            context.messages(),
            &[
                Message::System {
                    content: DEFAULT_SYSTEM_PROMPT.to_string(),
                },
                Message::System {
                    content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
                },
            ]
        );
    }

    #[test]
    fn push_messages_appends_in_order() {
        let mut context = Context::new();
        context.push_user("first user turn".to_string());
        context.push_assistant("first response".to_string(), AssistantStatus::Complete);
        context.push_user("second user turn".to_string());
        assert_eq!(context.messages().len(), 5);
        assert_eq!(
            context.messages()[2],
            Message::User {
                content: "first user turn".to_string(),
            }
        );
        assert_eq!(
            context.messages()[3],
            Message::Assistant {
                content: "first response".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            }
        );
        assert_eq!(
            context.messages()[4],
            Message::User {
                content: "second user turn".to_string(),
            }
        );
    }

    #[test]
    fn push_assistant_marks_interrupted_status() {
        let mut context = Context::new();
        context.push_assistant("partial".to_string(), AssistantStatus::Interrupted);
        assert_eq!(
            context.messages()[2],
            Message::Assistant {
                content: "partial".to_string(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            }
        );
    }

    #[test]
    fn push_assistant_tool_call_stores_calls() {
        let mut context = Context::new();
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "read".into(),
                arguments: serde_json::json!({ "path": "a.txt" }),
            }],
        );
        assert_eq!(
            context.messages()[2],
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                }],
            }
        );
    }

    #[test]
    fn push_tool_result_and_error_append_messages() {
        let mut context = Context::new();
        let id = Uuid::new_v4();
        context.push_tool_result(id, "read".into(), "contents".into());
        context.push_tool_error(id, "write".into(), "denied".into());
        assert_eq!(
            context.messages()[2],
            Message::ToolResult {
                id,
                name: "read".into(),
                result: "contents".into(),
            }
        );
        assert_eq!(
            context.messages()[3],
            Message::ToolError {
                id,
                name: "write".into(),
                error: "denied".into(),
            }
        );
    }

    #[test]
    fn messages_returns_immutable_read_back_slice() {
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let read_back = context.messages();
        assert_eq!(read_back.len(), 3);
        assert!(matches!(read_back[0], Message::System { .. }));
        assert!(matches!(read_back[1], Message::System { .. }));
        assert_eq!(
            read_back[2],
            Message::User {
                content: "hello".into()
            }
        );
    }

    #[test]
    fn construct_and_pattern_match_protocol_enums() {
        assert!(matches!(
            AgentCommand::Prompt("hello".into()),
            AgentCommand::Prompt(text) if text == "hello"
        ));
        assert!(matches!(AgentCommand::Stop, AgentCommand::Stop));
        let id = Uuid::new_v4();
        assert!(matches!(
            AgentCommand::ApproveTool { id },
            AgentCommand::ApproveTool { id: got } if got == id
        ));
        assert!(matches!(
            AgentCommand::DenyTool { id },
            AgentCommand::DenyTool { id: got } if got == id
        ));

        assert!(matches!(
            AgentEvent::StreamDone { partial: "done".into() },
            AgentEvent::StreamDone { partial } if partial == "done"
        ));
        assert!(matches!(
            AgentEvent::StreamCancelled { partial: "cancel".into() },
            AgentEvent::StreamCancelled { partial } if partial == "cancel"
        ));
        assert!(matches!(
            AgentEvent::StreamError("boom".into()),
            AgentEvent::StreamError(message) if message == "boom"
        ));

        assert!(
            matches!(UiUpdate::AppendText("tok".into()), UiUpdate::AppendText(text) if text == "tok")
        );
        assert!(matches!(UiUpdate::StreamStarted, UiUpdate::StreamStarted));
        assert!(matches!(UiUpdate::StreamDone, UiUpdate::StreamDone));
        assert!(matches!(
            UiUpdate::StreamCancelled,
            UiUpdate::StreamCancelled
        ));
        assert!(
            matches!(UiUpdate::Error("oops".into()), UiUpdate::Error(message) if message == "oops")
        );

        assert!(
            matches!(StreamEvent::Text("tok".into()), StreamEvent::Text(text) if text == "tok")
        );
        assert!(matches!(StreamEvent::Done, StreamEvent::Done));
        assert!(matches!(StreamEvent::Cancelled, StreamEvent::Cancelled));
        assert!(matches!(
            StreamEvent::Error(anyhow::anyhow!("network")),
            StreamEvent::Error(error) if format!("{error}") == "network"
        ));
        assert!(matches!(
            StreamEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: None,
                function_name: Some("read".into()),
                arguments_fragment: Some("{\"path\":\"a.txt\"}".into()),
            }),
            StreamEvent::Delta(_)
        ));

        assert_eq!(AgentMode::Idle, AgentMode::Idle);
        assert_eq!(AgentMode::Streaming, AgentMode::Streaming);
        assert_eq!(AgentMode::ToolPending, AgentMode::ToolPending);
    }

    #[test]
    fn accumulator_assembles_single_fragment_call() {
        let mut acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: None,
            arguments: String::new(),
        };
        acc.name = Some("read".to_string());
        acc.arguments.push_str(r#"{"path":"a.txt"}"#);
        let call = acc.into_tool_call_checked().expect("valid arguments");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, serde_json::json!({ "path": "a.txt" }));
    }

    #[test]
    fn accumulator_concatenates_fragmented_arguments_across_indices() {
        let mut acc_0 = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("read".into()),
            arguments: String::new(),
        };
        let mut acc_1 = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("glob".into()),
            arguments: String::new(),
        };
        acc_0.arguments.push_str(r#"{"path""#);
        acc_1.arguments.push_str(r#"{"pattern""#);
        acc_0.arguments.push_str(r#":"a.txt"}"#);
        acc_1.arguments.push_str(r#":"*.rs"}"#);
        let call_0 = acc_0.into_tool_call_checked().expect("a.txt parses");
        let call_1 = acc_1.into_tool_call_checked().expect("glob parses");
        assert_eq!(call_0.arguments, serde_json::json!({ "path": "a.txt" }));
        assert_eq!(call_1.arguments, serde_json::json!({ "pattern": "*.rs" }));
    }

    #[test]
    fn accumulator_reports_incomplete_json_error_with_fragment() {
        let acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("read".into()),
            arguments: r#"{"path":"unterminated"#.to_string(),
        };
        let err = acc.into_tool_call_checked().expect_err("incomplete JSON");
        assert_eq!(err, r#"{"path":"unterminated"#);
    }

    #[test]
    fn accumulator_empty_arguments_yields_null() {
        let acc = ToolCallAccumulator {
            id: Uuid::new_v4(),
            name: Some("glob".into()),
            arguments: String::new(),
        };
        let call = acc.into_tool_call_checked().expect("empty is valid");
        assert_eq!(call.arguments, Value::Null);
    }
}
