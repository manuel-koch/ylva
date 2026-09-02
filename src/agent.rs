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
use tracing::Instrument;
use uuid::Uuid;

use crate::engine::Engine;
use crate::llm::{ToolEnvelope, ToolSchema};

pub mod tools;

use tools::{ToolCall, ToolRegistry};

/// Maximum number of LLM requests the tool-use loop starts within a single user
/// turn before the guard stops re-prompting. Per-engine configurability is a
/// later (Phase 5) concern; this module-level default is the Phase 3 SSOT.
pub const MAX_TOOL_USE_ITERATIONS: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Idle,
    /// Prompt sent to the provider; waiting for the first `StreamStarted`
    /// event. The agent is busy — a second message typed now is a steer.
    Pending,
    Streaming,
    ToolPending,
    /// A tool has been approved and is actively executing.
    ToolRunning,
    ClarifyPending,
    /// User requested abort (double Ctrl+C); `AgentCommand::Stop` has been
    /// sent. The TUI waits for the resulting `StreamCancelled` / `Idle`
    /// transition before quitting.
    Aborting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    Stop {
        reason: String,
    },
    ApproveTool {
        id: Uuid,
    },
    DenyTool {
        id: Uuid,
        reason: String,
    },
    AnswerClarify(String),
    /// Abort the pending clarify call without an answer.
    /// The agent records a tool error and continues, so the LLM can react.
    AbortClarify,
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
        result_metadata: Option<serde_json::Value>,
        result_metadata_schema: Option<serde_json::Value>,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
        partial_output: Option<String>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
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
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    ToolPending {
        name: String,
        id: Uuid,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
    },
    ToolDone {
        id: Uuid,
        name: String,
        result_section: crate::tui_reducer_state::ResultSection,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    ToolError {
        id: Uuid,
        error: String,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    /// A tool call was denied automatically by the sandbox before it started.
    /// Creates a `Denied` bubble directly — no `ToolStart` is ever sent.
    ToolAutoDenied {
        id: Uuid,
        name: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        content_sections: Vec<crate::agent::tools::Section>,
        reason: String,
    },
    ToolKilled {
        id: Uuid,
    },
    /// A steer (user message while busy) was queued. No payload — the TUI
    /// driver creates the pending bubble itself before sending the command.
    SteerQueued,
    /// All previously queued steers have been injected into the LLM context.
    SteerApplied,
    /// A fragment of extended-thinking / reasoning-token output arrived.
    /// Appended to `partial_thinking_text`, never to the assistant-text
    /// accumulator; flushed into a `ChatEntry::ThinkingMessage` bubble ahead
    /// of the assistant bubble on `StreamDone`.
    AppendThinking(String),
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
    /// A fragment of extended-thinking / reasoning-token output. Distinct
    /// from `Text` so it never contaminates the assistant text accumulator;
    /// it is purely a model capability (models must have extended-thinking
    /// / reasoning enabled to produce it) and is provider-agnostic.
    Thinking(String),
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

/// Distinguishes user messages that were typed explicitly by the user from
/// those injected by the agent harness as model hints.
///
/// `Explicit` messages are rendered in the TUI conversation list and the
/// headless CLI output. `ImplicitToolNote` messages are sent to the model
/// as regular `role: "user"` turns but are **never rendered** in any UI —
/// they exist solely to give the model richer context (e.g. a denial reason).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UserMessageKind {
    #[default]
    Explicit,
    ImplicitToolNote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
        kind: UserMessageKind,
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
        /// Structured metadata emitted by the tool for this specific result.
        result_metadata: Option<serde_json::Value>,
        /// JSON Schema for the fields present in `result_metadata`.
        result_metadata_schema: Option<serde_json::Value>,
    },
    ToolError {
        id: Uuid,
        name: String,
        error: String,
        duration: std::time::Duration,
    },
}

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
        self.messages.push(Message::User {
            content: text,
            kind: UserMessageKind::Explicit,
        });
    }

    /// Push a model hint injected by the agent harness. The message is sent
    /// to the model as a regular `role: "user"` turn but is never rendered in
    /// any UI (TUI conversation list or headless CLI output).
    pub fn push_implicit_user(&mut self, text: String) {
        self.messages.push(Message::User {
            content: text,
            kind: UserMessageKind::ImplicitToolNote,
        });
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

    pub fn push_tool_result(
        &mut self,
        id: Uuid,
        name: String,
        result: String,
        result_metadata: Option<serde_json::Value>,
        result_metadata_schema: Option<serde_json::Value>,
    ) {
        self.messages.push(Message::ToolResult {
            id,
            name,
            result,
            result_metadata,
            result_metadata_schema,
        });
    }

    pub fn push_tool_error(
        &mut self,
        id: Uuid,
        name: String,
        error: String,
        partial_output: Option<&str>,
    ) {
        let llm_error = match partial_output.filter(|s| !s.is_empty()) {
            Some(output) => format!(
                "{error}\n\nThe tool produced the following output before being terminated:\n\
                 {output}"
            ),
            None => error,
        };
        self.messages.push(Message::ToolError {
            id,
            name,
            error: llm_error,
            duration: std::time::Duration::ZERO,
        });
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
pub struct PendingTool {
    pub call: Box<dyn ToolCall>,
}

impl std::fmt::Debug for PendingTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingTool")
            .field("name", &self.call.name())
            .field("id", &self.call.id())
            .finish()
    }
}

