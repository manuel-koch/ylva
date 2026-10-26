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

//! Conversation state and operations.

use super::{AppState, ChatSelection, ContextUsage, Focus, TuiMode};
use crate::agent::AgentMode;
use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;

mod navigation;
mod replay;
mod tools;
pub use replay::project_messages;
#[cfg(test)]
pub(super) use replay::project_messages_with_subagent_runs;

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
    /// Result text intentionally hidden from the user-facing tool bubble.
    pub const HIDDEN: &str = "hidden";
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
        /// used alongside `title_arguments` while running. Deliberately
        /// **not** carried forward into `Done` (which has no equivalent
        /// field) — once the call truly finishes, `Done`'s own `summary`
        /// is the single source of truth for its final status, so a stale
        /// in-flight value here can never linger into the terminal bubble.
        /// Internal progress values such as `status` and `run_id` are retained
        /// for rendering and routing but omitted from the rendered subagent
        /// title.
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
    /// Non-message divider marking where compaction cut the transcript: the
    /// `summarized_messages` messages before it are represented in the model
    /// view by the compaction summary, while the full transcript stays
    /// browsable here. Never sent to the model and never part of the
    /// transcript — it exists only in this view-model, built from the
    /// persisted snapshot's boundary.
    CompactionMarker {
        /// How many transcript messages the summary replaces — the boundary's
        /// index in persisted-transcript coordinates.
        summarized_messages: usize,
    },
}

/// A single in-flight turn's accumulated stream text, shared by the root
/// conversation (`AppState::turn_buffer`) and every subagent conversation
/// (`SubagentConversation::turn_buffer`) — the single place that knows how to
/// buffer/segment a live token stream into ordered `ChatEntry::ThinkingMessage`/
/// `ChatEntry::AssistantMessage` bubbles, so root and subagent transcripts can
/// never drift apart in how they handle thinking-vs-response text.
#[derive(Debug, Clone, Default)]
pub struct TurnBuffer {
    /// In-flight assistant reply text, appended by `push_response_chunk`
    /// (root: `StreamChunk`; subagent: `SubagentToken { is_thinking: false, .. }`).
    pub partial_assistant_text: String,
    /// In-flight extended-thinking/reasoning text, accumulated separately
    /// from `partial_assistant_text` so it never contaminates the assistant
    /// reply. Appended by `push_thinking_chunk` (root: `StreamThinkingChunk`;
    /// subagent: `SubagentToken { is_thinking: true, .. }`). Flushed into a
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

/// Identifies one subagent run's identity/metadata, keyed by `run_id` in
/// `AppState::subagent_conversations`. Populated on `DomainEvent::SubagentStarted`
/// and kept for the lifetime of the session (even after the subagent finishes)
/// so its transcript/header bubble can still be drilled into afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRunInfo {
    pub run_id: Uuid,
    /// The `id` of the real root `spawn_subagent` `ToolBubble` this run
    /// belongs to (the tool-call id) — the single source of truth
    /// correlating a specific bubble to a specific subagent conversation, used
    /// by both `AppState::header_bubble_id` (view → bubble) and the
    /// double-click handler (bubble → view), so multiple concurrent/queued
    /// `spawn_subagent` bubbles are never confused with one another.
    pub bubble_id: Uuid,
    pub agent_type: String,
    pub task: String,
    pub label: String,
}

/// One subagent run's own conversation transcript plus in-flight `TurnBuffer`,
/// keyed by `run_id` in `AppState::subagent_conversations`. Kept indefinitely
/// once created (mirrors `AppState::chat_entries`, which is also
/// session-lifetime-unbounded) so a finished subagent's transcript remains
/// drillable for as long as its root `spawn_subagent` bubble is visible.
#[derive(Debug, Clone)]
pub struct SubagentConversation {
    pub info: SubagentRunInfo,
    /// Monotonic start of live execution; replayed runs have no live timer.
    pub started_at: Option<Instant>,
    pub messages: Vec<ChatEntry>,
    pub turn_buffer: TurnBuffer,
    /// Latest context estimate reported by this subagent run. Kept with the
    /// conversation so the footer can show the correct window while drilled
    /// into this subagent, even after it finishes.
    pub context_usage: Option<ContextUsage>,
    pub needs_lazy_load: bool,
}

impl SubagentConversation {
    pub(crate) fn new(info: SubagentRunInfo) -> Self {
        SubagentConversation {
            info,
            started_at: Some(Instant::now()),
            messages: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            context_usage: None,
            needs_lazy_load: false,
        }
    }

    pub(crate) fn new_replay(info: SubagentRunInfo) -> Self {
        SubagentConversation {
            info,
            started_at: None,
            messages: Vec::new(),
            turn_buffer: TurnBuffer::default(),
            context_usage: None,
            needs_lazy_load: true,
        }
    }
}

