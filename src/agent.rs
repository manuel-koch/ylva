use std::collections::VecDeque;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::llm::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Idle,
    Streaming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    Prompt(String),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    StreamDone { partial: String },
    StreamCancelled { partial: String },
    StreamError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiUpdate {
    AppendText(String),
    StreamStarted,
    StreamDone,
    StreamCancelled,
    Error(String),
}

#[derive(Debug)]
pub enum StreamEvent {
    Text(String),
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

#[derive(Debug, Default)]
pub struct Context {
    messages: Vec<Message>,
}

impl Context {
    pub fn new() -> Self {
        Context {
            messages: vec![Message::System {
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            }],
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

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// Non-blocking agent loop (blueprint §4B).
///
/// State machine: `Idle` receives a `Prompt` and starts a stream; further
/// `Prompt`s while `Streaming` are queued as **steers** and injected at the
/// next natural point (`StreamDone` in Phase 1) — they never abort a live
/// stream. Only `Stop` aborts, via the current request's `CancellationToken`.
///
/// The outer loop task is the sole writer of `Context` (`single-writer`
/// rule): no `Arc<Mutex<Context>>` anywhere. `run()` takes the loop's channel
/// receivers out of `self`; the caller keeps `self` (and thus `cmd_tx`) alive
/// and shuts the loop down by **dropping the `AgentLoop`**, after which the
/// task returns the final committed `Context` from `handle.await`.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    cmd_rx: Option<mpsc::Receiver<AgentCommand>>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
    agent_rx: Option<mpsc::Receiver<AgentEvent>>,
}

impl AgentLoop {
    pub fn new(provider: Arc<dyn Provider>) -> (Self, mpsc::Receiver<UiUpdate>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(10);
        let (ui_tx, ui_rx) = mpsc::channel::<UiUpdate>(100);
        let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>(10);
        let agent = AgentLoop {
            provider,
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
            let mut pending_steers: VecDeque<String> = VecDeque::new();

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
                                    context.push_user(text);
                                    mode = AgentMode::Streaming;
                                    current_cancel = Some(start_request(
                                        &context,
                                        provider.clone(),
                                        ui_tx.clone(),
                                        agent_tx.clone(),
                                    ));
                                } else {
                                    // Cooperative steering: never cancel the live stream.
                                    pending_steers.push_back(text);
                                }
                            }
                            AgentCommand::Stop => {
                                // Interrupt: hard-abort the current round.
                                pending_steers.clear();
                                if let Some(cancel_token) = current_cancel.take() {
                                    cancel_token.cancel();
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Spawns the inner streaming task for one request, built from an immutable
/// snapshot of the conversation history. Returns the `CancellationToken` that
/// the outer loop uses to abort this request.
fn start_request(
    context: &Context,
    provider: Arc<dyn Provider>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<AgentEvent>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let messages = context.messages().to_vec();
    let inner_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut stream = match provider.chat(messages, inner_cancel).await {
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
        while let Some(event) = stream.recv().await {
            match event {
                StreamEvent::Text(text) => {
                    partial.push_str(&text);
                    let _ = ui_tx.send(UiUpdate::AppendText(text)).await;
                }
                StreamEvent::Done => {
                    let _ = ui_tx.send(UiUpdate::StreamDone).await;
                    let _ = agent_tx.send(AgentEvent::StreamDone { partial }).await;
                    break;
                }
                StreamEvent::Cancelled => {
                    let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
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
    fn session_begins_with_seeded_system_message() {
        let context = Context::new();
        assert_eq!(
            context.messages(),
            &[Message::System {
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            }]
        );
    }

    #[test]
    fn push_messages_appends_in_order() {
        let mut context = Context::new();
        context.push_user("first user turn".to_string());
        context.push_assistant("first response".to_string(), AssistantStatus::Complete);
        context.push_user("second user turn".to_string());
        assert_eq!(context.messages().len(), 4);
        assert_eq!(
            context.messages()[1],
            Message::User {
                content: "first user turn".to_string(),
            }
        );
        assert_eq!(
            context.messages()[2],
            Message::Assistant {
                content: "first response".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            }
        );
        assert_eq!(
            context.messages()[3],
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
            context.messages()[1],
            Message::Assistant {
                content: "partial".to_string(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            }
        );
    }

    #[test]
    fn messages_returns_immutable_read_back_slice() {
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let read_back = context.messages();
        assert_eq!(read_back.len(), 2);
        assert!(matches!(read_back[0], Message::System { .. }));
        assert_eq!(
            read_back[1],
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

        assert_eq!(AgentMode::Idle, AgentMode::Idle);
        assert_eq!(AgentMode::Streaming, AgentMode::Streaming);
    }
}