/// Default execution timeout for file-system tools (blueprint §4D: 5s).
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 5;

impl PendingTool {
    /// Spawn the tool in its own task, bounded by `timeout_secs` (or the
    /// default) and a per-tool `CancellationToken`. Completion is reported via
    /// `agent_tx` (`ToolDone`/`ToolError`/`ToolKilled`); returns the token the
    /// outer loop stores as the running tool's cancel.
    pub fn execute(self, agent_tx: mpsc::Sender<AgentEvent>) -> CancellationToken {
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let id = self.call.id();
        let name = self.call.name().to_string();
        let timeout_secs = self.call.timeout_secs();
        let span = tracing::info_span!("tool_execute", tool = %name, tool_call_id = %id);
        tokio::spawn(
            async move {
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
                        let start = std::time::Instant::now();
                        // TODO: remove artificial delay
                        #[cfg(feature = "artificial-tool-delay")]
                        {
                            let delay_secs = 2
                                + (std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .subsec_nanos()
                                    % 3) as u64;
                            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        }
                        // END TODO
                        let result = self.call.run().await;
                        let elapsed = start.elapsed();
                        (result, elapsed)
                    }) => result,
                };
                match outcome {
                    Ok((Ok(tool_output), duration)) => {
                        let result_metadata = tool_output.metadata().and_then(|m| {
                            if m.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                                None
                            } else {
                                Some(m)
                            }
                        });
                        let result_metadata_schema = result_metadata.as_ref().and_then(|_| {
                            tool_output
                                .metadata_schema()
                                .and_then(|schema| serde_json::to_value(schema).ok())
                        });
                        let result = tool_output.text();
                        // All metadata keys become the UI summary (key=value).
                        let summary: Vec<(String, String)> = result_metadata
                            .as_ref()
                            .and_then(|m| {
                                if let serde_json::Value::Object(obj) = m {
                                    Some(obj)
                                } else {
                                    None
                                }
                            })
                            .map(|obj| {
                                obj.iter()
                                    .map(|(k, v)| {
                                        let display = match v {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        (k.clone(), display)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let content_sections = tool_output.content_sections();
                        let _ = agent_tx
                            .send(AgentEvent::ToolDone {
                                id,
                                name,
                                result,
                                result_metadata,
                                result_metadata_schema,
                                summary,
                                duration,
                                content_sections,
                            })
                            .await;
                    }
                    Ok((Err(error), duration)) => {
                        let partial_output = error
                            .partial_output()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        let content_sections: Vec<crate::agent::tools::Section> =
                            if let Some(output) = &partial_output {
                                vec![crate::agent::tools::Section::Content(
                                    crate::agent::tools::ContentSection {
                                        label: Some("partial output".to_string()),
                                        content: output.clone(),
                                        file_extension: None,
                                        line_number_offset: Some(0),
                                    },
                                )]
                            } else {
                                vec![]
                            };
                        let _ = agent_tx
                            .send(AgentEvent::ToolError {
                                id,
                                name,
                                error: error.to_string(),
                                partial_output,
                                duration,
                                content_sections,
                            })
                            .await;
                    }
                    Err(_elapsed) => {
                        let message = format!("Tool '{name}' timed out after {timeout_secs}s");
                        let _ = agent_tx
                            .send(AgentEvent::ToolError {
                                id,
                                name,
                                error: message,
                                partial_output: None,
                                duration: Duration::from_secs(timeout_secs),
                                content_sections: vec![],
                            })
                            .await;
                    }
                }
            }
            .instrument(span),
        );
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
/// Indicates whether an `AgentLoop` is acting as the root (top-level) agent
/// or as a child agent spawned by a root agent.
///
/// `Child` is a placeholder in Phase 5; `AgentFactory::create` returns `Err`
/// when asked to create a `Child` agent. Full implementation comes in Phase 6.
pub enum Role {
    /// The top-level agent for a session.
    Root,
    /// A child agent spawned by a root agent. Not yet constructable via
    /// `AgentFactory::create`; will be implemented in Phase 6.
    Child,
}

/// The outer loop task is the sole writer of `Context` (`single-writer`
/// rule): no `Arc<Mutex<Context>>` anywhere. `run()` takes the loop's channel
/// receivers out of `self`; the caller keeps `self` (and thus `cmd_tx`) alive
/// and shuts the loop down by **dropping the `AgentLoop`**, after which the
/// task returns the final committed `Context` from `handle.await`.
pub struct AgentLoop {
    engine: Arc<dyn Engine>,
    registry: Arc<ToolRegistry>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    cmd_rx: Option<mpsc::Receiver<AgentCommand>>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
    agent_rx: Option<mpsc::Receiver<AgentEvent>>,
}

impl AgentLoop {
    pub fn new(
        engine: Arc<dyn Engine>,
        registry: Arc<ToolRegistry>,
    ) -> (Self, mpsc::Receiver<UiUpdate>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(10);
        let (ui_tx, ui_rx) = mpsc::channel::<UiUpdate>(100);
        let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>(10);
        let agent = AgentLoop {
            engine,
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

    /// Clone of the command channel sender, for consumers (e.g. the TUI
    /// driver and the SIGINT relay) that send commands without a `&self`.
    pub fn command_sender(&self) -> mpsc::Sender<AgentCommand> {
        self.cmd_tx.clone()
    }

    pub fn run(&mut self) -> tokio::task::JoinHandle<anyhow::Result<Context>> {
        let engine = self.engine.clone();
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
            let mut clarify_started_at: Option<std::time::Instant> = None;

            loop {
                tokio::select! {
                    biased;
                    Some(event) = agent_rx.recv() => {
                        match event {
                            AgentEvent::StreamDone { partial } => {
                                context.push_assistant(partial, AssistantStatus::Complete);
                                if inject_pending_steers(&mut pending_steers, &mut context) {
                                    let _ = ui_tx.send(UiUpdate::SteerApplied).await;
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        engine.clone(),
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
                                tracing::error!(message = %error, "agent stream error");
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
                                for assembled in calls {
                                    match registry.create_call(
                                        &assembled.name,
                                        assembled.id,
                                        assembled.arguments.clone(),
                                    ) {
                                        Some(call) => {
                                            pending_tools.push_back(PendingTool { call });
                                        }
                                        None => {
                                            let error =
                                                format!("Unknown tool '{}'", assembled.name);
                                            let _ = agent_tx
                                                .send(AgentEvent::ToolError {
                                                    id: assembled.id,
                                                    name: assembled.name.clone(),
                                                    error,
                                                    partial_output: None,
                                                    duration: std::time::Duration::ZERO,
                                                    content_sections: vec![],
                                                })
                                                .await;
                                        }
                                    }
                                }
                                mode = AgentMode::ToolPending;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    false,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                )
                                .await;
                            }
                            AgentEvent::ToolDone { id, name, result, result_metadata, result_metadata_schema, summary, duration, content_sections } => {
                                current_tool_cancel = None;
                                context.push_tool_result(id, name.clone(), result.clone(), result_metadata, result_metadata_schema);
                                let _ = ui_tx
                                    .send(UiUpdate::ToolDone {
                                        id,
                                        name,
                                        result_section: crate::tui_reducer_state::ResultSection::plain(result),
                                        summary,
                                        duration,
                                        content_sections,
                                    })
                                    .await;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    true,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                )
                                .await;
                            }
                            AgentEvent::ToolError { id, name, error, partial_output, duration, content_sections } => {
                                current_tool_cancel = None;
                                context.push_tool_error(id, name, error.clone(), partial_output.as_deref());
                                let _ = ui_tx.send(UiUpdate::ToolError {
                                    id,
                                    error,
                                    duration,
                                    content_sections,
                                }).await;
                                advance_tool_queue(
                                    &mut pending_tools,
                                    &mut context,
                                    &mut mode,
                                    &mut current_cancel,
                                    &mut current_tool_cancel,
                                    &mut iterations,
                                    true,
                                    engine.clone(),
                                    Arc::clone(&registry),
                                    ui_tx.clone(),
                                    agent_tx.clone(),
                                    &mut clarify_started_at,
                                    &mut pending_steers,
                                )
                                .await;
                            }
                            AgentEvent::ToolKilled { id, name: _ } => {
                                tracing::info!(%id, "tool killed; agent returning to idle");
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
                                tracing::debug!(text_len = text.len(), "command: prompt");
                                if mode == AgentMode::Idle {
                                    iterations = 1;
                                    context.push_user(text);
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        engine.clone(),
                                        registry.schemas(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                    ));
                                } else {
                                    // Cooperative steering: never cancel the live
                                    // stream. Queued from ToolPending too; injection
                                    // is deferred (Phase 4 adds descope_pending_tools).
                                    pending_steers.push_back(text);
                                    let _ = ui_tx.send(UiUpdate::SteerQueued).await;
                                }
                            }
                            AgentCommand::ApproveTool { id } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id() == id);
                                if is_front {
                                    let pending =
                                        pending_tools.pop_front().expect("checked front");
                                    let name = pending.call.name().to_string();
                                    let call_id = pending.call.id();
                                    let arguments = pending.call.arguments().clone();
                                    let content_sections = pending.call.pending_content_sections();
                                    let title_arguments = pending.call.title_arguments();
                                    tracing::info!(tool = %name, tool_call_id = %call_id, "tool approved");
                                    current_tool_cancel = Some(PendingTool::execute(
                                        pending,
                                        agent_tx.clone(),
                                    ));
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolStart {
                                            name: name.clone(),
                                            id: call_id,
                                            arguments: arguments.clone(),
                                            content_sections,
                                            title_arguments,
                                        })
                                        .await;
                                }
                            }
                            AgentCommand::DenyTool { id, reason } => {
                                // Only valid in `ToolPending`; ignored otherwise.
                                let is_front = mode == AgentMode::ToolPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.id() == id);
                                if is_front {
                                    let denied =
                                        pending_tools.pop_front().expect("checked front");
                                    let name = denied.call.name().to_string();
                                    let call_id = denied.call.id();
                                    tracing::info!(tool = %name, tool_call_id = %call_id, "tool denied");
                                    let mut error = format!("Tool '{name}' was denied by the user");
                                    if !reason.is_empty() {
                                        error.push_str(&format!(
                                            ". Here is their reason for the denial:\n\n{reason}"
                                        ));
                                    }
                                    context.push_tool_error(call_id, name.clone(), error.clone(), None);
                                    // When the user supplied a reason, also push a
                                    // `user` role message so the model reads the
                                    // correction through the authoritative instruction
                                    // channel rather than just the tool-error envelope.
                                    if !reason.is_empty() {
                                        context.push_implicit_user(format!(
                                            "I denied the '{name}' tool call \
                                             ( tool_call_id={call_id} ). \
                                             Here is my reason for the denial:\n\n{reason}"
                                        ));
                                    }
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolError {
                                            id: call_id,
                                            error,
                                            duration: std::time::Duration::ZERO,
                                            content_sections: vec![],
                                        })
                                        .await;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::AnswerClarify(answer) => {
                                let is_front = mode == AgentMode::ClarifyPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.name() == "clarify");
                                if is_front {
                                    let pending = pending_tools.pop_front().expect("checked front");
                                    let call_id = pending.call.id();
                                    let choices_sections =
                                        tools::clarify::ClarifyTool::answer_display_sections(
                                            pending.call.arguments(),
                                        );
                                    context.push_tool_result(
                                        call_id,
                                        tools::clarify::ClarifyTool::NAME.to_string(),
                                        answer.clone(),
                                        None,
                                        None,
                                    );
                                    let duration = clarify_started_at
                                        .take()
                                        .map(|started| started.elapsed())
                                        .unwrap_or(std::time::Duration::ZERO);
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolDone {
                                            id: call_id,
                                            name: tools::clarify::ClarifyTool::NAME.to_string(),
                                            result_section: crate::tui_reducer_state::ResultSection {
                                                id: crate::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                                                content: answer,
                                            },
                                            summary: vec![],
                                            duration,
                                            content_sections: choices_sections,
                                        })
                                        .await;
                                    mode = AgentMode::ToolPending;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::AbortClarify => {
                                let is_front = mode == AgentMode::ClarifyPending
                                    && pending_tools
                                        .front()
                                        .is_some_and(|pending| pending.call.name() == "clarify");
                                if is_front {
                                    let pending = pending_tools.pop_front().expect("checked front");
                                    let call_id = pending.call.id();
                                    let error =
                                        "User aborted the clarify question without answering."
                                            .to_string();
                                    context.push_tool_error(
                                        call_id,
                                        tools::clarify::ClarifyTool::NAME.to_string(),
                                        error.clone(),
                                        None,
                                    );
                                    let _ = ui_tx
                                        .send(UiUpdate::ToolError {
                                            id: call_id,
                                            error,
                                            duration: std::time::Duration::ZERO,
                                            content_sections: vec![],
                                        })
                                        .await;
                                    mode = AgentMode::ToolPending;
                                    advance_tool_queue(
                                        &mut pending_tools,
                                        &mut context,
                                        &mut mode,
                                        &mut current_cancel,
                                        &mut current_tool_cancel,
                                        &mut iterations,
                                        true,
                                        engine.clone(),
                                        Arc::clone(&registry),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                        &mut clarify_started_at,
                                        &mut pending_steers,
                                    )
                                    .await;
                                }
                            }
                            AgentCommand::Stop { reason } => {
                                tracing::info!(
                                    %reason,
                                    has_stream = current_cancel.is_some(),
                                    has_tool = current_tool_cancel.is_some(),
                                    ?mode,
                                    "stop command received"
                                );
                                // Interrupt: hard-abort the current round.
                                pending_steers.clear();
                                if let Some(cancel_token) = current_cancel.take() {
                                    cancel_token.cancel();
                                }
                                if let Some(tool_cancel) = current_tool_cancel.take() {
                                    tool_cancel.cancel();
                                }
                                if mode == AgentMode::ClarifyPending {
                                    if let Some(pending) = pending_tools.pop_front() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let mut error =
                                            format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(id, name, error.clone(), None);
                                        let _ = ui_tx
                                            .send(UiUpdate::ToolError {
                                                id,
                                                error,
                                                duration: std::time::Duration::ZERO,
                                                content_sections: vec![],
                                            })
                                            .await;
                                    }
                                    for pending in pending_tools.iter() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let mut error =
                                            format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(id, name, error, None);
                                    }
                                    pending_tools.clear();
                                    mode = AgentMode::Idle;
                                } else if mode == AgentMode::ToolPending {
                                    // Record a denial result for every pending
                                    // tool so the model history stays consistent
                                    // (every tool call must have a result).
                                    // Only the front tool has a visible bubble;
                                    // the rest are queued without one yet.
                                    let mut first = true;
                                    for pending in pending_tools.iter() {
                                        let name = pending.call.name().to_string();
                                        let id = pending.call.id();
                                        let mut error = format!("Tool '{name}' was denied by the user");
                                        if !reason.is_empty() {
                                            error.push_str(&format!(
                                                ". Here is their reason for the denial:\n\n{reason}"
                                            ));
                                        }
                                        context.push_tool_error(id, name, error.clone(), None);
                                        if first {
                                            let _ = ui_tx
                                                .send(UiUpdate::ToolError {
                                                    id,
                                                    error,
                                                    duration: std::time::Duration::ZERO,
                                                    content_sections: vec![],
                                                })
                                                .await;
                                            first = false;
                                        }
                                    }
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

/// Log the "tool approval requested" event for the given pending call using the
/// tool's own non-default redacted arguments so sensitive values are never
/// written to the log.
fn log_tool_approval_request(call: &dyn ToolCall) {
    let safe_args: serde_json::Map<String, serde_json::Value> =
        call.non_default_redacted_arguments().into_iter().collect();
    tracing::info!(
        tool = %call.name(),
        tool_call_id = %call.id(),
        args = %serde_json::to_string(&safe_args).unwrap_or_else(|_| "{}".to_string()),
        "tool approval requested"
    );
}

/// Advance the pending-tool queue. Processes auto-approved and auto-denied
/// tools immediately in a loop; stops at the first `UserApprovalRequired`
/// tool (sends `ToolPending`) or re-prompts the model when the queue is
/// empty.
///
/// `increment_iterations` is `false` when called immediately after a new
/// `ToolCall` batch arrives (the iteration counter was already set) and
/// `true` after a tool finishes executing (each completed tool round counts).
#[allow(clippy::too_many_arguments)]
async fn advance_tool_queue(
    pending_tools: &mut VecDeque<PendingTool>,
    context: &mut Context,
    mode: &mut AgentMode,
    current_cancel: &mut Option<CancellationToken>,
    current_tool_cancel: &mut Option<CancellationToken>,
    iterations: &mut usize,
    increment_iterations: bool,
    engine: Arc<dyn Engine>,
    registry: Arc<ToolRegistry>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
    clarify_started_at: &mut Option<std::time::Instant>,
    pending_steers: &mut VecDeque<String>,
) {
    loop {
        match pending_tools.front() {
            None => {
                // Queue empty: optionally increment and re-prompt.
                if increment_iterations {
                    *iterations += 1;
                    tracing::debug!(iterations = *iterations, "advancing tool queue");
                    if *iterations > MAX_TOOL_USE_ITERATIONS {
                        tracing::warn!(
                            iterations = *iterations,
                            "max tool-use iterations exceeded"
                        );
                        let message = format!(
                            "Max tool-use iterations ({}) exceeded.",
                            MAX_TOOL_USE_ITERATIONS
                        );
                        context.push_assistant(message.clone(), AssistantStatus::Interrupted);
                        let _ = ui_tx.send(UiUpdate::Error(message)).await;
                        *mode = AgentMode::Idle;
                        return;
                    }
                }
                // All tool results for this round are now in context — this is
                // the earliest safe point to inject queued steers (pending user
                // messages typed while tools were running), so they reach the
                // model on the very next request rather than waiting for a full
                // tool-free turn to complete via StreamDone.
                if inject_pending_steers(pending_steers, context) {
                    let _ = ui_tx.send(UiUpdate::SteerApplied).await;
                }
                *mode = AgentMode::Streaming;
                *current_cancel = Some(start_request(
                    context,
                    engine,
                    registry.schemas(),
                    ui_tx,
                    agent_tx,
                ));
                return;
            }
            Some(front) => {
                let approval = {
                    let _span = tracing::debug_span!(
                        "needs_approval",
                        tool = %front.call.name(),
                        tool_call_id = %front.call.id(),
                    )
                    .entered();
                    // Validate arguments against the tool's input schema first.
                    // Structurally invalid calls are denied immediately without an approval
                    // prompt, for all tools uniformly.
                    let validation = registry
                        .get(front.call.name())
                        .map(|tool| tool.validate_args(front.call.arguments()));
                    match validation {
                        Some(Err(reason)) => crate::sandbox::ApprovalCheck::AutoDenied { reason },
                        _ => registry
                            .get(front.call.name())
                            .map(|tool| tool.needs_approval(front.call.arguments()))
                            .unwrap_or(crate::sandbox::ApprovalCheck::UserApprovalRequired),
                    }
                };
                match approval {
                    crate::sandbox::ApprovalCheck::AutoApproved => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let arguments = pending.call.arguments().clone();
                        let content_sections = pending.call.pending_content_sections();
                        let title_arguments = pending.call.title_arguments();
                        tracing::info!(tool = %name, tool_call_id = %call_id, "tool auto-approved");
                        *current_tool_cancel =
                            Some(PendingTool::execute(pending, agent_tx.clone()));
                        let _ = ui_tx
                            .send(UiUpdate::ToolStart {
                                name,
                                id: call_id,
                                arguments,
                                content_sections,
                                title_arguments,
                            })
                            .await;
                        return; // wait for ToolDone/ToolError
                    }
                    crate::sandbox::ApprovalCheck::AutoDenied { reason } => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let title_arguments = pending.call.title_arguments();
                        let content_sections = pending.call.pending_content_sections();
                        tracing::info!(tool = %name, tool_call_id = %call_id, %reason, "tool auto-denied");
                        context.push_tool_error(call_id, name.clone(), reason.clone(), None);
                        let _ = ui_tx
                            .send(UiUpdate::ToolAutoDenied {
                                id: call_id,
                                name,
                                title_arguments,
                                content_sections,
                                reason,
                            })
                            .await;
                        // Continue loop to handle next tool in queue.
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired
                        if front.call.name() == tools::clarify::ClarifyTool::NAME =>
                    {
                        let args = front.call.arguments();
                        let question = args
                            .get("question")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        let context = args
                            .get("context")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let choices =
                            args.get("choices")
                                .and_then(|value| value.as_array())
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(|value| value.as_str().map(str::to_string))
                                        .collect()
                                });
                        let call_id = front.call.id();
                        *mode = AgentMode::ClarifyPending;
                        *clarify_started_at = Some(std::time::Instant::now());
                        let _ = ui_tx
                            .send(UiUpdate::ClarifyPending {
                                id: call_id,
                                question,
                                context,
                                choices,
                            })
                            .await;
                        return;
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired => {
                        log_tool_approval_request(front.call.as_ref());
                        *mode = AgentMode::ToolPending;
                        let _ = ui_tx
                            .send(UiUpdate::ToolPending {
                                name: front.call.name().to_string(),
                                id: front.call.id(),
                                arguments: front.call.arguments().clone(),
                                content_sections: front.call.pending_content_sections(),
                                title_arguments: front.call.title_arguments(),
                            })
                            .await;
                        return;
                    }
                }
            }
        }
    }
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
    engine: Arc<dyn Engine>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let messages = context.messages().to_vec();
    let inner_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut stream = match engine.chat(messages, tools, inner_cancel.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                // If the cancel token was already fired (e.g. user aborted
                // while the server was still thinking), treat the resulting
                // error as a cancellation rather than a hard error so
                // domain_aborting receives StreamCancelled and exits cleanly.
                if inner_cancel.is_cancelled() {
                    tracing::debug!("engine chat failed after cancel; treating as StreamCancelled");
                    let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                    let _ = agent_tx
                        .send(AgentEvent::StreamCancelled {
                            partial: String::new(),
                        })
                        .await;
                } else {
                    let message = format!("{error}");
                    tracing::error!(message = %message, "engine chat failed");
                    let _ = ui_tx.send(UiUpdate::Error(message.clone())).await;
                    let _ = agent_tx.send(AgentEvent::StreamError(message)).await;
                }
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
                StreamEvent::Thinking(text) => {
                    // Never appended to `partial` — thinking content must
                    // not affect the assistant text or tool-call logic.
                    let _ = ui_tx.send(UiUpdate::AppendThinking(text)).await;
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
    use std::path::PathBuf;

    use crate::test_support::capture_logs;
    use crate::{
        agent::tools::{clarify::ClarifyTool, fs::WriteTool},
        engine::{Engine, EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH},
        llm::MockProvider,
        sandbox::{Sandbox, SandboxContext},
    };

    fn test_engine() -> Arc<dyn Engine> {
        Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
            EngineConfig {
                provider: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: DEFAULT_CONTEXT_LENGTH,
                api_base_url: None,
                api_key: None,
            },
        ))
    }

    fn test_sandbox() -> std::sync::Arc<std::sync::RwLock<Sandbox>> {
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::with_root(PathBuf::from(
            env!("CARGO_MANIFEST_DIR"),
        ))))
    }

    #[test]
    fn constructs_message_variants() {
        let system = Message::System {
            content: "system".into(),
        };
        let user = Message::User {
            content: "user".into(),
            kind: UserMessageKind::Explicit,
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
            result_metadata: None,
            result_metadata_schema: None,
        };
        let tool_error = Message::ToolError {
            id: Uuid::new_v4(),
            name: "tool".into(),
            error: "err".into(),
            duration: std::time::Duration::ZERO,
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
                kind: UserMessageKind::Explicit,
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
                kind: UserMessageKind::Explicit,
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
        context.push_tool_result(id, "read".into(), "contents".into(), None, None);
        context.push_tool_error(id, "write".into(), "denied".into(), None);
        assert_eq!(
            context.messages()[2],
            Message::ToolResult {
                id,
                name: "read".into(),
                result: "contents".into(),
                result_metadata: None,
                result_metadata_schema: None,
            }
        );
        assert_eq!(
            context.messages()[3],
            Message::ToolError {
                id,
                name: "write".into(),
                error: "denied".into(),
                duration: std::time::Duration::ZERO,
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
                content: "hello".into(),
                kind: UserMessageKind::Explicit,
            }
        );
    }

    #[test]
    fn construct_and_pattern_match_protocol_enums() {
        assert!(matches!(
            AgentCommand::Prompt("hello".into()),
            AgentCommand::Prompt(text) if text == "hello"
        ));
        assert!(matches!(
            AgentCommand::Stop {
                reason: String::new()
            },
            AgentCommand::Stop { .. }
        ));
        let id = Uuid::new_v4();
        assert!(matches!(
            AgentCommand::ApproveTool { id },
            AgentCommand::ApproveTool { id: got } if got == id
        ));
        assert!(matches!(
            AgentCommand::DenyTool { id, reason: "r".into() },
            AgentCommand::DenyTool { id: got, .. } if got == id
        ));
        assert!(matches!(
            AgentCommand::AnswerClarify("answer".into()),
            AgentCommand::AnswerClarify(answer) if answer == "answer"
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
        assert!(matches!(
            UiUpdate::AppendThinking("hmm".into()),
            UiUpdate::AppendThinking(text) if text == "hmm"
        ));
        assert!(matches!(UiUpdate::StreamStarted, UiUpdate::StreamStarted));
        assert!(matches!(UiUpdate::StreamDone, UiUpdate::StreamDone));
        assert!(matches!(
            UiUpdate::StreamCancelled,
            UiUpdate::StreamCancelled
        ));
        assert!(matches!(
            UiUpdate::ClarifyPending {
                id,
                question: "question".into(),
                context: None,
                choices: None,
            },
            UiUpdate::ClarifyPending {
                id: got,
                question: got_question,
                context: _,
                choices: None,
            } if got == id && got_question == "question"
        ));
        assert!(
            matches!(UiUpdate::Error("oops".into()), UiUpdate::Error(message) if message == "oops")
        );

        assert!(
            matches!(StreamEvent::Text("tok".into()), StreamEvent::Text(text) if text == "tok")
        );
        assert!(matches!(
            StreamEvent::Thinking("hmm".into()),
            StreamEvent::Thinking(text) if text == "hmm"
        ));
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
        assert_eq!(AgentMode::ClarifyPending, AgentMode::ClarifyPending);
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

    #[test]
    fn tool_execution_log_events_carry_spanned_tool_call_id() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let write_path = dir.path().join("out.txt");
        let log = capture_logs(async {
            let id = Uuid::new_v4();
            let (agent_tx, mut agent_rx) = mpsc::channel(8);
            let registry = tools::default_registry(
                std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
                tools::ToolRegistryConfig::default(),
            );
            let pending = PendingTool {
                call: registry
                    .create_call(
                        "write",
                        id,
                        serde_json::json!({
                            "path": write_path.display().to_string(),
                            "content": "abc",
                        }),
                    )
                    .expect("write tool registered"),
            };
            let _cancel = pending.execute(agent_tx);
            tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(event) = agent_rx.recv().await {
                    if matches!(event, AgentEvent::ToolDone { .. }) {
                        break;
                    }
                }
            })
            .await
            .expect("tool completes in time");
        });
        let contents = log;
        assert!(
            contents.contains("tool_execute{tool=write"),
            "tool span with its tool field must be rendered: {contents}"
        );
        assert!(
            contents.contains("tool_call_id="),
            "tool call id must be attached to the tool's log events: {contents}"
        );
        assert!(
            contents.contains("wrote file"),
            "the write tool's log event must be inside the tool span: {contents}"
        );
        assert!(
            !contents.contains("abc"),
            "the write tool must not log the written content: {contents}"
        );
        let written = std::fs::read_to_string(&write_path).expect("read written file");
        assert_eq!(written, "abc");
    }

    #[tokio::test]
    async fn advance_tool_queue_emits_clarify_pending_for_clarify_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ClarifyTool));
        let registry = Arc::new(registry);
        let call_id = Uuid::new_v4();
        let mut pending_tools = VecDeque::from([PendingTool {
            call: registry
                .create_call(
                    "clarify",
                    call_id,
                    serde_json::json!({
                        "question": "Pick one",
                        "choices": ["a", "b"],
                    }),
                )
                .expect("clarify tool registered"),
        }]);
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            false,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            &mut clarify_started_at,
            &mut pending_steers,
        )
        .await;