/// Which conversation transcript is currently rendered in the main chat area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationView {
    /// The root/parent conversation (`AppState::chat_entries`).
    Root,
    /// The currently-tracked subagent's conversation (`AppState::subagent_conversations`).
    Subagent {
        run_id: Uuid,
        agent_type: String,
        task: String,
        label: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleExpansionTarget {
    Tool(Uuid),
    /// `STREAMING_THINKING_ENTRY_INDEX` denotes the in-flight thinking bubble.
    Thinking(usize),
}
/// Drill-in subagent-view header bubble state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubagentViewState {
    /// Current vertical scroll offset inside the subagent-view header bubble
    /// (lines from the top of its content), used only when the bubble's full
    /// content exceeds the header's clamped max height. Clamped to the valid
    /// range by `tui_widgets::draw` each frame; reset to 0 whenever the header bubble
    /// it applies to changes (see `presenter::domain::domain_subagent_lifecycle`).
    pub header_bubble_scroll_offset: usize,
    /// Plain text of every rendered header line, indexed before the vertical
    /// scroll offset. Used for extracting selected text on mouse-up.
    pub header_rendered_lines: Vec<String>,
    /// Number of leading header chrome characters stripped from each rendered
    /// line. Used to map terminal columns back to content columns.
    pub header_rendered_line_prefix_widths: Vec<usize>,
    /// Active or committed header selection.
    pub header_selection: Option<ChatSelection>,
    /// `true` while the left mouse button is held down in the header bubble.
    pub header_drag_active: bool,
}
/// Root-or-subagent scrollable conversation-list state (Phase 6.10 drill-in
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
    /// `TuiState::chat_rendered_line_prefix_widths`).
    pub chat_rendered_line_prefix_widths: Vec<usize>,
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
            chat_rendered_line_prefix_widths: Vec::new(),
            chat_selection: None,
            chat_drag_active: false,
            bubble_title_rows: Vec::new(),
            thinking_title_rows: Vec::new(),
        }
    }
}

impl AppState {
    /// Latest context usage for the conversation currently shown in the main
    /// chat area. Root usage remains independent from all subagent updates.
    pub fn active_context_usage(&self) -> Option<ContextUsage> {
        match &self.active_view {
            ConversationView::Root => self.context_usage,
            ConversationView::Subagent { run_id, .. } => self
                .subagent_conversations
                .get(run_id)
                .and_then(|conversation| conversation.context_usage),
        }
    }

