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

//! Pure TUI state model for the reducer pattern.
//!
//! This module defines the types consumed by `presenter::update()`:
//! - [`AppState`] — the authoritative view-model.
//! - [`TuiMode`] — agent-side control flow state.
//! - [`Focus`] — keyboard/mouse routing.
//! - [`InputEvent`] / [`DomainEvent`] — the typed event surface of the Presenter.
//! - [`Effect`] — the typed I/O commands the Driver executes.
//! - [`AppLayout`] — screen-space geometry with hit-test helpers.
//!
//! **No `use ratatui::backend`**, no channel types, no `UiUpdate` imports.
//! Only [`ratatui::layout::Rect`] is allowed from ratatui here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use uuid::Uuid;

use crate::agent::{AgentMode, AssistantStatus, Message, UserMessageKind};
use crate::llm::engine::{EngineDisplayInfo, ProviderModelOption};
use crate::slash_command::{slash_command_suggestions, SlashCommandInfo};
use crate::store::ChildRunRecord;

// ─── Duration constant ───────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

/// Maximum gap between two clicks on the same chat bubble's title row for
/// the second click to count as a double-click (the drill-into-subagent
/// gesture) rather than a second, independent single click (collapse toggle).
pub(crate) const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(250);

/// Sentinel `entry_index` used in `thinking_title_rows` for the in-flight
/// (not yet flushed) thinking bubble rendered from `partial_thinking_text`.
/// Real entries are keyed by their `chat_entries` index (always `< usize::MAX`
/// in practice), so this value can never collide with a genuine index; it
/// lets click handling route to `AppState::toggle_partial_thinking_collapsed`
/// instead of `AppState::toggle_thinking_collapsed`.
pub const STREAMING_THINKING_ENTRY_INDEX: usize = usize::MAX;

// ─── ChatSelection ───────────────────────────────────────────────────────────

/// Screen-space selection state for the chat area. Line and column are
/// Normalised (start ≤ end) drag selection inside the clarify banner, in
/// absolute screen coordinates. Used only for rendering the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerDragSelection {
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
}

/// absolute line indices into `TuiState::chat_rendered_lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatSelection {
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl ChatSelection {
    /// Returns `(start_line, start_col, end_line, end_col)` in reading order.
    pub fn normalized(&self) -> (usize, usize, usize, usize) {
        if (self.anchor_line, self.anchor_col) <= (self.cursor_line, self.cursor_col) {
            (
                self.anchor_line,
                self.anchor_col,
                self.cursor_line,
                self.cursor_col,
            )
        } else {
            (
                self.cursor_line,
                self.cursor_col,
                self.anchor_line,
                self.anchor_col,
            )
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_line == self.cursor_line && self.anchor_col == self.cursor_col
    }
}

/// Extract `chars[start..end]` from a UTF-8 string, clamped to bounds.
fn char_range(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// The on-screen rect of the input box, written by `tui_widgets::draw` every frame and
/// read by the event loop for mouse click/drag cursor placement. Ephemeral
/// view state, reset to an empty rect between frames.
pub type InputArea = ratatui::layout::Rect;

/// State of a user message entry in the TUI chat history.
/// `ChatEntry` is ephemeral view state only — never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageState {
    /// Committed: injected into the model context.
    Sent,
    /// Pending steer: queued, not yet injected.
    Queued,
    /// Steer successfully applied: injected into the model context as a
    /// mid-stream correction — once injected, the LLM has seen it just like
    /// any other user message, and that's the only distinction that matters.
    Steered,
    /// Cancelled: stream was interrupted before injection; text retained in
    /// input_history so the user can recover via ↑.
    Cancelled,
}

/// Well-known result-section IDs used to identify [`ResultSection`] content in tool output.
///
/// The UI derives display titles from the combination of a tool name and one of these IDs.
pub mod result_section_ids {
    /// Standard tool output (used by all tools that return plain text results).
    pub const RESULT: &str = "result";
    /// The user's answer to a clarify question (used by the `clarify` tool).
    pub const USER_ANSWER: &str = "user_answer";
}

/// How the tool's result string is displayed in its completed bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultSection {
    /// Stable identifier for the section. See [`result_section_ids`] for known values.
    pub id: String,
    /// The result text.
    pub content: String,
}

impl ResultSection {
    /// Convenience constructor for a plain result with the standard [`result_section_ids::RESULT`] ID.
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            id: result_section_ids::RESULT.to_string(),
            content: content.into(),
        }
    }
}

/// State of a tool call bubble in the TUI chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallState {
    /// Awaiting user approval; arguments are shown so the user knows what the tool will do.
    Pending {
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Approved and running.
    Running {
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// Interim status key/value pairs patched in by `ToolProgress` (e.g.
        /// `spawn_subagent` reporting `status=spawned`/`queued` + `run_id`),
        /// shown alongside `title_arguments` while running. Deliberately
        /// **not** carried forward into `Done` (which has no equivalent
        /// field) — once the call truly finishes, `Done`'s own `summary`
        /// is the single source of truth for its final status, so a stale
        /// in-flight value here can never linger into the terminal bubble.
        progress_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Completed successfully.
    Done {
        arguments: serde_json::Value,
        result_section: ResultSection,
        content_sections: Vec<crate::agent::tools::Section>,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// Tool returned an error.
    Error {
        error: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    },
    /// Tool was killed (stream interrupted while tool was running).
    Killed,
    /// Tool was denied by the user; error message is the denial reason.
    Denied {
        arguments: serde_json::Value,
        error: String,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
}

/// Counts root-conversation subagent calls that have not reached a terminal
/// state. Finished calls are represented by `Done`/`Error`/`Killed` bubbles
/// and are deliberately excluded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveSubagentCounts {
    pub running: usize,
    pub queued: usize,
}

impl ActiveSubagentCounts {
    /// Count active `spawn_subagent` bubbles in a root transcript.
    ///
    /// A running bubble without interim status metadata is treated as running;
    /// this covers the short interval between creating the bubble and receiving
    /// the scheduler's first `ToolProgress` update.
    pub fn from_entries(entries: &[ChatEntry]) -> Self {
        let mut counts = Self::default();

        for entry in entries {
            let ChatEntry::ToolBubble {
                name,
                state: bubble_state,
                ..
            } = entry
            else {
                continue;
            };
            if name != crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME {
                continue;
            }

            let ToolCallState::Running {
                progress_arguments, ..
            } = bubble_state.as_ref()
            else {
                continue;
            };
            let status = progress_arguments
                .iter()
                .find_map(|(key, value)| (key == "status").then(|| value.as_str()))
                .flatten();
            if status == Some("queued") {
                counts.queued += 1;
            } else {
                counts.running += 1;
            }
        }

        counts
    }
}

/// One entry in the scrollable chat history area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    UserMessage {
        text: String,
        state: UserMessageState,
    },
    AssistantMessage {
        text: String,
        interrupted: bool,
    },
    /// Extended-thinking / reasoning output emitted before the assistant's
    /// visible reply. It is collapsible; it starts expanded
    /// (`collapsed: false`) as soon as thinking starts — how this default is
    /// configured is not yet defined — and flushing from the completed
    /// stream carries that collapse state through unchanged rather than
    /// resetting it, so it never auto-collapses when thinking ends; only an
    /// explicit user toggle changes it.
    ThinkingMessage {
        text: String,
        collapsed: bool,
        /// Wall-clock time between the first and last thinking fragment of
        /// this stream, shown in the collapsed summary line.
        duration: std::time::Duration,
    },
    /// A single bubble representing all lifecycle stages of one tool invocation.
    ToolBubble {
        id: uuid::Uuid,
        name: String,
        state: Box<ToolCallState>,
        /// Whether the bubble body (arguments, result) is hidden; only the title
        /// line is rendered when `true`. Toggled by clicking the title line.
        collapsed: bool,
    },
    ErrorLine {
        message: String,
    },
    /// Informational output produced by the TUI itself (slash-command
    /// results), never by the model. Only `/models`' switch confirmation is
    /// also committed to the [`crate::store::Store`]; the rest are transient
    /// and vanish with the session.
    Notice {
        text: String,
    },
}

/// A single in-flight turn's accumulated stream text, shared by the root
/// conversation (`AppState::turn_buffer`) and every child conversation
/// (`ChildConversation::turn_buffer`) — the single place that knows how to
/// buffer/segment a live token stream into ordered `ChatEntry::ThinkingMessage`/
/// `ChatEntry::AssistantMessage` bubbles, so root and child transcripts can
/// never drift apart in how they handle thinking-vs-response text.
#[derive(Debug, Clone, Default)]
pub struct TurnBuffer {
    /// In-flight assistant reply text, appended by `push_response_chunk`
    /// (root: `StreamChunk`; child: `ChildAgentToken { is_thinking: false, .. }`).
    pub partial_assistant_text: String,
    /// In-flight extended-thinking/reasoning text, accumulated separately
    /// from `partial_assistant_text` so it never contaminates the assistant
    /// reply. Appended by `push_thinking_chunk` (root: `StreamThinkingChunk`;
    /// child: `ChildAgentToken { is_thinking: true, .. }`). Flushed into a
    /// `ChatEntry::ThinkingMessage` ahead of the assistant bubble.
    pub partial_thinking_text: String,
    /// Set when the first fragment of a new `partial_thinking_text` segment
    /// arrives; cleared on flush. Used to compute the
    /// `ChatEntry::ThinkingMessage::duration` shown in the collapsed summary.
    pub thinking_started_at: Option<Instant>,
    /// Collapse state for the thinking bubble that will be produced when
    /// `partial_thinking_text` is flushed. Set to `false` (expanded)
    /// alongside `thinking_started_at` when a new thinking segment starts,
    /// then carried unchanged into the flushed `ChatEntry::ThinkingMessage`.
    pub partial_thinking_collapsed: bool,
}

impl TurnBuffer {
    /// Flush only the in-flight thinking buffer into a `ChatEntry::ThinkingMessage`
    /// appended to `messages`, leaving `partial_assistant_text` untouched.
    /// No-op when there is no pending thinking text.
    pub fn flush_thinking_message(&mut self, messages: &mut Vec<ChatEntry>) {
        if self.partial_thinking_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.partial_thinking_text);
        let collapsed = self.partial_thinking_collapsed;
        let duration = self
            .thinking_started_at
            .take()
            .map(|started_at| started_at.elapsed())
            .unwrap_or_default();
        messages.push(ChatEntry::ThinkingMessage {
            text,
            collapsed,
            duration,
        });
    }

    /// Flush only the in-flight assistant reply buffer into a
    /// `ChatEntry::AssistantMessage` appended to `messages`, leaving
    /// `partial_thinking_text` untouched. No-op when there is no pending
    /// assistant text, or it is whitespace-only (e.g. a lone `"\n\n"` chunk
    /// some models emit immediately before a tool call) — pushing that would
    /// render as a visually-empty bubble ahead of the tool call.
    pub fn flush_response_message(&mut self, messages: &mut Vec<ChatEntry>) {
        if self.partial_assistant_text.trim().is_empty() {
            self.partial_assistant_text.clear();
            return;
        }
        messages.push(ChatEntry::AssistantMessage {
            text: std::mem::take(&mut self.partial_assistant_text),
            interrupted: false,
        });
    }

    /// Commit the in-flight partial assistant text into a full
    /// `ChatEntry::AssistantMessage` appended to `messages`, clearing the
    /// partial buffer. If `partial_thinking_text` is non-empty, a
    /// `ChatEntry::ThinkingMessage` is pushed immediately before it — even
    /// when `partial_assistant_text` itself is empty (a thinking-only turn,
    /// e.g. the model reasons and then immediately calls a tool with no
    /// visible prose). A whitespace-only `partial_assistant_text` (see
    /// `flush_response_message`) is likewise dropped rather than pushed as an
    /// empty-looking bubble.
    pub fn flush_assistant_message(&mut self, messages: &mut Vec<ChatEntry>, interrupted: bool) {
        self.flush_thinking_message(messages);
        if !self.partial_assistant_text.trim().is_empty() {
            messages.push(ChatEntry::AssistantMessage {
                text: std::mem::take(&mut self.partial_assistant_text),
                interrupted,
            });
        } else {
            self.partial_assistant_text.clear();
        }
    }

    /// Append a fragment of assistant reply text, flushing any pending
    /// thinking segment into its own bubble first — mirrors a
    /// thinking-to-response mid-turn switch, so each segment renders as its
    /// own bubble in chronological order rather than merging.
    pub fn push_response_chunk(&mut self, text: &str, messages: &mut Vec<ChatEntry>) {
        self.flush_thinking_message(messages);
        self.partial_assistant_text.push_str(text);
    }

    /// Append a fragment of extended-thinking text, flushing any pending
    /// response segment first (mirrors `push_response_chunk`), and starting
    /// a fresh thinking-duration timer/expanded collapse state when this is
    /// the first fragment of a new segment.
    pub fn push_thinking_chunk(&mut self, text: &str, messages: &mut Vec<ChatEntry>) {
        self.flush_response_message(messages);
        if self.partial_thinking_text.is_empty() {
            self.thinking_started_at = Some(Instant::now());
            self.partial_thinking_collapsed = false;
        }
        self.partial_thinking_text.push_str(text);
    }
}

// ─── Drill-in conversation model (Phase 6.10) ────────────────────────────────

/// Identifies one child run's identity/metadata, keyed by `run_id` in
/// `AppState::child_conversations`. Populated on `DomainEvent::ChildAgentStarted`
/// and kept for the lifetime of the session (even after the child finishes)
/// so its transcript/header bubble can still be drilled into afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRunInfo {
    pub run_id: Uuid,
    /// The `id` of the real root `spawn_subagent` `ToolBubble` this run
    /// belongs to (the tool-call id) — the single source of truth
    /// correlating a specific bubble to a specific child conversation, used
    /// by both `AppState::header_bubble_id` (view → bubble) and the
    /// double-click handler (bubble → view), so multiple concurrent/queued
    /// `spawn_subagent` bubbles are never confused with one another.
    pub bubble_id: Uuid,
    pub agent_type: String,
    pub task: String,
    pub label: String,
}

/// One child run's own conversation transcript plus in-flight `TurnBuffer`,
/// keyed by `run_id` in `AppState::child_conversations`. Kept indefinitely
/// once created (mirrors `AppState::chat_entries`, which is also
/// session-lifetime-unbounded) so a finished child's transcript remains
/// drillable for as long as its root `spawn_subagent` bubble is visible.
#[derive(Debug, Clone)]
pub struct ChildConversation {
    pub info: ChildRunInfo,
    pub messages: Vec<ChatEntry>,
    pub turn_buffer: TurnBuffer,
    pub needs_lazy_load: bool,
}

impl ChildConversation {
    pub(crate) fn new(info: ChildRunInfo) -> Self {
        ChildConversation {
            info,
            messages: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            needs_lazy_load: false,
        }
    }

    pub(crate) fn new_replay(info: ChildRunInfo) -> Self {
        ChildConversation {
            info,
            messages: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            needs_lazy_load: true,
        }
    }
}

/// Project persisted domain messages into the same terminal-free chat model
/// used by live presenter updates.
pub fn project_messages(messages: &[Message]) -> Vec<ChatEntry> {
    project_messages_with_child_runs(messages, &[])
}

