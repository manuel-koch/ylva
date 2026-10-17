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

//! Conversation / replay state and operations.

use super::{
    result_section_ids, AppState, ChatEntry, ResultSection, SubagentConversation, SubagentRunInfo,
    ToolCallState, UserMessageState,
};
use crate::agent::{AssistantStatus, Message, UserMessageKind};
use crate::store::SubagentRunRecord;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

/// Project persisted domain messages into the same terminal-free chat model
/// used by live presenter updates.
pub fn project_messages(messages: &[Message]) -> Vec<ChatEntry> {
    project_messages_with_subagent_runs(messages, &[], None)
}

/// Project persisted messages into chat entries, inserting the compaction
/// marker at `compaction_boundary` — the transcript index where the verbatim
/// tail begins. `None` keeps the plain transcript view.
pub(in crate::tui_reducer_state) fn project_messages_with_subagent_runs(
    messages: &[Message],
    subagent_runs: &[SubagentRunRecord],
    compaction_boundary: Option<usize>,
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

    for (message_index, message) in messages.iter().enumerate() {
        if Some(message_index) == compaction_boundary {
            entries.push(ChatEntry::CompactionMarker {
                summarized_messages: message_index,
            });
        }
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
                        result_metadata,
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
                let mut display_summary = summary.clone();
                let mut display_sections = content_sections.clone();
                if name == crate::agent::tools::load_skill::LoadSkillTool::NAME {
                    use crate::agent::tools::{ContentSection, Section};

                    if !display_sections.iter().any(|section| {
                        matches!(section, Section::Content(content)
                            if content.label.as_deref() == Some("Source"))
                    }) {
                        let source = result_metadata
                            .as_ref()
                            .and_then(|metadata| metadata.get("source"))
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| {
                                summary
                                    .iter()
                                    .find(|(key, _)| key == "source")
                                    .map(|(_, value)| value.as_str())
                            });
                        if let Some(source) = source {
                            display_sections.insert(
                                0,
                                Section::Content(ContentSection {
                                    label: Some("Source".into()),
                                    content: source.to_string(),
                                    file_extension: None,
                                    line_number_offset: None,
                                }),
                            );
                        }
                    }
                    display_summary.retain(|(key, _)| key != "name" && key != "source");
                }
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
                            content: if result_section_id.as_deref()
                                == Some(result_section_ids::HIDDEN)
                            {
                                String::new()
                            } else {
                                result.clone()
                            },
                        },
                        content_sections: display_sections,
                        summary: display_summary,
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
    // A boundary at the very end of the transcript still gets its marker:
    // everything was summarized and the tail is empty.
    if Some(messages.len()) == compaction_boundary {
        entries.push(ChatEntry::CompactionMarker {
            summarized_messages: messages.len(),
        });
    }

    for subagent_run in subagent_runs {
        if subagent_run.call_id.is_nil() {
            continue;
        }
        let (name, arguments, title_arguments) = tool_calls
            .get(&subagent_run.call_id)
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
                ChatEntry::ToolBubble { id, state, .. } if *id == subagent_run.call_id => {
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
        let subagent_state = match (&subagent_run.status, &subagent_run.completed_at) {
            (Some(status), Some(_)) => {
                let finish_result = crate::agent::subagent_manager::FinishResult {
                    run_id: subagent_run.run_id,
                    call_id: subagent_run.call_id,
                    origin: subagent_run.origin,
                    status: status.clone(),
                    result: if status == "failure" {
                        None
                    } else {
                        subagent_run.result.clone()
                    },
                    error: if status == "failure" {
                        subagent_run.result.clone()
                    } else {
                        None
                    },
                    sources: subagent_run.sources.clone(),
                };
                preserved_content_sections.extend(finish_result.content_sections());
                ToolCallState::Done {
                    arguments,
                    result_section: ResultSection::plain(String::new()),
                    content_sections: preserved_content_sections,
                    summary: vec![("status".to_string(), status.clone())],
                    duration: subagent_run.duration.unwrap_or_default(),
                    title_arguments,
                }
            }
            _ => ToolCallState::Error {
                error: "Subagent did not finish before the session ended.".to_string(),
                title_arguments,
                duration: duration_since_timestamp(&subagent_run.created_at),
                content_sections: vec![],
            },
        };
        replace_or_append_tool_bubble(&mut entries, subagent_run.call_id, &name, subagent_state);
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

impl AppState {
    /// Replace the root view-model with a projection of a hydrated session.
    /// Persisted user messages also seed input history, while implicit
    /// harness notes remain model-only and are omitted by the projection.
    ///
    /// `compaction_boundary` is the persisted snapshot's
    /// transcript-relative cut (`SessionState::compaction_transcript_boundary`):
    /// it inserts [`ChatEntry::CompactionMarker`] at the position the model
    /// view was compacted at. `None` replays the plain transcript.
    pub fn replay_messages(
        &mut self,
        messages: &[Message],
        subagent_runs: &[SubagentRunRecord],
        compaction_boundary: Option<usize>,
    ) {
        self.invalidate_render_cache();
        self.chat_entries =
            project_messages_with_subagent_runs(messages, subagent_runs, compaction_boundary);
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
        self.subagent_conversations.clear();
        for subagent_run in subagent_runs {
            if subagent_run.call_id.is_nil() {
                continue;
            }
            self.subagent_conversations.insert(
                subagent_run.run_id,
                SubagentConversation::new_replay(SubagentRunInfo {
                    run_id: subagent_run.run_id,
                    bubble_id: subagent_run.call_id,
                    agent_type: subagent_run.agent_type.clone(),
                    task: subagent_run.task.clone(),
                    label: subagent_run.label.clone(),
                }),
            );
        }
        self.conversation.scroll_locked = true;
        self.conversation.scroll_offset = 0;
    }

    /// Replace a replay-discovered subagent's empty transcript after its first
    /// drill-in read, retaining its persisted compaction boundary.
    /// Repeated drill-ins keep the loaded transcript.
    pub fn load_subagent_messages(
        &mut self,
        run_id: Uuid,
        messages: &[Message],
        compaction_boundary: Option<usize>,
    ) {
        self.invalidate_transcript_layout(Some(run_id));
        if let Some(conversation) = self.subagent_conversations.get_mut(&run_id) {
            if conversation.needs_lazy_load {
                conversation.messages =
                    project_messages_with_subagent_runs(messages, &[], compaction_boundary);
                conversation.needs_lazy_load = false;
            }
        }
    }

    pub fn subagent_needs_lazy_load(&self, run_id: Uuid) -> bool {
        self.subagent_conversations
            .get(&run_id)
            .is_some_and(|conversation| conversation.needs_lazy_load)
    }
}