    /// Flush only the in-flight thinking buffer into a `ChatEntry::ThinkingMessage`,
    /// leaving `partial_assistant_text` untouched. Used when a stream switches
    /// from thinking to response text (or hits a tool call) mid-turn, so each
    /// segment renders as its own bubble in chronological order rather than
    /// merging every thinking fragment of the turn into a single bubble.
    /// No-op when there is no pending thinking text. Delegates to
    /// `TurnBuffer::flush_thinking_message` — see there for the shared
    /// root/subagent implementation.
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
        self.queued_render_revision += 1;
        for text in std::mem::take(&mut self.queued_steer_texts) {
            self.chat_entries
                .push(ChatEntry::UserMessage { text, state });
        }
    }

    /// Flush `run_id`'s own `TurnBuffer` (accumulated `SubagentToken`
    /// fragments, thinking and assistant text alike) into new
    /// `ChatEntry::ThinkingMessage`/`ChatEntry::AssistantMessage` entries in
    /// its own transcript. No-op when there is no pending text, or `run_id`
    /// is not a tracked subagent. Delegates to `TurnBuffer::flush_assistant_message`
    /// — the same shared logic the root conversation uses.
    pub fn flush_subagent_partial_text(&mut self, run_id: Uuid) {
        let Some(conversation) = self.subagent_conversations.get_mut(&run_id) else {
            return;
        };
        conversation
            .turn_buffer
            .flush_assistant_message(&mut conversation.messages, false);
    }

    /// Flushes whichever partial-text accumulator `run_id` targets before a
    /// new tool/clarify bubble is created — that subagent's own `TurnBuffer`
    /// when `run_id` identifies any tracked subagent run, the root's own
    /// `turn_buffer` otherwise. Without this, a subagent's in-flight reply would
    /// stay stuck in its buffer until `SubagentDone`, making a tool call it
    /// made appear to precede its own explanatory text in its transcript.
    pub(crate) fn flush_target_partial_text(&mut self, run_id: Option<Uuid>) {
        if let Some(run_id) = run_id {
            if self.subagent_conversations.contains_key(&run_id) {
                self.flush_subagent_partial_text(run_id);
                return;
            }
        }
        self.flush_partial_assistant_message(false);
    }

    /// Manual navigation into `run_id`'s subagent view (double-clicking a
    /// `spawn_subagent` bubble's title). Marks the switch as *not*
    /// auto-switched, so the view never auto-returns to `Root` on its own —
    /// the user must navigate back explicitly (Esc with an empty draft).
    /// Works for any tracked subagent (running, queued, or already finished —
    /// see `subagent_conversations`), not just the currently-running one, so
    /// each `spawn_subagent` bubble unambiguously drills into *its own*
    /// conversation rather than whichever subagent happens to be running now.
    /// No-op when `run_id` is not a tracked subagent.
    /// Registers a new tracked subagent run (adding a fresh, empty entry to
    /// `subagent_conversations`) and marks it as running (`running_subagents`).
    /// Used by `presenter::domain::domain_subagent_lifecycle` on
    /// `DomainEvent::SubagentStarted`, and directly by tests that need to
    /// simulate a subagent run's existence without driving the full event
    /// pipeline.
    pub(crate) fn start_subagent_run(&mut self, info: SubagentRunInfo) {
        self.invalidate_transcript_layout(Some(info.run_id));
        self.running_subagents.insert(info.run_id, info.clone());
        self.subagent_conversations
            .insert(info.run_id, SubagentConversation::new(info));
    }

    pub(crate) fn subagent_started_at(&self) -> HashMap<Uuid, Instant> {
        self.subagent_conversations
            .values()
            .filter_map(|conversation| {
                conversation
                    .started_at
                    .map(|started_at| (conversation.info.bubble_id, started_at))
            })
            .chain(
                self.bash_started_at
                    .iter()
                    .map(|(id, started_at)| (*id, *started_at)),
            )
            .collect()
    }

    /// Append an informational bubble produced by a slash command.
    pub fn push_notice(&mut self, text: String) {
        self.push_notice_to(None, text);
    }

    /// Push a transcript notice into the root transcript, or — when
    /// `run_id` identifies a subagent — into that subagent's transcript, the same
    /// routing [`super::DomainEvent`]s use via [`AppState::target_messages_mut`].
    pub fn push_notice_to(&mut self, run_id: Option<Uuid>, text: String) {
        self.target_messages_mut(run_id)
            .push(ChatEntry::Notice { text });
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

    /// Return the active subagent counts for the root conversation.
    ///
    /// This is derived from the root transcript rather than
    /// `subagent_conversations`, which intentionally retains finished
    /// conversations for drill-in navigation.
    pub fn active_subagent_counts(&self) -> ActiveSubagentCounts {
        ActiveSubagentCounts::from_entries(&self.chat_entries)
    }

    /// Returns `true` if any `ChatEntry::ToolBubble` is currently in the
    /// `Running` state, i.e. a tool call has started but not yet finished
    /// (`Done`/`Error`/`Denied`/`Killed`). Used by `agent_mode` to
    /// distinguish "a tool is executing" from plain LLM streaming (thinking
    /// or assistant text) in the footer, and — for `TuiMode::Idle` — to
    /// detect a still-running `spawn_subagent` bubble whose subagent hasn't
    /// finished yet even though the root loop's own turn already has.
    pub(in crate::tui_reducer_state) fn has_running_tool_bubble(&self) -> bool {
        Self::entries_have_running_tool_bubble(&self.chat_entries)
    }

    /// Shared by `has_running_tool_bubble` (root) and `agent_mode_for_subagent`
    /// (a drilled-into subagent's own transcript) — same "is a tool call still
    /// executing" check, just parameterized over which entry list to scan.
    fn entries_have_running_tool_bubble(entries: &[ChatEntry]) -> bool {
        entries.iter().any(|entry| {
            matches!(
                entry,
                ChatEntry::ToolBubble { state, .. } if matches!(state.as_ref(), ToolCallState::Running { .. })
            )
        })
    }

    /// [`AgentMode`] for a drilled-into subagent conversation (`run_id`), used
    /// by `render::project_for_draw` in place of `agent_mode()` whenever
    /// `active_view` is `ConversationView::Subagent` — `agent_mode()` alone
    /// reflects only the *root* loop's `TuiMode`, which stays whatever it
    /// was when the root issued `spawn_subagent` (typically `Idle`, mapped
    /// to `ToolRunning` via `has_running_tool_bubble` because the root's own
    /// `spawn_subagent` `ToolBubble` is still `Running`) regardless of
    /// whether the subagent itself is currently thinking/streaming, running one
    /// of its own tools, or already finished. Without this, the subagent's own
    /// in-flight `partial_thinking_text`/`partial_assistant_text` (correctly
    /// projected in `project_for_draw`) never renders as a streaming bubble,
    /// because `tui_widgets::draw` only shows it when `agent_mode ==
    /// AgentMode::Streaming` — text popped straight from "invisible" to a
    /// fully-committed bubble once flushed (see docs/known-test-issues.md).
    pub fn agent_mode_for_subagent(&self, run_id: uuid::Uuid) -> AgentMode {
        // Any Pending bubble in this subagent's transcript means its loop is
        // blocked on an approval, even when that prompt is queued behind the
        // head shown in `Focus::ToolApproval`.
        let has_pending_approval = self.subagent_conversations.get(&run_id).is_some_and(|conversation| {
            conversation.messages.iter().any(|entry| {
                matches!(entry, ChatEntry::ToolBubble { state, .. } if matches!(**state, ToolCallState::Pending { .. }))
            })
        });
        if has_pending_approval {
            return AgentMode::ToolPending;
        }
        let has_running_tool_bubble =
            self.subagent_conversations
                .get(&run_id)
                .is_some_and(|conversation| {
                    Self::entries_have_running_tool_bubble(&conversation.messages)
                });
        if has_running_tool_bubble {
            AgentMode::ToolRunning
        } else if self.running_subagents.contains_key(&run_id) {
            AgentMode::Streaming
        } else {
            AgentMode::Idle
        }
    }
}