fn project_messages_with_child_runs(
    messages: &[Message],
    child_runs: &[ChildRunRecord],
) -> Vec<ChatEntry> {
    let tool_calls = messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .map(|call| {
            (
                call.id,
                (
                    call.name.clone(),
                    call.arguments.clone(),
                    call.title_arguments(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut entries = Vec::new();

    for message in messages {
        match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } => entries.push(ChatEntry::UserMessage {
                text: content.clone(),
                state: UserMessageState::Sent,
            }),
            Message::User {
                kind: UserMessageKind::ImplicitToolNote,
                ..
            }
            | Message::System { .. } => {}
            Message::Assistant {
                content,
                status,
                tool_calls,
            } => {
                if !content.trim().is_empty() {
                    entries.push(ChatEntry::AssistantMessage {
                        text: content.clone(),
                        interrupted: matches!(status, AssistantStatus::Interrupted),
                    });
                }
                for call in tool_calls {
                    entries.push(ChatEntry::ToolBubble {
                        id: call.id,
                        name: call.name.clone(),
                        state: Box::new(ToolCallState::Running {
                            arguments: call.arguments.clone(),
                            content_sections: vec![],
                            title_arguments: call.title_arguments(),
                            progress_arguments: vec![],
                        }),
                        collapsed: call.name != crate::agent::tools::clarify::ClarifyTool::NAME,
                    });
                }
            }
            Message::ToolResult {
                value:
                    crate::agent::ToolResult {
                        id,
                        name,
                        result,
                        result_section_id,
                        summary,
                        content_sections,
                        duration,
                        ..
                    },
            } => {
                let (arguments, title_arguments) = tool_calls
                    .get(id)
                    .map(|(_, arguments, title_arguments)| {
                        (arguments.clone(), title_arguments.clone())
                    })
                    .unwrap_or((serde_json::Value::Null, vec![]));
                replace_or_append_tool_bubble(
                    &mut entries,
                    *id,
                    name,
                    ToolCallState::Done {
                        arguments,
                        result_section: ResultSection {
                            id: result_section_id
                                .as_deref()
                                .unwrap_or(result_section_ids::RESULT)
                                .to_string(),
                            content: result.clone(),
                        },
                        content_sections: content_sections.clone(),
                        summary: summary.clone(),
                        duration: *duration,
                        title_arguments,
                    },
                );
            }
            Message::ToolError {
                id,
                name,
                error,
                duration,
                content_sections,
            } => {
                let (arguments, title_arguments) = tool_calls
                    .get(id)
                    .map(|(_, arguments, title_arguments)| {
                        (arguments.clone(), title_arguments.clone())
                    })
                    .unwrap_or((serde_json::Value::Null, vec![]));
                replace_or_append_tool_bubble(
                    &mut entries,
                    *id,
                    name,
                    ToolCallState::Error {
                        error: error.clone(),
                        title_arguments,
                        duration: *duration,
                        content_sections: content_sections.clone(),
                    },
                );
                let _ = arguments;
            }
        }
    }

    for child_run in child_runs {
        if child_run.call_id.is_nil() {
            continue;
        }
        let (name, arguments, title_arguments) = tool_calls
            .get(&child_run.call_id)
            .cloned()
            .unwrap_or_else(|| {
                (
                    crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                    serde_json::Value::Null,
                    vec![],
                )
            });
        let mut preserved_content_sections = entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ChatEntry::ToolBubble { id, state, .. } if *id == child_run.call_id => {
                    match state.as_ref() {
                        ToolCallState::Pending {
                            content_sections, ..
                        }
                        | ToolCallState::Running {
                            content_sections, ..
                        }
                        | ToolCallState::Done {
                            content_sections, ..
                        }
                        | ToolCallState::Error {
                            content_sections, ..
                        }
                        | ToolCallState::Denied {
                            content_sections, ..
                        } => Some(content_sections.clone()),
                        ToolCallState::Killed => None,
                    }
                }
                _ => None,
            })
            .unwrap_or_default();
        let child_state = match (&child_run.status, &child_run.completed_at) {
            (Some(status), Some(_)) => {
                let finish_result = crate::agent::child_agent_manager::FinishResult {
                    run_id: child_run.run_id,
                    call_id: child_run.call_id,
                    status: status.clone(),
                    result: if status == "failure" {
                        None
                    } else {
                        child_run.result.clone()
                    },
                    error: if status == "failure" {
                        child_run.result.clone()
                    } else {
                        None
                    },
                    sources: child_run.sources.clone(),
                };
                preserved_content_sections.extend(finish_result.content_sections());
                ToolCallState::Done {
                    arguments,
                    result_section: ResultSection::plain(String::new()),
                    content_sections: preserved_content_sections,
                    summary: vec![("status".to_string(), status.clone())],
                    duration: child_run.duration.unwrap_or_default(),
                    title_arguments,
                }
            }
            _ => ToolCallState::Error {
                error: "Subagent did not finish before the session ended.".to_string(),
                title_arguments,
                duration: duration_since_timestamp(&child_run.created_at),
                content_sections: vec![],
            },
        };
        replace_or_append_tool_bubble(&mut entries, child_run.call_id, &name, child_state);
    }

    entries
}

fn replace_or_append_tool_bubble(
    entries: &mut Vec<ChatEntry>,
    id: Uuid,
    name: &str,
    state: ToolCallState,
) {
    if let Some(entry) = entries.iter_mut().rev().find(
        |entry| matches!(entry, ChatEntry::ToolBubble { id: entry_id, .. } if *entry_id == id),
    ) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = state;
        }
        return;
    }
    entries.push(ChatEntry::ToolBubble {
        id,
        name: name.to_string(),
        state: Box::new(state),
        collapsed: true,
    });
}

fn duration_since_timestamp(timestamp: &str) -> Duration {
    let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return Duration::ZERO;
    };
    chrono::Utc::now()
        .signed_duration_since(started_at)
        .to_std()
        .unwrap_or_default()
}

/// Which conversation transcript is currently rendered in the main chat area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationView {
    /// The root/parent conversation (`AppState::chat_entries`).
    Root,
    /// The currently-tracked child's conversation (`AppState::child_conversations`).
    Child {
        run_id: Uuid,
        agent_type: String,
        task: String,
        label: String,
    },
}

/// Direction for [`AppState::navigate_input_message_history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Up,
    Down,
}

// ─── TuiMode ─────────────────────────────────────────────────────────────────

/// TUI-local control-flow state that mirrors the subset of [`crate::agent::AgentMode`]
/// the Presenter needs. The Driver converts `UiUpdate` events into
/// [`DomainEvent`]s that drive transitions here; the Presenter never imports
/// from `agent.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiMode {
    /// No agent activity; ready to accept user input.
    Idle,
    /// Prompt sent to the provider; waiting for `StreamStarted`.
    Pending,
    /// Provider is streaming a response.
    Streaming,
    /// Waiting for the user to answer a `clarify` tool question.
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
    },
    /// `AgentCommand::Stop` has been sent; waiting for `StreamCancelled` or
    /// the agent to become idle.
    Aborting,
    /// A first confirm-key press (Ctrl+C or Esc) has been received; waiting
    /// for a second press of the *same* key within the confirm window to
    /// actually quit/stop, or for the window to expire.
    DoublePressAbort {
        pressed_at: Instant,
        /// The mode to restore when the confirm window expires.
        return_to: Box<TuiMode>,
        /// Which key armed this confirm window. Only a second press of the
        /// *same* key confirms; a press of the other key starts its own
        /// fresh confirm window instead (so e.g. Esc-then-Ctrl+C never
        /// quits, and Ctrl+C-then-Esc never confirms Ctrl+C's quit).
        armed_by: ConfirmKey,
    },
}

impl TuiMode {
    /// Short, stable variant tag for logging state transitions — omits the
    /// `ClarifyPending`/`DoublePressAbort` payloads (question text, timers).
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            TuiMode::Idle => "Idle",
            TuiMode::Pending => "Pending",
            TuiMode::Streaming => "Streaming",
            TuiMode::ClarifyPending { .. } => "ClarifyPending",
            TuiMode::Aborting => "Aborting",
            TuiMode::DoublePressAbort { .. } => "DoublePressAbort",
        }
    }
}

/// The key that armed a pending [`TuiMode::DoublePressAbort`] confirm window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKey {
    CtrlC,
    Escape,
}

// ─── Focus ────────────────────────────────────────────────────────────────────

/// Keyboard and mouse routing target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// Normal typing and history navigation.
    MainInput,
    /// Modal help overlay; captures all keyboard and mouse input.
    HelpPopup,
    /// Tool-approval banner; captures keyboard only (non-modal).
    /// Tool arguments are **not** stored here — they live solely in the
    /// corresponding `ToolBubble` chat entry.
    ToolApproval { tool_id: Uuid, tool_name: String },
}

impl Focus {
    /// Returns `true` only for modal focus variants (currently only
    /// [`Focus::HelpPopup`]).
    pub fn is_modal(&self) -> bool {
        matches!(self, Focus::HelpPopup)
    }
}

// ─── InputEvent ───────────────────────────────────────────────────────────────

/// Typed user-input event surface for the Presenter. The Driver converts
/// [`crate::events::AppAction`] into `InputEvent` variants so the Presenter
/// never imports `AppAction` or raw crossterm types for routing decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// F1 pressed — toggle help popup.
    ShowHelp,
    /// OS SIGINT (external `kill -INT`): interrupt immediately, no confirm.
    SigInt,
    /// Ctrl+C terminal key event; follows double-press confirm logic.
    CtrlC,
    /// Esc key; interpreted based on current [`Focus`] and [`TuiMode`].
    Escape,
    /// Any other key event forwarded verbatim.
    KeyInput(KeyEvent),
    /// Bracketed paste text.
    Paste(String),
}

// ─── DomainEvent ──────────────────────────────────────────────────────────────

/// Agent-domain events the Presenter reacts to. The Driver converts each
/// [`crate::agent::UiUpdate`] variant into the matching `DomainEvent` before
/// calling `presenter::update()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    /// Provider started streaming (first token / acknowledgement).
    StreamStarted,
    /// A text chunk arrived from the provider.
    StreamChunk(String),
    /// The stream ended cleanly.
    StreamDone,
    /// The stream was cancelled (e.g. after `AgentCommand::Stop`).
    StreamCancelled,
    /// The agent transitioned to idle from any path (error, cancel, done).
    /// Resets `TuiMode` to [`TuiMode::Idle`] and `Focus` to
    /// [`Focus::MainInput`] if needed.
    AgentIdle,
    /// A tool call is waiting for user approval.
    ToolPending {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// `None` for the root's own pending call; `Some(run_id)` when it
        /// belongs to the currently-tracked child (see `UiUpdate::ToolPending`).
        /// Drives which transcript (`chat_entries` vs `child_messages`) the
        /// resulting bubble is created in and whether the drill-in view
        /// auto-switches to `ConversationView::Child`.
        run_id: Option<Uuid>,
    },
    /// The model is asking the user to clarify something.
    ClarifyPending {
        id: Uuid,
        question: String,
        context: Option<String>,
        choices: Option<Vec<String>>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call started executing (approved).
    ToolStart {
        id: Uuid,
        name: String,
        arguments: serde_json::Value,
        content_sections: Vec<crate::agent::tools::Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A still-running tool call reports an interim status update; the
    /// bubble stays `Running` (spinner keeps going) — only its
    /// `title_arguments` are patched. See `UiUpdate::ToolProgress`.
    ToolProgress {
        id: Uuid,
        title_arguments: Vec<(String, serde_json::Value)>,
    },
    /// A tool call completed successfully.
    ToolDone {
        id: Uuid,
        name: String,
        result_section: ResultSection,
        summary: Vec<(String, String)>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call was killed (e.g. via Stop).
    ToolKilled {
        id: Uuid,
        /// See `DomainEvent::ToolPending::run_id`.
        run_id: Option<Uuid>,
    },
    /// A tool call was denied by the user.
    ToolDenied { id: Uuid },
    /// An error occurred in the agent — in its own loop when `run_id` is
    /// `None`, or in the child run identified by `run_id` (e.g. that
    /// child's own max-tool-use-iterations guard tripped). See
    /// `DomainEvent::ToolPending::run_id`.
    Error {
        message: String,
        run_id: Option<Uuid>,
    },
    /// The double-press confirm window expired; revert to `return_to` mode.
    ConfirmExpired,
    /// A steer was queued while the agent was busy. See
    /// `UiUpdate::SteerQueued::run_id`.
    SteerQueued { run_id: Option<Uuid> },
    /// All queued steers have been injected into the LLM context. See
    /// `UiUpdate::SteerQueued::run_id`.
    SteerApplied { run_id: Option<Uuid> },
    /// A fragment of extended-thinking / reasoning-token output arrived.
    StreamThinkingChunk(String),
    /// A child agent actually started running. See `UiUpdate::ChildAgentStarted`.
    ChildAgentStarted {
        run_id: Uuid,
        /// The `spawn_subagent` tool-call id (== the root `ToolBubble`'s
        /// `id`) this run belongs to. See `ChildRunInfo::bubble_id`.
        call_id: Uuid,
        agent_type: String,
        task: String,
        label: String,
    },
    /// The provider-connection/model identity of a constructed child engine.
    ChildAgentEngineConfigured {
        run_id: Uuid,
        engine: EngineDisplayInfo,
    },
    /// A fragment of `run_id`'s own text/thinking output.
    /// See `UiUpdate::ChildAgentToken`.
    ChildAgentToken {
        run_id: Uuid,
        text: String,
        is_thinking: bool,
    },
    /// The child agent run identified by `run_id` has finished.
    /// See `UiUpdate::ChildAgentDone`.
    ChildAgentDone {
        run_id: Uuid,
        status: String,
        result: Option<String>,
    },
    /// `DOUBLE_CLICK_WINDOW` elapsed after a single click on a bubble's
    /// title row without a confirming second click arriving — applies the
    /// deferred collapse/expand toggle that click was waiting to commit
    /// (see `AppState::last_bubble_click`). A no-op if a second click (or a
    /// click on a different bubble) already resolved/cleared it first.
    BubbleClickWindowExpired,
}

impl DomainEvent {
    /// Short, stable variant tag for logging state transitions — deliberately
    /// omits payloads (chunk text, tool arguments/content) so per-event
    /// logging stays cheap and never leaks tool output into the log.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            DomainEvent::StreamStarted => "StreamStarted",
            DomainEvent::StreamChunk(_) => "StreamChunk",
            DomainEvent::StreamDone => "StreamDone",
            DomainEvent::StreamCancelled => "StreamCancelled",
            DomainEvent::AgentIdle => "AgentIdle",
            DomainEvent::ToolPending { .. } => "ToolPending",
            DomainEvent::ClarifyPending { .. } => "ClarifyPending",
            DomainEvent::ToolStart { .. } => "ToolStart",
            DomainEvent::ToolProgress { .. } => "ToolProgress",
            DomainEvent::ToolDone { .. } => "ToolDone",
            DomainEvent::ToolKilled { .. } => "ToolKilled",
            DomainEvent::ToolDenied { .. } => "ToolDenied",
            DomainEvent::Error { .. } => "Error",
            DomainEvent::ConfirmExpired => "ConfirmExpired",
            DomainEvent::SteerQueued { .. } => "SteerQueued",
            DomainEvent::SteerApplied { .. } => "SteerApplied",
            DomainEvent::StreamThinkingChunk(_) => "StreamThinkingChunk",
            DomainEvent::ChildAgentStarted { .. } => "ChildAgentStarted",
            DomainEvent::ChildAgentEngineConfigured { .. } => "ChildAgentEngineConfigured",
            DomainEvent::ChildAgentToken { .. } => "ChildAgentToken",
            DomainEvent::ChildAgentDone { .. } => "ChildAgentDone",
            DomainEvent::BubbleClickWindowExpired => "BubbleClickWindowExpired",
        }
    }

    /// The tool-call/clarify `id` this event correlates to, if any — the one
    /// piece of payload worth logging alongside `kind()`, since it is what
    /// lets a `ToolStart`/`ToolProgress`/`ToolDone` (or `ClarifyPending`)
    /// sequence for the *same* call be followed across log lines.
    pub(crate) fn correlation_id(&self) -> Option<Uuid> {
        match self {
            DomainEvent::ToolPending { id, .. }
            | DomainEvent::ClarifyPending { id, .. }
            | DomainEvent::ToolStart { id, .. }
            | DomainEvent::ToolProgress { id, .. }
            | DomainEvent::ToolDone { id, .. }
            | DomainEvent::ToolKilled { id, .. }
            | DomainEvent::ToolDenied { id } => Some(*id),
            _ => None,
        }
    }
}

// ─── slash-command overlay ───────────────────────────────────────────────────

/// One selectable entry of the `/models` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// Named provider connection and model passed to
    /// `Engine::switch_model_option`.
    pub selection: ProviderModelOption,
    /// Human-readable line shown in the picker.
    pub label: String,
    /// Whether this choice is the model currently serving the conversation.
    pub is_active: bool,
    /// `Some(engine_name)` when this entry is a shortcut to a configured
    /// `[engines.<name>]` table (or its implicit one-per-connection
    /// equivalent), rendered ahead of the raw provider/model catalog and
    /// separated from it by a divider. `None` for a plain provider/model
    /// entry from the catalog itself.
    pub engine_name: Option<String>,
}