        assert_eq!(mode, AgentMode::ClarifyPending);
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ClarifyPending {
                id,
                question,
                context: _,
                choices: Some(choices),
            }) if id == call_id && question == "Pick one" && choices == vec!["a".to_string(), "b".to_string()]
        ));
    }

    #[tokio::test]
    async fn advance_tool_queue_emits_tool_pending_for_non_clarify_tool() {
        let sandbox = test_sandbox();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool::new(SandboxContext::new(sandbox))));
        let registry = Arc::new(registry);
        let call_id = Uuid::new_v4();
        let mut pending_tools = VecDeque::from([PendingTool {
            call: registry
                .create_call(
                    "write",
                    call_id,
                    serde_json::json!({
                        "path": "Cargo.toml",
                        "content": "ignored",
                    }),
                )
                .expect("write tool registered"),
        }]);
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::new();

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            false,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            &mut clarify_started_at,
            &mut pending_steers,
        )
        .await;

        assert_eq!(mode, AgentMode::ToolPending);
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ToolPending { id, name, .. }) if id == call_id && name == "write"
        ));
    }

    #[tokio::test]
    async fn advance_tool_queue_injects_pending_steers_before_starting_next_request() {
        // Regression test: queued steers (pending user messages typed while
        // tools were running) must reach the model on the very next request
        // once all tool results for this round are in context, rather than
        // waiting for a full tool-free turn to complete via StreamDone.
        let registry = Arc::new(ToolRegistry::new());
        let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let (agent_tx, _agent_rx) = mpsc::channel(1);
        let mut context = Context::new();
        context.push_tool_result(
            Uuid::new_v4(),
            "write".to_string(),
            "ok".to_string(),
            None,
            None,
        );
        let mut mode = AgentMode::ToolPending;
        let mut current_cancel = None;
        let mut current_tool_cancel = None;
        let mut iterations = 1usize;
        let mut clarify_started_at = None;
        let mut pending_steers = VecDeque::from(["steered while tool ran".to_string()]);

        advance_tool_queue(
            &mut pending_tools,
            &mut context,
            &mut mode,
            &mut current_cancel,
            &mut current_tool_cancel,
            &mut iterations,
            true,
            test_engine(),
            registry,
            ui_tx,
            agent_tx,
            &mut clarify_started_at,
            &mut pending_steers,
        )
        .await;

        assert_eq!(mode, AgentMode::Streaming);
        assert!(pending_steers.is_empty(), "steer must be drained");
        assert_eq!(
            context.messages().last(),
            Some(&Message::User {
                content: "steered while tool ran".to_string(),
                kind: UserMessageKind::Explicit,
            })
        );
        assert!(matches!(ui_rx.recv().await, Some(UiUpdate::SteerApplied)));
    }
}