impl ModelChoice {
    /// Refresh the row text after changing its selected reasoning effort.
    pub fn refresh_label(&mut self) {
        self.label = match &self.engine_name {
            Some(engine_name) => engine_choice_label(engine_name, &self.selection, self.is_active),
            None => model_choice_label(&self.selection, self.is_active),
        };
    }
}

/// Render one `/models` picker row for a plain provider/model catalog entry.
pub(crate) fn model_choice_label(option: &ProviderModelOption, is_active: bool) -> String {
    format_model_choice_label(
        format!("{}/{}", option.provider_connection, option.model.id),
        option,
        is_active,
    )
}

/// Render one `/models` picker row for a named-engine shortcut, prefixed
/// with the engine name so it reads as a shortcut to the provider/model it
/// resolves to.
pub(crate) fn engine_choice_label(
    engine_name: &str,
    option: &ProviderModelOption,
    is_active: bool,
) -> String {
    format_model_choice_label(
        format!(
            "{engine_name} → {}/{}",
            option.provider_connection, option.model.id
        ),
        option,
        is_active,
    )
}

fn format_model_choice_label(
    prefix: String,
    option: &ProviderModelOption,
    is_active: bool,
) -> String {
    let mut label = prefix;
    if let Some(context_length) = option.model.context_length {
        label.push_str(&format!(
            "  context: {}",
            format_context_length(context_length)
        ));
    }
    match option.model.reasoning_efforts.as_deref() {
        None => label.push_str("  reasoning: unavailable"),
        Some([]) => label.push_str("  reasoning: default"),
        Some(_) => label.push_str(&format!(
            "  reasoning: {} ⚙",
            option.reasoning_effort.as_deref().unwrap_or("default")
        )),
    }
    if is_active {
        label.push_str("  — active");
    }
    label
}

fn format_context_length(context_length: usize) -> String {
    const KILOBYTE: usize = 1024;
    const MEGABYTE: usize = KILOBYTE * 1024;

    if context_length >= MEGABYTE {
        format!("{}mb", context_length / MEGABYTE)
    } else if context_length >= KILOBYTE {
        format!("{}kb", context_length / KILOBYTE)
    } else {
        format!("{context_length} tokens")
    }
}

/// Overlay rendered above the input while a slash command is being composed
/// or resolved (blueprint §6).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SlashOverlay {
    /// No slash command is in play.
    #[default]
    Hidden,
    /// The draft looks like a partially typed command; list what it could be.
    Suggestions {
        choices: Vec<SlashCommandInfo>,
        selected: usize,
    },
    /// `/models` was submitted and the provider's list is being fetched.
    LoadingModels,
    /// `/models` returned; the user picks the model to switch to. `choices`
    /// always holds the full fetched catalog — narrowing by `filter` only
    /// changes which indices are *displayed/selectable*, so an in-progress
    /// reasoning-effort edit on a choice survives the user typing more of
    /// the filter afterwards.
    ModelPicker {
        choices: Vec<ModelChoice>,
        filter: String,
        selected: usize,
    },
}

impl SlashOverlay {
    /// Whether the overlay currently captures navigation keys.
    pub fn is_visible(&self) -> bool {
        !matches!(self, SlashOverlay::Hidden)
    }
}

/// Indices into `choices` whose `provider_connection/model` identity
/// contains every whitespace-separated word of `filter`, case-insensitively
/// (in any order — e.g. `"deepseek v3"` matches `stub/deepseek-v3` and
/// `"v3 deepseek"` matches it too). An empty (or all-whitespace) filter
/// matches every choice.
pub(crate) fn matching_model_indices(choices: &[ModelChoice], filter: &str) -> Vec<usize> {
    let filter_words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            let identity = format!(
                "{}/{}",
                choice.selection.provider_connection, choice.selection.model.id
            )
            .to_lowercase();
            filter_words.iter().all(|word| identity.contains(word))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Render-row mapping for the `/models` picker. `Some(index)` identifies an
/// entry in the filtered choice list; `None` identifies the divider between
/// configured engines and provider models.
pub(crate) fn model_picker_row_entries(
    choices: &[ModelChoice],
    filter: &str,
) -> Vec<Option<usize>> {
    let visible_indices = matching_model_indices(choices, filter);
    let separator_before = visible_indices
        .iter()
        .position(|&index| choices[index].engine_name.is_none())
        .filter(|&boundary| boundary > 0);
    (0..visible_indices.len())
        .flat_map(|index| {
            let separator = (separator_before == Some(index)).then_some(None);
            separator.into_iter().chain(std::iter::once(Some(index)))
        })
        .collect()
}

/// Return the plain text represented by each selectable row in a slash
/// overlay. Model-picker divider rows are represented by an empty string
/// because they have no selectable text.
pub(crate) fn slash_overlay_text_rows(overlay: &SlashOverlay) -> Vec<String> {
    match overlay {
        SlashOverlay::Hidden => Vec::new(),
        SlashOverlay::Suggestions { choices, .. } => choices
            .iter()
            .map(|info| format!("{:<8} {}", info.name, info.description))
            .collect(),
        SlashOverlay::LoadingModels => vec!["Loading models…".to_string()],
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } => {
            let visible_indices = matching_model_indices(choices, filter);
            if visible_indices.is_empty() {
                vec!["Nothing matched your model filter.".to_string()]
            } else {
                model_picker_row_entries(choices, filter)
                    .into_iter()
                    .map(|entry| {
                        entry
                            .and_then(|visible_index| {
                                visible_indices
                                    .get(visible_index)
                                    .and_then(|choice_index| choices.get(*choice_index))
                            })
                            .map(|choice| choice.label.clone())
                            .unwrap_or_default()
                    })
                    .collect()
            }
        }
    }
}

/// The live model-picker filter typed after `/models` in `draft`. Falls
/// back to the whole trimmed draft if it no longer starts with `/models`
/// (e.g. the user backspaced into the command name itself).
fn extract_models_filter(draft: &str) -> String {
    let trimmed = draft.trim_start();
    trimmed
        .strip_prefix("/models")
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

// ─── Effect ───────────────────────────────────────────────────────────────────

/// I/O command emitted by the Presenter for the Driver to execute.
/// The Presenter is pure; all actual I/O (channel sends, textarea mutations)
/// happens in `execute_effects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send the current textarea contents as a prompt.
    SubmitPrompt { steer: bool },
    /// Replace the textarea with an empty, freshly-configured default.
    ClearTextarea,
    /// Snapshot the current textarea lines into
    /// `AppState::pending_approval_input_snapshot` then clear the textarea.
    SaveTextareaAndClear,
    /// Restore the textarea from the snapshot saved by `SaveTextareaAndClear`.
    RestoreTextarea,
    /// Replace the textarea content with the given text.
    SetInputText(String),
    /// Feed a key event into the textarea.
    TextareaKey(KeyEvent),
    /// Feed a pasted string (dedented) into the textarea.
    Paste(String),
    /// Send `AgentCommand::ApproveTool { id }` to the agent.
    ApproveTool { id: Uuid },
    /// Send `AgentCommand::DenyTool { id, reason }` to the agent.
    /// `reason` is the user-typed denial text; empty when no reason was given.
    DenyTool { id: Uuid, reason: String },
    /// Send `AgentCommand::AnswerClarify(answer)` to the agent.
    AnswerClarify(String),
    /// Send `AgentCommand::AbortClarify` to the agent (user cancelled without answering).
    AbortClarify,
    /// Send `AgentCommand::Stop` to the agent.
    StopAgent,
    /// Quit the TUI event loop.
    Quit,
    /// Load a history entry into the textarea (driver replaces textarea content).
    LoadHistory { direction: HistoryDirection },
    /// Exit history browsing mode (driver restores live draft).
    ExitBrowsing,
    /// Place the textarea cursor at the given terminal cell coordinates.
    PlaceCursorAtMousePos { column: u16, row: u16 },
    /// Start a textarea text selection at the current cursor position.
    StartInputSelection,
    /// Extend the textarea text selection to the given terminal cell.
    ExtendInputSelection { column: u16, row: u16 },
    /// Copy the given text to the OS clipboard.
    CopyToClipboard(String),
    /// Finalize and copy the current textarea text selection.
    FinalizeInputSelection,
    /// `/clear` — drop the transcript and start a new persisted session.
    ClearConversation,
    /// `/tools` — show the tools the running agent may call.
    ShowTools,
    /// Read a replay-discovered child transcript before entering its view.
    LoadChildTranscript { run_id: Uuid },
    /// `/models` — fetch the provider's model list for the picker,
    /// optionally narrowed by a typed model filter.
    ListModels { filter: Option<String> },
    /// A provider-connection/model was picked: activate it and confirm in the transcript.
    SwitchModel(ProviderModelOption),
}

// ─── AppLayout ────────────────────────────────────────────────────────────────

/// Screen-space geometry for the chat scroll area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatArea {
    pub rect: Rect,
}

impl ChatArea {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the text-input box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputAreaLayout {
    pub rect: Rect,
}

impl InputAreaLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the help popup overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelpPopupLayout {
    pub rect: Rect,
}

impl HelpPopupLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside the
    /// popup's rendered area (equivalent to `contains` but named `hit` to
    /// signal it is used for modal dismiss-on-outside-click logic).
    pub fn hit(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the slash-command overlay's selectable content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlashOverlayLayout {
    pub content_area: Rect,
    /// Absolute row in the overlay's content list corresponding to the first
    /// row currently rendered in `content_area`.
    pub first_visible_row: usize,
}

impl SlashOverlayLayout {
    /// Return the absolute content row under a terminal cell, if it is inside
    /// the rendered entry column.
    pub fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.content_area;
        if col < area.x || col >= area.x + area.width || row < area.y || row >= area.y + area.height
        {
            return None;
        }
        Some(self.first_visible_row + row.saturating_sub(area.y) as usize)
    }
}

/// Aggregated screen-space geometry written by the View (`render.rs`) each
/// frame and read by the Presenter for mouse hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppLayout {
    pub chat_area: ChatArea,
    pub input_area: InputAreaLayout,
    pub help_popup: HelpPopupLayout,
    pub slash_overlay: SlashOverlayLayout,
    /// Rect of the clarify banner. Zero-sized when no clarify is pending.
    pub clarify_banner: ratatui::layout::Rect,
    /// Rect of the child-view header bubble's own content column (excludes
    /// its scrollbar column). Zero-sized while viewing `Root`. Used to route
    /// mouse-wheel scroll events to the header bubble rather than the chat
    /// area when the pointer is hovering over it.
    pub header_bubble: ratatui::layout::Rect,
    /// Rect of the Approve button rendered in the tool-approval footer.
    /// Written by the Driver after each `tui_widgets::draw` call; zero-sized when
    /// no approval footer is visible.
    pub approve_button: Rect,
    /// Rect of the Deny button rendered in the tool-approval footer.
    /// Written by the Driver after each `tui_widgets::draw` call; zero-sized when
    /// no approval footer is visible.
    pub deny_button: Rect,
}

// ─── Per-area sub-structs ──────────────────────────────────────────────────────
//
// Groups related `AppState` fields by the user-facing area they belong to
// (help popup, drill-in child view, clarify banner, conversation list),
// mirroring the `AppLayout` precedent above. On-screen rects for these same
// areas stay in `AppLayout` (written once per frame by the Driver) rather
// than living here — these sub-structs are data-only. `TuiState`
// (`src/tui_render_state.rs`) defines its own, differently-shaped sub-structs
// with the same names: it has no separate layout type, so its versions carry
// their area's rect field(s) alongside the data. The two are intentionally
// distinct types, analogous per area rather than identical.

/// Help popup (`?` overlay) scroll state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelpPopupState {
    pub help_scroll_offset: usize,
    pub help_scroll_x: usize,
}

/// Drill-in child-view header bubble state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChildViewState {
    /// Current vertical scroll offset inside the child-view header bubble
    /// (lines from the top of its content), used only when the bubble's full
    /// content exceeds the header's clamped max height. Clamped to the valid
    /// range by `tui_widgets::draw` each frame; reset to 0 whenever the header bubble
    /// it applies to changes (see `presenter::domain_child_lifecycle`).
    pub header_bubble_scroll_offset: usize,
}

/// Clarify-tool banner scroll/drag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClarifyBannerState {
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,
    /// Terminal position where the left button was pressed inside a clarify
    /// choice row. Cleared on mouse-up. A mouse-up at the same position is a
    /// click (→ fill textarea); a mouse-up at a different position is a drag
    /// (→ no action, allows text selection).
    pub clarify_choice_mousedown: Option<(u16, u16)>,
    /// Updated on every `MouseDrag` event while `clarify_choice_mousedown` is
    /// set. Together with the down position this defines the highlighted row
    /// range in the banner. Cleared on mouse-up. Combined with
    /// `clarify_choice_mousedown` into a single `BannerDragSelection` by
    /// `render::project_for_draw` for `TuiState::clarify_banner_state`.
    pub clarify_banner_drag_end: Option<(u16, u16)>,
}

/// Root-or-child scrollable conversation-list state (Phase 6.10 drill-in
/// model: shared by both `active_view` cases, not duplicated per view).
#[derive(Debug, Clone)]
pub struct ConversationListState {
    pub scroll_offset: usize,
    /// `true` = auto-scroll to bottom on new content.
    pub scroll_locked: bool,
    /// Updated on every resize event.
    pub chat_area_height: u16,
    /// Plain text of every rendered line from the most recent draw call.
    pub chat_rendered_lines: Vec<String>,
    /// Number of leading chrome characters stripped per line (see
    /// `TuiState::chat_rendered_line_chrome_widths`).
    pub chat_rendered_line_chrome_widths: Vec<usize>,
    /// Active or committed chat selection.
    pub chat_selection: Option<ChatSelection>,
    /// `true` while the left mouse button is held down in the chat area.
    pub chat_drag_active: bool,
    /// Maps each rendered tool bubble to its absolute line-index range
    /// `(start_row, end_row, id)` (inclusive, before scroll offset).
    pub bubble_title_rows: Vec<(usize, usize, Uuid)>,
    /// Maps each rendered thinking bubble to its absolute line-index range
    /// `(start_row, end_row, chat_entries index)` (inclusive, before scroll
    /// offset). Mirrors `bubble_title_rows` but is index-keyed since
    /// `ChatEntry::ThinkingMessage` has no `Uuid`.
    pub thinking_title_rows: Vec<(usize, usize, usize)>,
}

impl Default for ConversationListState {
    /// `scroll_locked` defaults to `true` (auto-scroll to bottom) — every
    /// other field defaults to empty/zero.
    fn default() -> Self {
        ConversationListState {
            scroll_offset: 0,
            scroll_locked: true,
            chat_area_height: 0,
            chat_rendered_lines: Vec::new(),
            chat_rendered_line_chrome_widths: Vec::new(),
            chat_selection: None,
            chat_drag_active: false,
            bubble_title_rows: Vec::new(),
            thinking_title_rows: Vec::new(),
        }
    }
}

fn move_selection(selected: &mut usize, choices_length: usize, delta: isize) {
    if choices_length == 0 {
        return;
    }
    let last_index = choices_length - 1;
    let moved = (*selected as isize + delta).clamp(0, last_index as isize);
    *selected = moved as usize;
}

// ─── AppState ────────────────────────────────────────────────────────────────

/// The authoritative TUI view-model owned by the Driver.
///
/// This is the single source of truth: `presenter::update` mutates it in
/// response to [`DomainEvent`]s, and every frame `render::project_for_draw`
/// derives a fresh [`crate::tui_render_state::TuiState`] snapshot from it for
/// `tui_widgets::draw` to render. `TuiState` is never the source of truth and is not fed back into
/// `AppState` except for the handful of view-geometry fields `tui_widgets::draw`
/// itself writes (see `render::sync_from_draw`).
///
/// Fields that require `TextArea` to interpret (e.g. `mouse_drag`, used by
/// `cursor_for_click`) or that exist purely to pass rendering artifacts between
/// draw calls (e.g. `button_areas`) are kept in the Driver and excluded here.
pub struct AppState {
    pub mode: TuiMode,
    pub focus: Focus,
    pub layout: AppLayout,
    /// Provider/model identity for the root conversation. The Driver refreshes
    /// this from the live root engine before each frame so runtime model
    /// switches are reflected immediately.
    pub engine_display: EngineDisplayInfo,

    // ── chat view-model ─────────────────────────────────────────────────────
    pub chat_entries: Vec<ChatEntry>,
    /// Root conversation's in-flight stream-text accumulator. See
    /// `TurnBuffer`; every child conversation has its own instance
    /// (`ChildConversation::turn_buffer`).
    pub turn_buffer: TurnBuffer,
    /// Steers (user messages sent while the agent is busy) staged for live
    /// rendering as `Queued` bubbles, not yet committed to `chat_entries`.
    /// See `queue_steer_message` / `commit_queued_steer_messages`.
    pub queued_steer_texts: Vec<String>,

    // ── scroll state / chat selection / bubble hit-testing ─────────────────
    pub conversation: ConversationListState,

    // ── tool-approval ───────────────────────────────────────────────────────
    /// Textarea lines saved when a `ToolPending` approval sub-mode starts so
    /// the unsubmitted draft can be restored once the approval is resolved.
    /// `None` when no approval is in progress or the input was empty.
    pub pending_approval_input_snapshot: Option<Vec<String>>,

    // ── input history ───────────────────────────────────────────────────────
    /// All `UserMessage` texts, oldest → newest.
    pub input_history: Vec<String>,
    /// `None` = live draft; `Some(i)` = browsing history at index `i`.
    pub history_index: Option<usize>,
    /// Saved live draft while browsing history.
    pub draft: String,

    // ── steer indicator ─────────────────────────────────────────────────────
    /// `true` while ≥1 steer is queued.
    pub steer_indicator: bool,

    // ── transient UI ────────────────────────────────────────────────────────
    /// Floating notification, auto-dismissed after [`TOAST_DURATION`].
    pub toast: Option<(String, Instant)>,
    /// Incremented each tick while agent is busy; drives the footer spinner.
    pub spinner_frame: u8,

    // ── help popup ──────────────────────────────────────────────────────────
    pub help_popup: HelpPopupState,

    // ── clarify banner ───────────────────────────────────────────────────────
    pub clarify_banner_state: ClarifyBannerState,

    // ── child-view header bubble ────────────────────────────────────────────
    pub child_view: ChildViewState,

    // ── input field ─────────────────────────────────────────────────────────
    /// Mirrors ratatui-textarea's internal scroll-top so scroll indicators
    /// reflect the actual rendered viewport position.
    pub input_scroll_top: usize,

    // ── history navigation helper ────────────────────────────────────────────
    /// Exposes history navigation for use by the presenter.
    pub _history_direction: Option<HistoryDirection>,

    // ── abort-exit flag ──────────────────────────────────────────────────────
    /// `true` when `TuiMode::Aborting` was entered via Ctrl+C double-press,
    /// meaning the Driver should break the event loop once mode returns to
    /// Idle. `false` when Aborting was entered via Esc (soft cancel) — the
    /// TUI stays open so the user can continue chatting.
    pub aborting_exit: bool,

    // ── drill-in conversation model (Phase 6.10) ─────────────────────────────
    /// Which transcript (root or child) is currently rendered in the main
    /// chat area. Independent of `running_child`: the parent stays in
    /// `Root` view while a child streams in the background, per
    /// `docs/plans/phase6-terminal-superpowers.md` Phase 6.10.
    pub active_view: ConversationView,
    /// Every child run's own conversation transcript/partial-text
    /// accumulator, keyed by `run_id` — one entry per `spawn_subagent`
    /// child that has ever started, added on `ChildAgentStarted` and never
    /// removed (mirrors `chat_entries`, which is also session-lifetime
    /// unbounded), so any of them can still be drilled into later even
    /// while another child is running/queued or has since finished. Fixes
    /// the bug where a single shared transcript was silently overwritten
    /// whenever the next queued child started, corrupting whichever
    /// child's conversation the user was currently viewing.
    pub child_conversations: HashMap<Uuid, ChildConversation>,
    /// Provider/model identity for each constructed child engine, keyed by
    /// child run id. Child conversations can remain drillable after a run
    /// finishes, so these entries have the same session lifetime as their
    /// transcripts.
    pub child_engine_displays: HashMap<Uuid, EngineDisplayInfo>,
    /// Identifies the currently *running* child (at most one at a time —
    /// see `ChildAgentManager`), independent of whether `active_view` has
    /// switched to it yet, and independent of which (if any) child the user
    /// is currently viewing. `None` when no child is currently running
    /// (though earlier children may still have entries in
    /// `child_conversations`). Populated on `ChildAgentStarted`, cleared on
    /// `ChildAgentDone`.
    pub running_child: Option<ChildRunInfo>,
    /// `true` when `active_view` was switched to `Child` automatically (an
    /// interactive pause), rather than by explicit user navigation (click).
    /// Gates whether the view auto-returns to
    /// `pre_pause_view`/`Root` once that pause resolves or the child
    /// finishes.
    pub auto_switched_to_child: bool,
    /// Snapshot of `active_view` from immediately before an auto-switch
    /// (`note_child_interactive_pause`), so `resolve_possible_child_pause`
    /// can restore *that* view — which may be `Root` or a different,
    /// already-finished child's view the user had manually drilled into —
    /// instead of unconditionally landing back on `Root`. Populated only by
    /// `note_child_interactive_pause`, consumed (taken) by
    /// `resolve_possible_child_pause`.
    pub pre_pause_view: Option<ConversationView>,
    /// `(bubble_id, clicked_at)` of the most recent plain click on a chat
    /// bubble's title row, used to detect a second click on the same
    /// `spawn_subagent` bubble within `DOUBLE_CLICK_WINDOW` as the
    /// drill-in-conversation gesture — distinct from the single-click
    /// collapse/expand toggle every bubble (including spawn bubbles) always
    /// supports.
    pub last_bubble_click: Option<(uuid::Uuid, std::time::Instant)>,
    /// `(scroll_offset, scroll_locked)` of the root transcript, captured by
    /// `switch_to_child_view` right before it re-pins the shared scroll
    /// state to the bottom of the child's transcript. Restored by
    /// `switch_to_root_view` (Esc with an empty draft) as an immediate best-effort
    /// placeholder — the precise fix is `saved_root_scroll_bubble` below,
    /// resolved once root is rendered again. `None` before the first
    /// drill-in, or once restored.
    pub saved_root_scroll: Option<(usize, bool)>,
    /// `Uuid` of the `spawn_subagent` `ToolBubble` the user drilled in from,
    /// captured by `switch_to_child_view` (from the drilled-into run's
    /// `ChildRunInfo::bubble_id`). Consumed by
    /// `switch_to_root_view`, which moves it into `pending_scroll_to_bubble`
    /// so `render::sync_from_draw` can scroll to that bubble's *current* row
    /// — robust against the root conversation having grown while the child
    /// view was open, unlike a raw saved offset which would drift. `None`
    /// before the first drill-in, or once consumed.
    pub saved_root_scroll_bubble: Option<uuid::Uuid>,
    /// Armed by `switch_to_root_view` with the `Uuid` from
    /// `saved_root_scroll_bubble`; consumed by `render::sync_from_draw`
    /// immediately after the first root frame following the switch is drawn
    /// (so `AppState::bubble_title_rows` reflects root's, not the child's,
    /// bubbles), which scrolls to that bubble's row and clears this field.
    /// `None` when there is nothing pending.
    pub pending_scroll_to_bubble: Option<uuid::Uuid>,

    // ── slash commands (Phase 7) ─────────────────────────────────────────────
    /// Suggestion list / model picker driven by the slash-command line.
    pub slash_overlay: SlashOverlay,
    /// Screen position where a slash-overlay text drag started.
    pub slash_overlay_drag_start: Option<(u16, u16)>,
    /// Current screen position of a slash-overlay text drag.
    pub slash_overlay_drag_end: Option<(u16, u16)>,
    /// Names of the tools the running agent may call, captured once at
    /// startup from the resolved `ToolRegistry` so `/tools` needs no I/O.
    pub available_tool_names: Vec<String>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mode: TuiMode::Idle,
            focus: Focus::MainInput,
            layout: AppLayout::default(),
            engine_display: EngineDisplayInfo::default(),

            chat_entries: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            queued_steer_texts: Vec::new(),

            conversation: ConversationListState::default(),

            pending_approval_input_snapshot: None,

            input_history: Vec::new(),
            history_index: None,
            draft: String::new(),

            steer_indicator: false,

            toast: None,
            spinner_frame: 0,

            help_popup: HelpPopupState::default(),

            clarify_banner_state: ClarifyBannerState::default(),

            child_view: ChildViewState::default(),

            input_scroll_top: 0,

            _history_direction: None,

            slash_overlay: SlashOverlay::default(),
            slash_overlay_drag_start: None,
            slash_overlay_drag_end: None,
            available_tool_names: Vec::new(),

            aborting_exit: false,

            active_view: ConversationView::Root,
            child_conversations: HashMap::new(),
            child_engine_displays: HashMap::new(),
            running_child: None,
            auto_switched_to_child: false,
            pre_pause_view: None,
            last_bubble_click: None,
            saved_root_scroll: None,
            saved_root_scroll_bubble: None,
            pending_scroll_to_bubble: None,
        }
    }

    /// Replace the root view-model with a projection of a hydrated session.
    /// Persisted user messages also seed input history, while implicit
    /// harness notes remain model-only and are omitted by the projection.
    pub fn replay_messages(&mut self, messages: &[Message], child_runs: &[ChildRunRecord]) {
        self.chat_entries = project_messages_with_child_runs(messages, child_runs);
        self.input_history = messages
            .iter()
            .filter_map(|message| match message {
                Message::User {
                    content,
                    kind: UserMessageKind::Explicit,
                } => Some(content.clone()),
                _ => None,
            })
            .collect();
        self.history_index = None;
        self.child_conversations.clear();
        for child_run in child_runs {
            if child_run.call_id.is_nil() {
                continue;
            }
            self.child_conversations.insert(
                child_run.run_id,
                ChildConversation::new_replay(ChildRunInfo {
                    run_id: child_run.run_id,
                    bubble_id: child_run.call_id,
                    agent_type: child_run.agent_type.clone(),
                    task: child_run.task.clone(),
                    label: child_run.label.clone(),
                }),
            );
        }
        self.conversation.scroll_locked = true;
        self.conversation.scroll_offset = 0;
    }

    /// Replace a replay-discovered child's empty transcript after its first
    /// drill-in read. Repeated drill-ins keep the loaded transcript.
    pub fn load_child_messages(&mut self, run_id: Uuid, messages: &[Message]) {
        if let Some(conversation) = self.child_conversations.get_mut(&run_id) {
            if conversation.needs_lazy_load {
                conversation.messages = project_messages(messages);
                conversation.needs_lazy_load = false;
            }
        }
    }

    pub fn child_needs_lazy_load(&self, run_id: Uuid) -> bool {
        self.child_conversations
            .get(&run_id)
            .is_some_and(|conversation| conversation.needs_lazy_load)
    }

    /// Returns `true` if a toast is active and has not yet expired.
    pub fn has_active_toast(&self) -> bool {
        self.toast
            .as_ref()
            .map(|(_, start)| start.elapsed() < TOAST_DURATION)
            .unwrap_or(false)
    }

    /// Flush only the in-flight thinking buffer into a `ChatEntry::ThinkingMessage`,
    /// leaving `partial_assistant_text` untouched. Used when a stream switches
    /// from thinking to response text (or hits a tool call) mid-turn, so each
    /// segment renders as its own bubble in chronological order rather than
    /// merging every thinking fragment of the turn into a single bubble.
    /// No-op when there is no pending thinking text. Delegates to
    /// `TurnBuffer::flush_thinking_message` — see there for the shared
    /// root/child implementation.
    pub fn flush_partial_thinking_message(&mut self) {
        self.turn_buffer
            .flush_thinking_message(&mut self.chat_entries);
    }

    /// Flush only the in-flight assistant reply buffer into a
    /// `ChatEntry::AssistantMessage`, leaving `partial_thinking_text` untouched.
    /// Used when a stream switches from response text back to thinking mid-turn
    /// (mirrors `flush_partial_thinking_message`). Never relocates queued
    /// steers — those only need to move once, at the true end of the turn.
    /// No-op when there is no pending assistant text. Delegates to
    /// `TurnBuffer::flush_response_message`.
    pub fn flush_partial_response_message(&mut self) {
        self.turn_buffer
            .flush_response_message(&mut self.chat_entries);
    }

    /// Commit the in-flight partial assistant text into a full `AssistantMessage`
    /// entry and clear the partial buffer.
    ///
    /// If `partial_thinking_text` is non-empty, a `ChatEntry::ThinkingMessage` is
    /// pushed immediately before the assistant bubble — even when
    /// `partial_assistant_text` itself is empty (a thinking-only turn, e.g. the
    /// model reasons and then immediately calls a tool with no visible prose).
    /// Delegates to `TurnBuffer::flush_assistant_message`.
    pub fn flush_partial_assistant_message(&mut self, interrupted: bool) {
        self.turn_buffer
            .flush_assistant_message(&mut self.chat_entries, interrupted);
    }

    /// Commit every staged queued-steer text (see `queue_steer_message`) into
    /// `chat_entries` as a `UserMessage` with the given final `state`
    /// (`Steered` once applied to the model, `Cancelled` once the round is
    /// interrupted), in the order they were typed, then clear the staging
    /// list. Must be called after any other flush for the same event (e.g.
    /// `flush_partial_assistant_message`) so the committed bubbles land after
    /// everything that happened while they were staged, rather than at the
    /// stale position where the user happened to press `Enter`.
    pub fn commit_queued_steer_messages(&mut self, state: UserMessageState) {
        for text in std::mem::take(&mut self.queued_steer_texts) {
            self.chat_entries
                .push(ChatEntry::UserMessage { text, state });
        }
    }

    /// Find the most-recent `ToolBubble` with `id == target_id`, searching
    /// every child's own transcript (`child_conversations`) and the root
    /// (`chat_entries`) — a bubble's `id` (a `Uuid`) is unique regardless of
    /// which transcript it was created in (see `target_messages_mut`), so
    /// updaters (`apply_tool_*`) can stay agnostic of which one it lives in.
    pub fn find_tool_bubble_mut(&mut self, target_id: Uuid) -> Option<&mut ChatEntry> {
        for conversation in self.child_conversations.values_mut() {
            if let Some(pos) = conversation.messages.iter().rposition(
                |entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id),
            ) {
                return conversation.messages.get_mut(pos);
            }
        }
        self.chat_entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == target_id))
    }

    /// Target transcript for a *new* tool/clarify bubble tagged with
    /// `run_id`: that child's own transcript when `run_id` identifies any
    /// tracked child run (running, queued, or already finished — see
    /// `child_conversations`), the root transcript otherwise (`run_id` is
    /// `None`, or an unrecognised id).
    pub(crate) fn target_messages_mut(&mut self, run_id: Option<Uuid>) -> &mut Vec<ChatEntry> {
        if let Some(run_id) = run_id {
            if let Some(conversation) = self.child_conversations.get_mut(&run_id) {
                return &mut conversation.messages;
            }
        }
        &mut self.chat_entries
    }

    /// Called whenever a `ToolPending`/`ClarifyPending` bubble tagged with
    /// `run_id` is created. When it belongs to the currently-*running*
    /// child and the user isn't already looking at that exact child's view,
    /// auto-switches `active_view` to it (armed for auto-return via
    /// `auto_switched_to_child`), snapshotting whatever view was active
    /// beforehand into `pre_pause_view` so `resolve_possible_child_pause`
    /// can restore it — this may be `Root` *or* a different (e.g. already
    /// finished) child's drill-in view the user had manually navigated to;
    /// an approval belonging to a background child must never get stranded
    /// behind whatever transcript happens to be on screen. If the user is
    /// already viewing this exact running child (manually or via an earlier
    /// auto-switch), no further switch/re-arm happens — matches
    /// `docs/plans/phase6-terminal-superpowers.md` Phase 6.10 step 74:
    /// "If `active_view` is already `Child`: set `auto_switched_to_child` to
    /// `false` (user navigated manually; no auto-return)."
    pub(crate) fn note_child_interactive_pause(&mut self, run_id: Option<Uuid>) {
        let Some(child) = self.running_child.clone() else {
            return;
        };
        if run_id != Some(child.run_id) {
            return;
        }
        let already_viewing_this_child = matches!(
            &self.active_view,
            ConversationView::Child { run_id: r, .. } if *r == child.run_id
        );
        if already_viewing_this_child {
            self.auto_switched_to_child = false;
            return;
        }
        self.pre_pause_view = Some(self.active_view.clone());
        self.active_view = ConversationView::Child {
            run_id: child.run_id,
            agent_type: child.agent_type,
            task: child.task,
            label: child.label,
        };
        self.auto_switched_to_child = true;
    }

    /// Called whenever a tool/clarify bubble identified by `id` reaches a
    /// terminal state (or is denied). If `id` lives in the child transcript
    /// currently being viewed and the drill-in view was auto-switched there
    /// for it, auto-returns to whatever view was active before the
    /// auto-switch (`pre_pause_view` — `Root` or a different child's view),
    /// falling back to `Root` if none was recorded — matches Phase 6.10
    /// step 74: "When child interactive pause resolves: auto-return to
    /// `Root` only if `auto_switched_to_child` is `true`."
    pub(crate) fn resolve_possible_child_pause(&mut self, id: Uuid) {
        if !self.auto_switched_to_child {
            return;
        }
        let ConversationView::Child { run_id, .. } = &self.active_view else {
            return;
        };
        let run_id = *run_id;
        let was_child_pause = self.child_conversations.get(&run_id).is_some_and(|conversation| {
            conversation.messages.iter().any(
                |entry| matches!(entry, ChatEntry::ToolBubble { id: entry_id, .. } if *entry_id == id),
            )
        });
        if was_child_pause {
            self.active_view = self.pre_pause_view.take().unwrap_or(ConversationView::Root);
            self.auto_switched_to_child = false;
        }
    }

    /// Flush `run_id`'s own `TurnBuffer` (accumulated `ChildAgentToken`
    /// fragments, thinking and assistant text alike) into new
    /// `ChatEntry::ThinkingMessage`/`ChatEntry::AssistantMessage` entries in
    /// its own transcript. No-op when there is no pending text, or `run_id`
    /// is not a tracked child. Delegates to `TurnBuffer::flush_assistant_message`
    /// — the same shared logic the root conversation uses.
    pub fn flush_child_partial_text(&mut self, run_id: Uuid) {
        let Some(conversation) = self.child_conversations.get_mut(&run_id) else {
            return;
        };
        conversation
            .turn_buffer
            .flush_assistant_message(&mut conversation.messages, false);
    }

    /// Flushes whichever partial-text accumulator `run_id` targets before a
    /// new tool/clarify bubble is created — that child's own `TurnBuffer`
    /// when `run_id` identifies any tracked child run, the root's own
    /// `turn_buffer` otherwise. Without this, a child's in-flight reply would
    /// stay stuck in its buffer until `ChildAgentDone`, making a tool call it
    /// made appear to precede its own explanatory text in its transcript.
    pub(crate) fn flush_target_partial_text(&mut self, run_id: Option<Uuid>) {
        if let Some(run_id) = run_id {
            if self.child_conversations.contains_key(&run_id) {
                self.flush_child_partial_text(run_id);
                return;
            }
        }
        self.flush_partial_assistant_message(false);
    }

    /// Manual navigation into `run_id`'s child view (double-clicking a
    /// `spawn_subagent` bubble's title). Marks the switch as *not*
    /// auto-switched, so the view never auto-returns to `Root` on its own —
    /// the user must navigate back explicitly (Esc with an empty draft).
    /// Works for any tracked child (running, queued, or already finished —
    /// see `child_conversations`), not just the currently-running one, so
    /// each `spawn_subagent` bubble unambiguously drills into *its own*
    /// conversation rather than whichever child happens to be running now.
    /// No-op when `run_id` is not a tracked child.
    /// Registers a new tracked child run (adding a fresh, empty entry to
    /// `child_conversations`) and marks it as the currently *running*
    /// child. Used by `presenter::domain_child_lifecycle` on
    /// `DomainEvent::ChildAgentStarted`, and directly by tests that need to
    /// simulate a child run's existence without driving the full event
    /// pipeline.
    pub(crate) fn start_child_run(&mut self, info: ChildRunInfo) {
        self.running_child = Some(info.clone());
        self.child_conversations
            .insert(info.run_id, ChildConversation::new(info));
    }

    pub fn switch_to_child_view(&mut self, run_id: Uuid) {
        let Some(info) = self
            .child_conversations
            .get(&run_id)
            .map(|conversation| conversation.info.clone())
        else {
            return;
        };
        self.active_view = ConversationView::Child {
            run_id: info.run_id,
            agent_type: info.agent_type,
            task: info.task,
            label: info.label,
        };
        self.auto_switched_to_child = false;
        // Capture the root's current scroll position before it gets
        // overwritten below, so `switch_to_root_view` can restore it as a
        // best-effort fallback (see `saved_root_scroll_bubble` below for the
        // primary, content-shift-proof resolution).
        self.saved_root_scroll = Some((
            self.conversation.scroll_offset,
            self.conversation.scroll_locked,
        ));
        // Record the actual `spawn_subagent` bubble being drilled into, so
        // `switch_to_root_view` can scroll back to *its* current row rather
        // than a raw offset that would drift if the root conversation grows
        // while the user is away (new messages/tool bubbles shift row
        // indices below — sometimes even at — the saved offset).
        self.saved_root_scroll_bubble = Some(info.bubble_id);
        // Always start scrolled to the top of the header bubble on a fresh
        // navigation into the child view, regardless of where it was left
        // scrolled to on a previous visit.
        self.child_view.header_bubble_scroll_offset = 0;
        // The main chat area's `scroll_offset`/`scroll_locked` are shared
        // between the root and child views (single fields, not per-view) —
        // if the user had scrolled away from the bottom while viewing
        // `Root`, that stale, non-bottom position would otherwise carry
        // over into the child's transcript. Drilling in while the subagent
        // is running should always land on its latest activity, so
        // re-pin to the bottom on every navigation into the child view.
        self.scroll_to_bottom();
    }

    /// Finds the `id` of the real root `spawn_subagent` `ToolBubble`
    /// currently re-rendered as the child-view header (see
    /// `render::project_for_draw`'s `child_view_bubble`) — i.e. the bubble
    /// that belongs to whichever child `active_view` currently identifies.
    /// `None` while viewing `Root`, or if that child is somehow untracked.
    pub(crate) fn header_bubble_id(&self) -> Option<Uuid> {
        let ConversationView::Child { run_id, .. } = &self.active_view else {
            return None;
        };
        self.child_conversations
            .get(run_id)
            .map(|conversation| conversation.info.bubble_id)
    }

    /// Finds the `run_id` of the tracked child (running, queued, or already
    /// finished) whose root `spawn_subagent` `ToolBubble` is `bubble_id` —
    /// the inverse of `header_bubble_id`/`ChildRunInfo::bubble_id`. Used by
    /// the double-click handler to resolve *which* child a specific clicked
    /// bubble belongs to, rather than assuming "the" (singular) child, since
    /// multiple `spawn_subagent` bubbles (active + queued, or already
    /// finished) can coexist in `chat_entries`. `None` if `bubble_id` isn't
    /// (or is no longer) a tracked child's bubble.
    pub(crate) fn run_id_for_bubble(&self, bubble_id: Uuid) -> Option<Uuid> {
        self.child_conversations
            .iter()
            .find(|(_, conversation)| conversation.info.bubble_id == bubble_id)
            .map(|(run_id, _)| *run_id)
    }

    /// Toggle the collapsed state of the child-view header bubble (see
    /// `header_bubble_id`). Unlike `toggle_tool_bubble_collapsed`, this does
    /// not touch `scroll_offset`/`scroll_locked`/`bubble_title_rows` — those
    /// describe the currently-visible chat area (the child's own
    /// transcript), which the header bubble is not part of; any resulting
    /// change in the header's own content height is instead clamped by
    /// `tui_widgets::draw` against `header_bubble_scroll_offset` each frame. No-op
    /// while viewing `Root` (no header bubble is shown).
    pub fn toggle_header_bubble_collapsed(&mut self) {
        let Some(target_id) = self.header_bubble_id() else {
            return;
        };
        for entry in &mut self.chat_entries {
            if let ChatEntry::ToolBubble { id, collapsed, .. } = entry {
                if *id == target_id {
                    *collapsed = !*collapsed;
                    return;
                }
            }
        }
    }

    /// Manual "back to parent" navigation (Esc with an empty draft). Restores the root
    /// scroll position saved by `switch_to_child_view` (when present) as an
    /// immediate best-effort placeholder — avoids a blank/wrong-looking frame
    /// before the precise fix below lands — and arms `pending_scroll_to_bubble`
    /// with the drilled-into `spawn_subagent` bubble's id so `render.rs`'s
    /// `sync_from_draw` can scroll to that bubble's *current* row once root
    /// is rendered again this same frame. Resolving by bubble id (rather than
    /// trusting the raw saved offset) is what makes this correct even though
    /// the root conversation may have grown while the user was viewing the
    /// child — new messages/tool bubbles can shift row indices at or below
    /// the saved offset, landing it past the target bubble otherwise.
    /// `saved_root_scroll`/`saved_root_scroll_bubble` are `None` when the
    /// child view was entered via auto-switch (`note_child_interactive_pause`)
    /// rather than manual navigation; in that case the shared scroll fields
    /// were never touched, so there is nothing to restore.
    pub fn switch_to_root_view(&mut self) {
        self.active_view = ConversationView::Root;
        self.auto_switched_to_child = false;
        if let Some((scroll_offset, scroll_locked)) = self.saved_root_scroll.take() {
            self.conversation.scroll_offset = scroll_offset;
            self.conversation.scroll_locked = scroll_locked;
        }
        self.pending_scroll_to_bubble = self.saved_root_scroll_bubble.take();
    }

    /// Append a user message bubble and record the text in input history.
    /// Only for messages sent immediately (`UserMessageState::Sent`); a steer
    /// sent while the agent is busy must go through `queue_steer_message`
    /// instead, since it isn't safe to commit to `chat_entries` yet.
    /// Drop everything the user can see of the current conversation, keeping
    /// only what is not part of it: the input history (so ↑ still recalls
    /// earlier prompts) and the captured tool names.
    ///
    /// The in-memory `Context` and the session file are *not* touched here —
    /// the agent loop owns those and resets them on `AgentCommand::ClearContext`
    /// (blueprint §4C).
    pub fn clear_conversation(&mut self) {
        self.chat_entries.clear();
        self.turn_buffer = TurnBuffer::default();
        self.queued_steer_texts.clear();
        self.steer_indicator = false;
        self.child_conversations.clear();
        self.running_child = None;
        self.active_view = ConversationView::Root;
        self.child_view = ChildViewState::default();
        self.conversation.chat_selection = None;
        self.conversation.scroll_offset = 0;
        self.conversation.scroll_locked = true;
        self.history_index = None;
    }

    /// Append an informational bubble produced by a slash command.
    pub fn push_notice(&mut self, text: String) {
        self.chat_entries.push(ChatEntry::Notice { text });
    }

    /// Recompute the suggestion overlay from the live draft.
    ///
    /// Called by the Driver after every textarea mutation. A visible model
    /// picker outranks suggestions: it was opened by an already-submitted
    /// command and is dismissed by Esc or a selection, not by typing — but
    /// further typing still live-narrows the picker's already-fetched
    /// catalog by re-deriving its filter from the draft.
    pub fn refresh_slash_overlay(&mut self, draft: &str) {
        if matches!(self.slash_overlay, SlashOverlay::Hidden) {
            self.slash_overlay_drag_start = None;
            self.slash_overlay_drag_end = None;
        }
        if let SlashOverlay::ModelPicker {
            filter, selected, ..
        } = &mut self.slash_overlay
        {
            let new_filter = extract_models_filter(draft);
            if *filter != new_filter {
                *filter = new_filter;
                *selected = 0;
            }
            return;
        }
        if matches!(self.slash_overlay, SlashOverlay::LoadingModels) {
            return;
        }
        let suggestions = slash_command_suggestions(draft);
        self.slash_overlay = if suggestions.is_empty() {
            SlashOverlay::Hidden
        } else {
            SlashOverlay::Suggestions {
                choices: suggestions,
                selected: 0,
            }
        };
    }

    /// Return the normalised screen-space range of an active slash-overlay
    /// drag, for rendering its highlight.
    pub fn slash_overlay_drag_selection(&self) -> Option<BannerDragSelection> {
        if !self.slash_overlay.is_visible() {
            return None;
        }
        let (Some((start_col, start_row)), Some((end_col, end_row))) =
            (self.slash_overlay_drag_start, self.slash_overlay_drag_end)
        else {
            return None;
        };
        let ((start_row, start_col), (end_row, end_col)) =
            if (start_row, start_col) <= (end_row, end_col) {
                ((start_row, start_col), (end_row, end_col))
            } else {
                ((end_row, end_col), (start_row, start_col))
            };
        Some(BannerDragSelection {
            start_row,
            start_col,
            end_row,
            end_col,
        })
    }

    /// Move the visible slash overlay's selection by `delta` entries,
    /// saturating at both ends. A no-op unless a selectable overlay is open.
    pub fn move_slash_selection(&mut self, delta: isize) {
        match &mut self.slash_overlay {
            SlashOverlay::Suggestions { choices, selected } => {
                move_selection(selected, choices.len(), delta);
            }
            SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
            } => {
                let visible_count = matching_model_indices(choices, filter).len();
                move_selection(selected, visible_count, delta);
            }
            _ => {}
        }
    }

    /// Cycle the selected model's reasoning effort, if the model advertises
    /// selectable alternatives. The implicit first state is the provider
    /// default, followed by each advertised effort in order.
    pub fn cycle_selected_model_reasoning_effort(&mut self, delta: isize) {
        let SlashOverlay::ModelPicker {
            choices,
            filter,
            selected,
        } = &mut self.slash_overlay
        else {
            return;
        };
        let Some(&actual_index) = matching_model_indices(choices, filter).get(*selected) else {
            return;
        };
        let choice = &mut choices[actual_index];
        let Some(reasoning_efforts) = choice.selection.model.reasoning_efforts.as_ref() else {
            return;
        };
        if reasoning_efforts.is_empty() {
            return;
        }

        let current_index = choice
            .selection
            .reasoning_effort
            .as_deref()
            .and_then(|effort| {
                reasoning_efforts
                    .iter()
                    .position(|candidate| candidate == effort)
                    .map(|index| index + 1)
            })
            .unwrap_or(0);
        let option_count = reasoning_efforts.len() + 1;
        let next_index =
            (current_index as isize + delta).rem_euclid(option_count as isize) as usize;
        choice.selection.reasoning_effort = if next_index == 0 {
            None
        } else {
            Some(reasoning_efforts[next_index - 1].clone())
        };
        choice.refresh_label();
    }

    /// The command currently highlighted in the suggestion overlay, if any.
    pub fn selected_slash_command(&self) -> Option<SlashCommandInfo> {
        match &self.slash_overlay {
            SlashOverlay::Suggestions { choices, selected } => choices.get(*selected).copied(),
            _ => None,
        }
    }

    /// The model the picker currently highlights, if it is open.
    pub fn selected_model(&self) -> Option<&ModelChoice> {
        match &self.slash_overlay {
            SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
            } => {
                let actual_index = *matching_model_indices(choices, filter).get(*selected)?;
                choices.get(actual_index)
            }
            _ => None,
        }
    }

    pub fn push_user_bubble(&mut self, text: String, state: UserMessageState) {
        self.chat_entries.push(ChatEntry::UserMessage {
            text: text.clone(),
            state,
        });
        self.input_history.push(text);
        self.history_index = None;
    }

    /// Stage a steer (a user message sent while the agent is busy) without
    /// committing it to `chat_entries` yet. Committing immediately would fix
    /// its position in the list before thinking/tool-call content for the
    /// in-flight round has been flushed, wedging the steer bubble ahead of
    /// content that chronologically precedes it. The staged text renders as
    /// a live `Queued` bubble near the bottom of the chat (via
    /// `TuiState::queued_steer_texts`) until `commit_queued_steer_messages`
    /// — called on `SteerApplied` or cancellation — commits it to
    /// `chat_entries` at the then-current tail, guaranteed to be after
    /// everything already flushed.
    pub fn queue_steer_message(&mut self, text: String) {
        self.queued_steer_texts.push(text.clone());
        self.input_history.push(text);
        self.history_index = None;
    }

    /// Scroll chat up (toward earlier content); disables auto-scroll.
    pub fn scroll_up(&mut self, lines: usize) {
        self.conversation.scroll_locked = false;
        self.conversation.scroll_offset = self.conversation.scroll_offset.saturating_sub(lines);
    }

    /// Scroll chat down (toward later content); disables auto-scroll.
    pub fn scroll_down(&mut self, lines: usize) {
        self.conversation.scroll_locked = false;
        self.conversation.scroll_offset = self.conversation.scroll_offset.saturating_add(lines);
    }

    /// Re-enable auto-scroll (jump to bottom).
    pub fn scroll_to_bottom(&mut self) {
        self.conversation.scroll_locked = true;
        self.conversation.scroll_offset = 0;
    }

    /// Scroll the help popup up.
    pub fn help_scroll_up(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_sub(lines);
    }

    /// Scroll the help popup down.
    pub fn help_scroll_down(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_add(lines);
    }

    /// Scroll the help popup left.
    pub fn help_scroll_left(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_sub(columns);
    }

    /// Scroll the help popup right.
    pub fn help_scroll_right(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_add(columns);
    }

    /// Scroll the clarify banner up (toward earlier content).
    pub fn clarify_scroll_up(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_sub(lines);
    }

    /// Scroll the clarify banner down (toward later content). The upper bound
    /// is applied in `draw_clarify_banner`, which knows the total content height.
    pub fn clarify_scroll_down(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_add(lines);
    }

    /// Scroll the child-view header bubble up (toward earlier content).
    pub fn header_bubble_scroll_up(&mut self, lines: usize) {
        self.child_view.header_bubble_scroll_offset = self
            .child_view
            .header_bubble_scroll_offset
            .saturating_sub(lines);
    }

    /// Scroll the child-view header bubble down (toward later content). The
    /// upper bound is applied in `tui_widgets::draw`, which knows the total content
    /// height.
    pub fn header_bubble_scroll_down(&mut self, lines: usize) {
        self.child_view.header_bubble_scroll_offset = self
            .child_view
            .header_bubble_scroll_offset
            .saturating_add(lines);
    }

    /// Toggle the collapsed/expanded state of the tool bubble with the given id.
    ///
    /// Collapsing a bubble whose body extends below the viewport shrinks the
    /// total content height, which can push `scroll_offset` past the new
    /// `max_offset` and re-lock the view to the bottom, scrolling the bubble's
    /// title out of sight. Clamping `scroll_offset` to the bubble's own start
    /// row keeps that title row visible in the current viewport.
    ///
    /// Delegates the actual lookup to `find_tool_bubble_mut`, which searches
    /// both the root (`chat_entries`) and every tracked child's own
    /// transcript (`child_conversations`) — the click that triggers this can
    /// target a bubble in any of them, depending on `active_view`.
    pub fn toggle_tool_bubble_collapsed(&mut self, target_id: Uuid) {
        let found = if let Some(ChatEntry::ToolBubble { collapsed, .. }) =
            self.find_tool_bubble_mut(target_id)
        {
            *collapsed = !*collapsed;
            true
        } else {
            false
        };
        if found {
            self.conversation.scroll_locked = false;
            if let Some((start_row, _, _)) = self
                .conversation
                .bubble_title_rows
                .iter()
                .find(|(_, _, bubble_id)| *bubble_id == target_id)
            {
                self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
            }
        }
    }

    /// Toggle the collapsed state of the `ChatEntry::ThinkingMessage` at
    /// `target_index` in `chat_entries`. Mirrors `toggle_tool_bubble_collapsed`
    /// but is index-keyed since thinking bubbles carry no `Uuid`.
    pub fn toggle_thinking_collapsed(&mut self, target_index: usize) {
        if let Some(ChatEntry::ThinkingMessage { collapsed, .. }) =
            self.chat_entries.get_mut(target_index)
        {
            *collapsed = !*collapsed;
            self.conversation.scroll_locked = false;
            if let Some((start_row, _, _)) = self
                .conversation
                .thinking_title_rows
                .iter()
                .find(|(_, _, entry_index)| *entry_index == target_index)
            {
                self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
            }
        }
    }

    /// Toggle the collapsed state of the in-flight (not yet flushed) thinking
    /// bubble rendered from `partial_thinking_text`. Mirrors
    /// `toggle_thinking_collapsed` but flips `partial_thinking_collapsed`
    /// directly since there is no `ChatEntry` yet — the value carries through
    /// unchanged into the flushed `ChatEntry::ThinkingMessage` once the
    /// stream ends (see `flush_partial_thinking_message`).
    pub fn toggle_partial_thinking_collapsed(&mut self) {
        self.turn_buffer.partial_thinking_collapsed = !self.turn_buffer.partial_thinking_collapsed;
        self.conversation.scroll_locked = false;
        if let Some((start_row, _, _)) = self
            .conversation
            .thinking_title_rows
            .iter()
            .find(|(_, _, entry_index)| *entry_index == STREAMING_THINKING_ENTRY_INDEX)
        {
            self.conversation.scroll_offset = self.conversation.scroll_offset.min(*start_row);
        }
    }

    /// Extract the text covered by the current `chat_selection`.
    pub fn extract_chat_selection_text(&self) -> Option<String> {
        let sel = self.conversation.chat_selection.as_ref()?;
        if sel.is_empty() {
            return None;
        }
        let (start_line, start_col, end_line, end_col) = sel.normalized();
        let lines = &self.conversation.chat_rendered_lines;
        let start_line = start_line.min(lines.len().saturating_sub(1));
        let end_line = end_line.min(lines.len().saturating_sub(1));
        if start_line > end_line || lines.is_empty() {
            return None;
        }
        let chrome = |index: usize| -> usize {
            self.conversation
                .chat_rendered_line_chrome_widths
                .get(index)
                .copied()
                .unwrap_or(0)
        };
        if start_line == end_line {
            let line = &lines[start_line];
            let c = chrome(start_line);
            Some(char_range(
                line,
                start_col.saturating_sub(c),
                end_col.saturating_sub(c),
            ))
        } else {
            let mut result = String::new();
            for (absolute_index, line) in lines
                .iter()
                .enumerate()
                .skip(start_line)
                .take(end_line - start_line + 1)
            {
                let c = chrome(absolute_index);
                let fragment = if absolute_index == start_line {
                    char_range(line, start_col.saturating_sub(c), line.chars().count())
                } else if absolute_index == end_line {
                    char_range(line, 0, end_col.saturating_sub(c))
                } else {
                    line.clone()
                };
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&fragment);
            }
            Some(result)
        }
    }

    /// Show a transient toast notification.
    pub fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now() + TOAST_DURATION));
    }

    /// Navigate input history. Returns the text to load, or `None` if boundary reached.
    pub fn navigate_input_message_history(
        &mut self,
        direction: HistoryDirection,
        current_text: &str,
    ) -> Option<String> {
        match direction {
            HistoryDirection::Up => {
                if self.input_history.is_empty() {
                    return None;
                }
                match self.history_index {
                    None => {
                        self.draft = current_text.to_string();
                        self.history_index = Some(self.input_history.len() - 1);
                    }
                    Some(0) => return None,
                    Some(index) => {
                        self.history_index = Some(index - 1);
                    }
                }
                Some(self.input_history[self.history_index.expect("just set")].clone())
            }
            HistoryDirection::Down => match self.history_index {
                None => None,
                Some(index) if index < self.input_history.len() - 1 => {
                    self.history_index = Some(index + 1);
                    Some(self.input_history[index + 1].clone())
                }
                Some(_) => {
                    self.history_index = None;
                    Some(std::mem::take(&mut self.draft))
                }
            },
        }
    }

    /// Exit history browsing (if active) before an edit keypress.
    pub fn exit_input_history_browsing(&mut self, current_text: &str) {
        if self.history_index.is_some() {
            self.draft = current_text.to_string();
            self.history_index = None;
        }
    }

    // ── View-layer helpers ────────────────────────────────────────────────────

    /// Convert the current [`TuiMode`] (and [`Focus`]) to the legacy
    /// [`AgentMode`] used by `tui_widgets.rs` for footer and spinner rendering.
    pub fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            // A spawned subagent keeps its `spawn_subagent` tool bubble in
            // `Running` until the child actually finishes, independently of
            // the root loop's own `TuiMode` — the root can go `Idle` (its
            // own turn ended) while the child is still working. Report
            // `ToolRunning` in that case so the footer/spinner doesn't look
            // finished while a child is still active.
            TuiMode::Idle => {
                if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Idle
                }
            }
            TuiMode::Pending => AgentMode::Pending,
            TuiMode::Streaming => {
                if matches!(self.focus, Focus::ToolApproval { .. }) {
                    AgentMode::ToolPending
                } else if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Streaming
                }
            }
            TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
            TuiMode::Aborting => AgentMode::Aborting,
            TuiMode::DoublePressAbort { return_to, .. } => match return_to.as_ref() {
                TuiMode::Idle | TuiMode::DoublePressAbort { .. } => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Idle
                    }
                }
                TuiMode::Pending => AgentMode::Pending,
                TuiMode::Streaming => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Streaming
                    }
                }
                TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
                TuiMode::Aborting => AgentMode::Aborting,
            },
        }
    }

    /// Return the active child-agent counts for the root conversation.
    ///
    /// This is derived from the root transcript rather than
    /// `child_conversations`, which intentionally retains finished
    /// conversations for drill-in navigation.
    pub fn active_subagent_counts(&self) -> ActiveSubagentCounts {
        ActiveSubagentCounts::from_entries(&self.chat_entries)
    }

    /// Returns `true` if any `ChatEntry::ToolBubble` is currently in the
    /// `Running` state, i.e. a tool call has started but not yet finished
    /// (`Done`/`Error`/`Denied`/`Killed`). Used by `agent_mode` to
    /// distinguish "a tool is executing" from plain LLM streaming (thinking
    /// or assistant text) in the footer, and — for `TuiMode::Idle` — to
    /// detect a still-running `spawn_subagent` bubble whose child hasn't
    /// finished yet even though the root loop's own turn already has.
    fn has_running_tool_bubble(&self) -> bool {
        Self::entries_have_running_tool_bubble(&self.chat_entries)
    }

    /// Shared by `has_running_tool_bubble` (root) and `agent_mode_for_child`
    /// (a drilled-into child's own transcript) — same "is a tool call still
    /// executing" check, just parameterized over which entry list to scan.
    fn entries_have_running_tool_bubble(entries: &[ChatEntry]) -> bool {
        entries.iter().any(|entry| {
            matches!(
                entry,
                ChatEntry::ToolBubble { state, .. } if matches!(state.as_ref(), ToolCallState::Running { .. })
            )
        })
    }

    /// [`AgentMode`] for a drilled-into child conversation (`run_id`), used
    /// by `render::project_for_draw` in place of `agent_mode()` whenever
    /// `active_view` is `ConversationView::Child` — `agent_mode()` alone
    /// reflects only the *root* loop's `TuiMode`, which stays whatever it
    /// was when the root issued `spawn_subagent` (typically `Idle`, mapped
    /// to `ToolRunning` via `has_running_tool_bubble` because the root's own
    /// `spawn_subagent` `ToolBubble` is still `Running`) regardless of
    /// whether the child itself is currently thinking/streaming, running one
    /// of its own tools, or already finished. Without this, the child's own
    /// in-flight `partial_thinking_text`/`partial_assistant_text` (correctly
    /// projected in `project_for_draw`) never renders as a streaming bubble,
    /// because `tui_widgets::draw` only shows it when `agent_mode ==
    /// AgentMode::Streaming` — text popped straight from "invisible" to a
    /// fully-committed bubble once flushed (see docs/known-test-issues.md).
    pub fn agent_mode_for_child(&self, run_id: uuid::Uuid) -> AgentMode {
        let is_pending_approval_for_this_child = matches!(self.focus, Focus::ToolApproval { tool_id, .. }
        if self.child_conversations.get(&run_id).is_some_and(|conversation| {
            conversation.messages.iter().any(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
        }));
        if is_pending_approval_for_this_child {
            return AgentMode::ToolPending;
        }
        let has_running_tool_bubble =
            self.child_conversations
                .get(&run_id)
                .is_some_and(|conversation| {
                    Self::entries_have_running_tool_bubble(&conversation.messages)
                });
        if has_running_tool_bubble {
            AgentMode::ToolRunning
        } else if self
            .running_child
            .as_ref()
            .is_some_and(|child| child.run_id == run_id)
        {
            AgentMode::Streaming
        } else {
            AgentMode::Idle
        }
    }

    /// Returns `true` when the help popup is currently displayed.
    pub fn show_help(&self) -> bool {
        matches!(self.focus, Focus::HelpPopup)
    }

    /// Returns `true` when a tool is awaiting user approval.
    pub fn is_approval_pending(&self) -> bool {
        matches!(self.focus, Focus::ToolApproval { .. })
    }

    /// Returns the name of the tool awaiting approval, or `None` when not in
    /// approval sub-mode.
    pub fn approval_tool_name(&self) -> Option<&str> {
        if let Focus::ToolApproval { tool_name, .. } = &self.focus {
            Some(tool_name.as_str())
        } else {
            None
        }
    }

    /// Update the tool bubble for `id` to the Error state. If `id` was the
    /// tool awaiting approval, clear the approval focus and reset mode to Idle.
    pub fn apply_tool_error(
        &mut self,
        id: Uuid,
        error: String,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
        run_id: Option<Uuid>,
    ) {
        if let Some(entry) = self.find_tool_bubble_mut(id) {
            if let ChatEntry::ToolBubble {
                state: bubble_state,
                ..
            } = entry
            {
                // Skip bubbles already transitioned to Denied by the Driver
                // (via apply_tool_denied). The ToolError that the agent sends
                // after processing a DenyTool command is redundant for those.
                if matches!(bubble_state.as_ref(), ToolCallState::Denied { .. }) {
                    return;
                }
                let preserved_title_arguments = match bubble_state.as_ref() {
                    ToolCallState::Pending {
                        title_arguments, ..
                    }
                    | ToolCallState::Running {
                        title_arguments, ..
                    }
                    | ToolCallState::Done {
                        title_arguments, ..
                    }
                    | ToolCallState::Error {
                        title_arguments, ..
                    } => title_arguments.clone(),
                    _ => vec![],
                };
                **bubble_state = ToolCallState::Error {
                    error: error.clone(),
                    title_arguments: preserved_title_arguments,
                    duration,
                    content_sections,
                };
            }
        } else {
            self.target_messages_mut(run_id).push(ChatEntry::ErrorLine {
                message: error.clone(),
            });
        }
        if matches!(self.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
            self.focus = Focus::MainInput;
            self.mode = TuiMode::Idle;
        }
        self.resolve_possible_child_pause(id);
    }

    /// Create a `Denied` bubble for a tool that was auto-denied by the sandbox
    /// before it ever started. No prior `ToolStart` was sent for this id.
    pub fn apply_tool_auto_denied(
        &mut self,
        id: Uuid,
        name: String,
        title_arguments: Vec<(String, serde_json::Value)>,
        content_sections: Vec<crate::agent::tools::Section>,
        reason: String,
        run_id: Option<Uuid>,
    ) {
        self.target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name,
                state: Box::new(ToolCallState::Denied {
                    arguments: serde_json::Value::Null,
                    error: reason,
                    content_sections,
                    title_arguments,
                }),
                collapsed: false,
            });
    }

    /// Mark a `Pending` tool bubble as `Denied`, preserving its arguments.
    ///
    /// Called by the Driver immediately when `Effect::DenyTool` is executed,
    /// before the agent sends the corresponding `ToolError` response.
    /// `reason` is the user-supplied denial text; empty means no reason given.
    pub fn apply_tool_denied(&mut self, id: Uuid, reason: &str) {
        if let Some(ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        }) = self.find_tool_bubble_mut(id)
        {
            let (preserved_arguments, preserved_content_sections, preserved_title_arguments) =
                match bubble_state.as_mut() {
                    ToolCallState::Pending {
                        arguments,
                        content_sections,
                        title_arguments,
                    }
                    | ToolCallState::Running {
                        arguments,
                        content_sections,
                        title_arguments,
                        ..
                    } => (
                        std::mem::replace(arguments, serde_json::Value::Null),
                        std::mem::take(content_sections),
                        std::mem::take(title_arguments),
                    ),
                    _ => (serde_json::Value::Null, vec![], vec![]),
                };
            **bubble_state = ToolCallState::Denied {
                arguments: preserved_arguments,
                error: if reason.is_empty() {
                    "Denied by user".to_string()
                } else {
                    format!("Denied by user: {reason}")
                },
                content_sections: preserved_content_sections,
                title_arguments: preserved_title_arguments,
            };
        }
        self.resolve_possible_child_pause(id);
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolCallRequest;

    #[test]
    fn replay_projection_restores_full_fidelity_tool_bubble() {
        let tool_id = Uuid::new_v4();
        let content_section =
            crate::agent::tools::Section::Content(crate::agent::tools::ContentSection {
                label: Some("Output".to_string()),
                content: "file contents".to_string(),
                file_extension: Some("txt".to_string()),
                line_number_offset: Some(2),
            });
        let messages = vec![
            Message::User {
                content: "read the file".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id: tool_id,
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "README.md"}),
                    persisted_title_arguments: vec![(
                        "path".to_string(),
                        serde_json::json!("README.md"),
                    )],
                }],
            },
            Message::ToolResult {
                value: crate::agent::ToolResult {
                    id: tool_id,
                    name: "read".to_string(),
                    result: "file contents".to_string(),
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![("path".to_string(), "README.md".to_string())],
                    content_sections: vec![content_section.clone()],
                    duration: Duration::from_millis(37),
                },
            },
        ];

        let entries = project_messages(&messages);
        assert!(matches!(
            entries.first(),
            Some(ChatEntry::UserMessage { text, state: UserMessageState::Sent })
                if text == "read the file"
        ));
        assert!(matches!(
            entries.get(1),
            Some(ChatEntry::ToolBubble { id, state, collapsed: true, .. })
                if *id == tool_id && matches!(
                    state.as_ref(),
                    ToolCallState::Done {
                        arguments,
                        result_section,
                        content_sections,
                        summary,
                        duration,
                        title_arguments,
                    } if arguments == &serde_json::json!({"path": "README.md"})
                        && result_section == &ResultSection::plain("file contents")
                        && content_sections == &vec![content_section]
                        && summary == &vec![("path".to_string(), "README.md".to_string())]
                        && *duration == Duration::from_millis(37)
                        && title_arguments == &vec![("path".to_string(), serde_json::json!("README.md"))]
                )
        ));
    }

    #[test]
    fn replay_projection_replaces_spawn_result_with_finished_or_orphaned_child_state() {
        let finished_call_id = Uuid::new_v4();
        let orphaned_call_id = Uuid::new_v4();
        let finished_run_id = Uuid::new_v4();
        let orphaned_run_id = Uuid::new_v4();
        let spawn_call = |id| ToolCallRequest {
            id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            arguments: serde_json::json!({"task": "inspect the patch"}),
            persisted_title_arguments: vec![],
        };
        let messages = vec![Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![spawn_call(finished_call_id), spawn_call(orphaned_call_id)],
        }];
        let child_runs = vec![
            ChildRunRecord {
                run_id: finished_run_id,
                call_id: finished_call_id,
                parent_session: Uuid::new_v4(),
                agent_type: "research".to_string(),
                task: "inspect the patch".to_string(),
                label: "Inspect patch".to_string(),
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
                completed_at: Some("2026-01-01T00:00:01.000Z".to_string()),
                status: Some("success".to_string()),
                result: Some("patch is safe".to_string()),
                sources: vec!["src/lib.rs".to_string()],
                duration: Some(Duration::from_secs(1)),
            },
            ChildRunRecord {
                run_id: orphaned_run_id,
                call_id: orphaned_call_id,
                parent_session: Uuid::new_v4(),
                agent_type: "review".to_string(),
                task: "inspect the patch".to_string(),
                label: "Inspect patch".to_string(),
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
                completed_at: None,
                status: None,
                result: None,
                sources: vec![],
                duration: None,
            },
        ];

        let entries = project_messages_with_child_runs(&messages, &child_runs);
        let finished_entry = entries
            .iter()
            .find(|entry| {
                matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == finished_call_id)
            })
            .expect("finished child bubble");
        assert!(matches!(
            finished_entry,
            ChatEntry::ToolBubble {
                state,
                ..
            } if matches!(
                state.as_ref(),
                ToolCallState::Done {
                    summary,
                    content_sections,
                    duration,
                    ..
                } if summary == &vec![("status".to_string(), "success".to_string())]
                    && content_sections.len() == 2
                    && *duration == Duration::from_secs(1)
            )
        ));

        let orphaned_entry = entries
            .iter()
            .find(|entry| {
                matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == orphaned_call_id)
            })
            .expect("orphaned child bubble");
        assert!(matches!(
            orphaned_entry,
            ChatEntry::ToolBubble {
                state,
                ..
            } if matches!(
                state.as_ref(),
                ToolCallState::Error { error, .. }
                    if error == "Subagent did not finish before the session ended."
            )
        ));
    }

    #[test]
    fn replay_child_transcripts_load_once_on_drill_in() {
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let child_runs = vec![ChildRunRecord {
            run_id,
            call_id,
            parent_session: Uuid::new_v4(),
            agent_type: "research".to_string(),
            task: "inspect the patch".to_string(),
            label: "Inspect patch".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            completed_at: Some("2026-01-01T00:00:01.000Z".to_string()),
            status: Some("success".to_string()),
            result: Some("done".to_string()),
            sources: vec![],
            duration: Some(Duration::from_secs(1)),
        }];
        let root_messages = vec![Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: call_id,
                name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                arguments: serde_json::json!({"task": "inspect the patch"}),
                persisted_title_arguments: vec![],
            }],
        }];
        let child_messages = vec![
            Message::User {
                content: "inspect the patch".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: "done".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: vec![],
            },
        ];

        let mut state = AppState::new();
        state.replay_messages(&root_messages, &child_runs);
        assert!(state.child_needs_lazy_load(run_id));
        assert!(state
            .child_conversations
            .get(&run_id)
            .expect("replayed child")
            .messages
            .is_empty());

        state.load_child_messages(run_id, &child_messages);
        let loaded_messages = state
            .child_conversations
            .get(&run_id)
            .expect("replayed child")
            .messages
            .clone();
        assert!(!state.child_needs_lazy_load(run_id));
        assert_eq!(loaded_messages.len(), 2);

        state.load_child_messages(
            run_id,
            &[Message::Assistant {
                content: "should not replace the cached transcript".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: vec![],
            }],
        );
        assert_eq!(
            state
                .child_conversations
                .get(&run_id)
                .expect("replayed child")
                .messages,
            loaded_messages
        );
    }

    #[test]
    fn matching_model_indices_requires_every_filter_word_in_any_order() {
        let choice = |provider_connection: &str, model_id: &str| ModelChoice {
            label: format!("{provider_connection}/{model_id}"),
            selection: ProviderModelOption {
                provider_connection: provider_connection.to_string(),
                model: crate::llm::ModelInfo {
                    id: model_id.to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                },
                reasoning_effort: None,
            },
            is_active: false,
            engine_name: None,
        };
        let choices = vec![
            choice("openrouter", "deepseek-v3"),
            choice("ollama", "llama3.2"),
            choice("stub", "small"),
        ];

        // A multi-word filter must narrow by every word, not the joined
        // phrase as a single substring — each word can match anywhere
        // (provider or model) and in any order.
        assert_eq!(
            matching_model_indices(&choices, "deepseek v3"),
            vec![0],
            "every word of the filter must match, regardless of order"
        );
        assert_eq!(
            matching_model_indices(&choices, "v3 deepseek"),
            vec![0],
            "word order must not matter"
        );
        assert_eq!(
            matching_model_indices(&choices, "openrouter deepseek"),
            vec![0],
            "words may match across the provider and the model id"
        );
        assert_eq!(
            matching_model_indices(&choices, "deepseek missing-word"),
            Vec::<usize>::new(),
            "every word must match somewhere; one unmatched word excludes the choice"
        );
        assert_eq!(
            matching_model_indices(&choices, "   "),
            vec![0, 1, 2],
            "an all-whitespace filter matches everything"
        );
    }

    #[test]
    fn slash_overlay_text_rows_preserves_filtered_model_labels_and_divider() {
        let choice =
            |label: &str, provider_connection: &str, engine_name: Option<&str>| ModelChoice {
                label: label.to_string(),
                selection: ProviderModelOption {
                    provider_connection: provider_connection.to_string(),
                    model: crate::llm::ModelInfo {
                        id: label.to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                    },
                    reasoning_effort: None,
                },
                is_active: false,
                engine_name: engine_name.map(str::to_string),
            };
        let choices = vec![
            choice("engine/model", "engine", Some("engine")),
            choice("provider/model", "provider", None),
        ];

        let filtered_overlay = SlashOverlay::ModelPicker {
            choices: choices.clone(),
            filter: "provider".to_string(),
            selected: 0,
        };
        assert_eq!(
            slash_overlay_text_rows(&filtered_overlay),
            vec!["provider/model".to_string()],
            "filtering must keep labels tied to the matching choice index"
        );

        let all_choices_overlay = SlashOverlay::ModelPicker {
            choices,
            filter: String::new(),
            selected: 0,
        };
        assert_eq!(
            slash_overlay_text_rows(&all_choices_overlay),
            vec![
                "engine/model".to_string(),
                String::new(),
                "provider/model".to_string()
            ],
            "the divider must remain an empty non-selectable row"
        );
    }

    #[test]
    fn model_choice_label_explains_reasoning_effort_availability() {
        let model_option = |reasoning_efforts| ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: None,
                reasoning_efforts,
            },
            reasoning_effort: None,
        };

        assert_eq!(
            model_choice_label(&model_option(None), false),
            "stub/model  reasoning: unavailable"
        );
        assert_eq!(
            model_choice_label(&model_option(Some(vec![])), false),
            "stub/model  reasoning: default"
        );
        assert_eq!(
            model_choice_label(&model_option(Some(vec!["low".to_string()])), false),
            "stub/model  reasoning: default ⚙"
        );
    }

    #[test]
    fn model_choice_label_keeps_context_and_reasoning_metadata_visible() {
        let option = ProviderModelOption {
            provider_connection: "openrouter".to_string(),
            model: crate::llm::ModelInfo {
                id: "openai/gpt-oss-120b".to_string(),
                context_length: Some(131_072),
                reasoning_efforts: Some(vec!["low".to_string()]),
            },
            reasoning_effort: None,
        };

        assert_eq!(
            model_choice_label(&option, false),
            "openrouter/openai/gpt-oss-120b  context: 128kb  reasoning: default ⚙"
        );
    }

    #[test]
    fn model_choice_label_formats_context_length_as_integer_binary_size() {
        let model_option = |context_length| ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: Some(context_length),
                reasoning_efforts: None,
            },
            reasoning_effort: None,
        };

        assert_eq!(
            model_choice_label(&model_option(2_048), false),
            "stub/model  context: 2kb  reasoning: unavailable"
        );
        assert_eq!(
            model_choice_label(&model_option(1_048_576), false),
            "stub/model  context: 1mb  reasoning: unavailable"
        );
        assert_eq!(
            model_choice_label(&model_option(1_023), false),
            "stub/model  context: 1023 tokens  reasoning: unavailable"
        );
    }

    // ── Focus::is_modal ───────────────────────────────────────────────────────

    #[test]
    fn focus_is_modal_returns_true_only_for_help_popup() {
        assert!(Focus::HelpPopup.is_modal());
        assert!(!Focus::MainInput.is_modal());
        assert!(!Focus::ToolApproval {
            tool_id: Uuid::new_v4(),
            tool_name: "test_tool".to_string(),
        }
        .is_modal());
    }

    // ── ChatArea::contains geometry ───────────────────────────────────────────

    #[test]
    fn chat_area_contains_detects_interior_cells() {
        let area = ChatArea {
            rect: Rect::new(2, 3, 10, 5), // x=2, y=3, w=10, h=5
        };
        // Interior
        assert!(area.contains(2, 3)); // top-left corner
        assert!(area.contains(11, 7)); // bottom-right corner
        assert!(area.contains(5, 5)); // somewhere inside

        // Exterior
        assert!(!area.contains(1, 3)); // left of area
        assert!(!area.contains(12, 3)); // right of area (x + w = 12)
        assert!(!area.contains(2, 2)); // above area
        assert!(!area.contains(2, 8)); // below area (y + h = 8)
    }

    #[test]
    fn chat_area_page_scroll_rows_returns_height_minus_one() {
        let area = ChatArea {
            rect: Rect::new(0, 0, 80, 20),
        };
        assert_eq!(area.page_scroll_rows(), 19);
    }

    #[test]
    fn chat_area_page_scroll_rows_saturates_at_zero_for_tiny_area() {
        let area = ChatArea {
            rect: Rect::new(0, 0, 80, 0),
        };
        assert_eq!(area.page_scroll_rows(), 0);
    }

    // ── InputAreaLayout::contains geometry ───────────────────────────────────

    #[test]
    fn input_area_contains_detects_interior_cells() {
        let area = InputAreaLayout {
            rect: Rect::new(0, 20, 80, 4),
        };
        assert!(area.contains(0, 20));
        assert!(area.contains(79, 23));
        assert!(!area.contains(80, 20));
        assert!(!area.contains(0, 24));
    }

    // ── HelpPopupLayout::hit ──────────────────────────────────────────────────

    #[test]
    fn help_popup_hit_detects_interior_cells() {
        let popup = HelpPopupLayout {
            rect: Rect::new(10, 5, 60, 30),
        };
        assert!(popup.hit(10, 5));
        assert!(popup.hit(69, 34));
        assert!(!popup.hit(9, 5));
        assert!(!popup.hit(70, 5));
    }

    // ── TuiMode construction and equality ─────────────────────────────────────

    #[test]
    fn tui_mode_idle_equals_idle() {
        assert_eq!(TuiMode::Idle, TuiMode::Idle);
    }

    #[test]
    fn tui_mode_idle_does_not_equal_pending() {
        assert_ne!(TuiMode::Idle, TuiMode::Pending);
    }

    #[test]
    fn tui_mode_double_press_abort_holds_pressed_at_and_return_to() {
        let now = Instant::now();
        let mode = TuiMode::DoublePressAbort {
            pressed_at: now,
            return_to: Box::new(TuiMode::Idle),
            armed_by: ConfirmKey::CtrlC,
        };
        if let TuiMode::DoublePressAbort { return_to, .. } = &mode {
            assert_eq!(**return_to, TuiMode::Idle);
        } else {
            panic!("expected DoublePressAbort");
        }
    }

    #[test]
    fn agent_mode_maps_clarify_pending_to_clarify_pending() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Test?".to_string(),
            context: None,
            choices: None,
        };
        assert_eq!(state.agent_mode(), AgentMode::ClarifyPending);
    }

    #[test]
    fn agent_mode_reports_tool_running_when_a_tool_bubble_is_running() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: "read".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: true,
        });
        assert_eq!(state.agent_mode(), AgentMode::ToolRunning);
    }

    #[test]
    fn agent_mode_reports_tool_running_when_idle_but_a_spawned_child_is_still_running() {
        // Regression: the root loop's own turn can settle to `TuiMode::Idle`
        // (e.g. after `spawn_subagent` acked with "running") while the
        // spawned child is still working — its `spawn_subagent` tool bubble
        // stays `Running` until the child actually finishes. The footer must
        // not report `Idle` in that case; that made a genuinely-running
        // subagent look finished/stuck (see docs/known-test-issues.md).
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: "spawn_subagent".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: true,
        });
        assert_eq!(state.agent_mode(), AgentMode::ToolRunning);
    }

    #[test]
    fn agent_mode_reports_idle_when_no_tool_bubble_is_running() {
        let state = AppState::new();
        assert_eq!(state.agent_mode(), AgentMode::Idle);
    }

    #[test]
    fn active_subagent_counts_include_running_and_queued_bubbles_only() {
        let mut state = AppState::new();
        let push_running_subagent = |state: &mut AppState, status: &str| {
            state.chat_entries.push(ChatEntry::ToolBubble {
                id: Uuid::new_v4(),
                name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                state: Box::new(ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![(
                        "status".to_string(),
                        serde_json::Value::String(status.to_string()),
                    )],
                }),
                collapsed: true,
            });
        };

        push_running_subagent(&mut state, "running");
        push_running_subagent(&mut state, "queued");
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({}),
                result_section: ResultSection::plain("finished"),
                content_sections: vec![],
                summary: vec![],
                duration: Duration::ZERO,
                title_arguments: vec![],
            }),
            collapsed: true,
        });

        assert_eq!(
            state.active_subagent_counts(),
            ActiveSubagentCounts {
                running: 1,
                queued: 1,
            }
        );
    }

    #[test]
    fn agent_mode_reports_streaming_when_no_tool_bubble_is_running() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: "read".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({}),
                result_section: ResultSection {
                    id: "output".to_string(),
                    content: String::new(),
                },
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::ZERO,
                title_arguments: vec![],
            }),
            collapsed: true,
        });
        assert_eq!(state.agent_mode(), AgentMode::Streaming);
    }

    #[test]
    fn apply_tool_denied_preserves_arguments() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        let args = serde_json::json!({"cmd": "rm -rf /"});
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: args.clone(),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_denied(id, "");
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, error, .. } if *arguments == args && error == "Denied by user")
            ),
            "apply_tool_denied must produce Denied with the original Pending arguments"
        );
    }

    #[test]
    fn apply_tool_error_skips_already_denied_bubble() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        let args = serde_json::json!({"cmd": "ls"});
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Denied {
                arguments: args.clone(),
                error: "Denied by user".to_string(),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_error(
            id,
            "Tool 'shell' was denied: Denied by user".to_string(),
            std::time::Duration::ZERO,
            vec![],
            None,
        );
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Denied { arguments, .. } if *arguments == args)
            ),
            "apply_tool_error must not overwrite a Denied bubble"
        );
    }

    #[test]
    fn apply_tool_error_on_running_bubble_produces_error() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "shell".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({"cmd": "ls"}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        state.apply_tool_error(
            id,
            "execution failed".to_string(),
            std::time::Duration::ZERO,
            vec![],
            None,
        );
        assert!(
            matches!(
                state.chat_entries.last(),
                Some(ChatEntry::ToolBubble {
                    state,
                    ..
                }) if matches!(state.as_ref(), ToolCallState::Error { error, .. } if error == "execution failed")
            ),
            "Running + ToolError must produce Error"
        );
    }

    // ── Thinking bubble flush / toggle ────────────────────────────────────────

    #[test]
    fn flush_partial_assistant_message_pushes_thinking_before_assistant_bubble() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.turn_buffer.partial_assistant_text = "the answer".to_string();
        state.flush_partial_assistant_message(false);

        assert!(state.turn_buffer.partial_thinking_text.is_empty());
        assert!(state.turn_buffer.partial_assistant_text.is_empty());
        assert_eq!(state.chat_entries.len(), 2);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "pondering..." && !*collapsed
        ));
        assert!(matches!(
            &state.chat_entries[1],
            ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
        ));
    }

    #[test]
    fn flush_partial_assistant_message_computes_thinking_duration_from_started_at() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.turn_buffer.thinking_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(50));
        state.flush_partial_assistant_message(false);

        assert!(state.turn_buffer.thinking_started_at.is_none());
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { duration, .. } if *duration >= std::time::Duration::from_millis(50)
        ));
    }

    #[test]
    fn flush_partial_assistant_message_defaults_thinking_duration_when_not_started() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.flush_partial_assistant_message(false);

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { duration, .. } if *duration == std::time::Duration::ZERO
        ));
    }

    #[test]
    fn flush_partial_assistant_message_pushes_thinking_only_when_assistant_text_empty() {
        // A thinking-only turn: the model reasons, then calls a tool with no
        // visible prose. Must still flush the thinking bubble, without an
        // empty AssistantMessage.
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "considering options...".to_string();
        state.flush_partial_assistant_message(false);

        assert!(state.turn_buffer.partial_thinking_text.is_empty());
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, collapsed, .. } if text == "considering options..." && !*collapsed
        ));
    }

    #[test]
    fn flush_partial_assistant_message_pushes_nothing_when_both_empty() {
        let mut state = AppState::new();
        state.flush_partial_assistant_message(false);
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn flush_partial_assistant_message_drops_whitespace_only_assistant_text() {
        // Some models (observed: deepseek/deepseek-v4-flash) stream a lone
        // `"\n\n"` content chunk immediately before a tool call, with no
        // other prose. That whitespace-only text must not surface as a
        // visually-empty `AssistantMessage` bubble.
        let mut state = AppState::new();
        state.turn_buffer.partial_assistant_text = "\n\n".to_string();
        state.flush_partial_assistant_message(false);

        assert!(state.chat_entries.is_empty(), "{:?}", state.chat_entries);
        assert!(state.turn_buffer.partial_assistant_text.is_empty());
    }

    #[test]
    fn flush_child_partial_text_drops_whitespace_only_assistant_text() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        let conversation = state
            .child_conversations
            .get_mut(&run_id)
            .expect("conversation");
        conversation.turn_buffer.partial_assistant_text = "\n\n".to_string();
        state.flush_child_partial_text(run_id);

        let conversation = state
            .child_conversations
            .get(&run_id)
            .expect("conversation");
        assert!(
            conversation.messages.is_empty(),
            "{:?}",
            conversation.messages
        );
    }

    #[test]
    fn flush_partial_thinking_message_pushes_thinking_bubble_and_leaves_assistant_text_untouched() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.turn_buffer.partial_assistant_text = "so far so good".to_string();
        state.flush_partial_thinking_message();

        assert!(state.turn_buffer.partial_thinking_text.is_empty());
        assert_eq!(
            state.turn_buffer.partial_assistant_text, "so far so good",
            "must not touch the assistant buffer"
        );
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, .. } if text == "pondering..."
        ));
    }

    #[test]
    fn flush_partial_thinking_message_is_noop_when_no_pending_thinking_text() {
        let mut state = AppState::new();
        state.flush_partial_thinking_message();
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn flush_partial_response_message_pushes_assistant_bubble_and_leaves_thinking_text_untouched() {
        let mut state = AppState::new();
        state.turn_buffer.partial_assistant_text = "the answer".to_string();
        state.turn_buffer.partial_thinking_text = "still pondering...".to_string();
        state.flush_partial_response_message();

        assert!(state.turn_buffer.partial_assistant_text.is_empty());
        assert_eq!(
            state.turn_buffer.partial_thinking_text, "still pondering...",
            "must not touch the thinking buffer"
        );
        assert_eq!(state.chat_entries.len(), 1);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::AssistantMessage { text, interrupted } if text == "the answer" && !*interrupted
        ));
    }

    #[test]
    fn flush_partial_response_message_is_noop_when_no_pending_assistant_text() {
        let mut state = AppState::new();
        state.flush_partial_response_message();
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn toggle_thinking_collapsed_flips_state_at_index() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ThinkingMessage {
            text: "reasoning".to_string(),
            collapsed: true,
            duration: std::time::Duration::from_secs(3),
        });
        state.conversation.scroll_locked = true;

        state.toggle_thinking_collapsed(0);

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { collapsed, .. } if !*collapsed
        ));
        assert!(!state.conversation.scroll_locked);
    }

    #[test]
    fn toggle_partial_thinking_collapsed_flips_state_while_streaming() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.turn_buffer.partial_thinking_collapsed = false;
        state.conversation.scroll_locked = true;

        state.toggle_partial_thinking_collapsed();

        assert!(state.turn_buffer.partial_thinking_collapsed);
        assert!(!state.conversation.scroll_locked);

        state.toggle_partial_thinking_collapsed();

        assert!(!state.turn_buffer.partial_thinking_collapsed);
    }

    #[test]
    fn toggle_partial_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        // Simulate a previous render pass recording the in-flight bubble's
        // title row under the streaming sentinel index.
        state.conversation.thinking_title_rows = vec![(5, 40, STREAMING_THINKING_ENTRY_INDEX)];
        state.conversation.scroll_offset = 30;
        state.conversation.scroll_locked = false;

        state.toggle_partial_thinking_collapsed();

        assert_eq!(state.conversation.scroll_offset, 5);
    }

    #[test]
    fn toggle_thinking_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ThinkingMessage {
            text: "reasoning".to_string(),
            collapsed: false,
            duration: std::time::Duration::from_secs(3),
        });
        // Simulate a previous render pass recording this bubble's title row
        // and a viewport scrolled well below it (e.g. the long expanded body
        // pushed the view down).
        state.conversation.thinking_title_rows = vec![(5, 40, 0)];
        state.conversation.scroll_offset = 30;
        state.conversation.scroll_locked = false;

        state.toggle_thinking_collapsed(0);

        // The offset must not exceed the bubble's own start row, otherwise
        // the title scrolls out of the viewport once the body collapses.
        assert_eq!(state.conversation.scroll_offset, 5);
    }

    #[test]
    fn toggle_tool_bubble_collapsed_clamps_scroll_offset_to_bubble_start_row() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: "read".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        // Simulate a previous render pass recording this bubble's title row
        // and a viewport scrolled well below it (e.g. the long expanded body
        // pushed the view down).
        state.conversation.bubble_title_rows = vec![(5, 40, id)];
        state.conversation.scroll_offset = 30;
        state.conversation.scroll_locked = false;

        state.toggle_tool_bubble_collapsed(id);

        // The offset must not exceed the bubble's own start row, otherwise
        // the title scrolls out of the viewport once the body collapses.
        assert_eq!(state.conversation.scroll_offset, 5);
    }

    #[test]
    fn toggle_tool_bubble_collapsed_toggles_bubble_in_child_conversations() {
        let mut state = AppState::new();
        let id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state
            .child_conversations
            .get_mut(&run_id)
            .unwrap()
            .messages
            .push(ChatEntry::ToolBubble {
                id,
                name: "read".to_string(),
                state: Box::new(ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                }),
                collapsed: true,
            });

        state.toggle_tool_bubble_collapsed(id);

        let ChatEntry::ToolBubble { collapsed, .. } =
            &state.child_conversations[&run_id].messages[0]
        else {
            panic!("expected ToolBubble");
        };
        assert!(!collapsed);
    }

    #[test]
    fn toggle_thinking_collapsed_ignores_out_of_range_or_wrong_variant_index() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::AssistantMessage {
            text: "hi".to_string(),
            interrupted: false,
        });
        // Neither index should panic or mutate anything.
        state.toggle_thinking_collapsed(0);
        state.toggle_thinking_collapsed(5);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::AssistantMessage { .. }
        ));
    }

    // ── Header bubble scroll offset ───────────────────────────────────────────

    #[test]
    fn header_bubble_scroll_up_and_down_saturate_at_bounds() {
        let mut state = AppState::new();
        state.header_bubble_scroll_up(3);
        assert_eq!(
            state.child_view.header_bubble_scroll_offset, 0,
            "must not underflow below zero"
        );
        state.header_bubble_scroll_down(5);
        assert_eq!(state.child_view.header_bubble_scroll_offset, 5);
        state.header_bubble_scroll_up(2);
        assert_eq!(state.child_view.header_bubble_scroll_offset, 3);
    }

    #[test]
    fn switch_to_child_view_resets_header_bubble_scroll_offset() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.child_view.header_bubble_scroll_offset = 7;

        state.switch_to_child_view(run_id);

        assert_eq!(state.child_view.header_bubble_scroll_offset, 0);
    }

    #[test]
    fn switch_to_child_view_re_pins_chat_scroll_to_bottom() {
        // `scroll_offset`/`scroll_locked` are shared between the root and
        // child views (single fields, not per-view). If the user had
        // scrolled up while viewing `Root` and then drills into a running
        // subagent, that stale non-bottom position must not carry over —
        // the child transcript should always open pinned to its latest
        // activity (regression test for the bug where drilling in left the
        // list scrolled wherever `Root` had been left).
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.conversation.scroll_offset = 30;
        state.conversation.scroll_locked = false;

        state.switch_to_child_view(run_id);

        assert_eq!(state.conversation.scroll_offset, 0);
        assert!(
            state.conversation.scroll_locked,
            "child view must open auto-scrolled (pinned) to the bottom"
        );
    }

    #[test]
    fn switch_to_root_view_restores_scroll_position_from_before_drill_in() {
        // Regression test: Esc (empty draft) must land the user back where they
        // drilled in from (e.g. a `spawn_subagent` bubble scrolled up to),
        // not always at the end of the root conversation.
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.conversation.scroll_offset = 30;
        state.conversation.scroll_locked = false;

        state.switch_to_child_view(run_id);
        state.switch_to_root_view();

        assert_eq!(state.conversation.scroll_offset, 30);
        assert!(!state.conversation.scroll_locked);
        assert!(state.saved_root_scroll.is_none());
    }

    #[test]
    fn switch_to_root_view_is_a_no_op_on_scroll_when_child_was_auto_switched() {
        // `note_child_interactive_pause` auto-switches into the child view
        // without going through `switch_to_child_view`, so it never
        // populates `saved_root_scroll`. The scroll fields it leaves behind
        // belong to whatever was on screen while viewing `Root` and must be
        // left untouched on auto-return.
        let mut state = AppState::new();
        state.conversation.scroll_offset = 12;
        state.conversation.scroll_locked = false;
        state.active_view = ConversationView::Child {
            run_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        };

        state.switch_to_root_view();

        assert_eq!(state.conversation.scroll_offset, 12);
        assert!(!state.conversation.scroll_locked);
    }

    #[test]
    fn switch_to_child_view_captures_the_drilled_into_spawn_bubble_id() {
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });

        state.switch_to_child_view(run_id);

        assert_eq!(state.saved_root_scroll_bubble, Some(bubble_id));
    }

    #[test]
    fn switch_to_root_view_arms_pending_scroll_to_bubble_from_the_saved_bubble_id() {
        // Regression test: the raw offset saved by `switch_to_child_view`
        // drifts if the root conversation grows while the child view is
        // open (new messages/tool bubbles shift row indices). Resolving by
        // the drilled-into bubble's own id, once root is rendered again,
        // stays correct regardless.
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        // Simulate the root conversation growing while the child view is
        // open — must not affect which bubble is targeted.
        state.chat_entries.push(ChatEntry::AssistantMessage {
            text: "a concurrent root reply".to_string(),
            interrupted: false,
        });

        state.switch_to_root_view();

        assert_eq!(state.pending_scroll_to_bubble, Some(bubble_id));
        assert!(
            state.saved_root_scroll_bubble.is_none(),
            "must be consumed (taken), not just copied"
        );
    }

    #[test]
    fn header_bubble_id_is_none_while_viewing_root() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });

        assert_eq!(state.header_bubble_id(), None);
    }

    #[test]
    fn header_bubble_id_finds_the_root_spawn_subagent_bubble_while_viewing_child() {
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);

        assert_eq!(state.header_bubble_id(), Some(bubble_id));
    }

    #[test]
    fn toggle_header_bubble_collapsed_flips_the_root_bubble_without_touching_chat_scroll_state() {
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        state.conversation.scroll_offset = 42;
        state.conversation.scroll_locked = true;

        state.toggle_header_bubble_collapsed();

        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed)
        );
        assert_eq!(
            state.conversation.scroll_offset, 42,
            "must not touch the child transcript's own scroll"
        );
        assert!(
            state.conversation.scroll_locked,
            "must not touch the child transcript's own scroll lock"
        );
    }

    #[test]
    fn toggle_header_bubble_collapsed_is_a_no_op_while_viewing_root() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: Uuid::new_v4(),
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });

        state.toggle_header_bubble_collapsed();

        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed)
        );
    }
}
