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

use super::effects::{execute_effects, PendingRunIds};
use super::input::MouseDrag;
use super::{ActionOutcome, ClipboardSink};
use crate::agent::{AgentCommand, UiUpdate};
use crate::presenter::{self, TextareaContext, TuiEvent};
use crate::tui_reducer_state::{AppState, DomainEvent, Effect, TuiMode};
use crate::tui_render_state::TuiState;
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;

/// Map a [`UiUpdate`] to a [`DomainEvent`] for the Presenter.
/// Returns `None` for tool updates handled directly by the Driver.
pub(super) fn ui_update_to_domain_event(update: &UiUpdate) -> Option<DomainEvent> {
    match update {
        UiUpdate::StreamStarted => Some(DomainEvent::StreamStarted),
        UiUpdate::AppendText(text) => Some(DomainEvent::StreamChunk(text.clone())),
        UiUpdate::AppendThinking(text) => Some(DomainEvent::StreamThinkingChunk(text.clone())),
        UiUpdate::StreamDone => Some(DomainEvent::StreamDone),
        UiUpdate::StreamCancelled => Some(DomainEvent::StreamCancelled),
        UiUpdate::AgentIdle => Some(DomainEvent::AgentIdle),
        UiUpdate::ContextUsage {
            estimated_tokens,
            context_length,
            source,
        } => Some(DomainEvent::ContextUsage {
            estimated_tokens: *estimated_tokens,
            context_length: *context_length,
            source: *source,
        }),
        UiUpdate::SubagentContextUsage {
            run_id,
            estimated_tokens,
            context_length,
            source,
        } => Some(DomainEvent::SubagentContextUsage {
            run_id: *run_id,
            estimated_tokens: *estimated_tokens,
            context_length: *context_length,
            source: *source,
        }),
        UiUpdate::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        } => Some(DomainEvent::CompactionApplied {
            run_id: *run_id,
            entry_index: *entry_index,
            summarized_messages: *summarized_messages,
        }),
        UiUpdate::CompactionActivity {
            run_id,
            request_id,
            active,
        } => Some(DomainEvent::CompactionActivity {
            run_id: *run_id,
            request_id: *request_id,
            active: *active,
        }),
        UiUpdate::SteerQueued { run_id } => Some(DomainEvent::SteerQueued { run_id: *run_id }),
        UiUpdate::SteerApplied { run_id } => Some(DomainEvent::SteerApplied { run_id: *run_id }),
        UiUpdate::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => Some(DomainEvent::ToolPending {
            id: *id,
            name: name.clone(),
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => Some(DomainEvent::ClarifyPending {
            id: *id,
            question: question.clone(),
            context: context.clone(),
            choices: choices.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => Some(DomainEvent::ToolStart {
            id: *id,
            name: name.clone(),
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolProgress {
            id,
            title_arguments,
        } => Some(DomainEvent::ToolProgress {
            id: *id,
            title_arguments: title_arguments.clone(),
        }),
        UiUpdate::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => Some(DomainEvent::ToolDone {
            id: *id,
            name: name.clone(),
            result_section: result_section.clone(),
            summary: summary.clone(),
            duration: *duration,
            content_sections: content_sections.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolKilled { id, run_id } => Some(DomainEvent::ToolKilled {
            id: *id,
            run_id: *run_id,
        }),
        UiUpdate::Error { message, run_id } => Some(DomainEvent::Error {
            message: message.clone(),
            run_id: *run_id,
        }),
        UiUpdate::SubagentStarted {
            run_id,
            call_id,
            agent_type,
            task,
            label,
        } => Some(DomainEvent::SubagentStarted {
            run_id: *run_id,
            call_id: *call_id,
            agent_type: agent_type.clone(),
            task: task.clone(),
            label: label.clone(),
        }),
        UiUpdate::SubagentEngineConfigured { run_id, engine } => {
            Some(DomainEvent::SubagentEngineConfigured {
                run_id: *run_id,
                engine: engine.clone(),
            })
        }
        UiUpdate::SubagentToken {
            run_id,
            text,
            is_thinking,
        } => Some(DomainEvent::SubagentToken {
            run_id: *run_id,
            text: text.clone(),
            is_thinking: *is_thinking,
        }),
        UiUpdate::SubagentDone {
            run_id,
            status,
            result,
        } => Some(DomainEvent::SubagentDone {
            run_id: *run_id,
            status: status.clone(),
            result: result.clone(),
        }),
        // ToolError, ToolAutoDenied and Notice are handled directly by the
        // Driver via AppState; no corresponding DomainEvent exists in the
        // Presenter's surface.
        UiUpdate::ToolError { .. } | UiUpdate::ToolAutoDenied { .. } | UiUpdate::Notice { .. } => {
            None
        }
    }
}

/// Resolve a replay-discovered subagent transcript before the pure reducer enters
/// the subagent view. Live subagents are already populated and pass through
/// unchanged.
pub(super) fn prepare_subagent_replay_effects(
    state: &mut AppState,
    effects: Vec<Effect>,
    store: Option<&crate::store::Store>,
) -> Vec<Effect> {
    let session_span = store.map(|store| store.span());
    let _session_scope = session_span.as_ref().map(tracing::Span::enter);
    let mut prepared = Vec::with_capacity(effects.len());
    for effect in effects {
        match effect {
            Effect::LoadSubagentTranscript { run_id } => {
                if state.subagent_needs_lazy_load(run_id) {
                    let (messages, compaction_boundary) = match store {
                        Some(store) => {
                            let path = crate::store::session_path(
                                store.chats_root(),
                                store.project_key(),
                                run_id,
                            );
                            match crate::store::Store::read_session_state(&path) {
                                Ok(session_state) => (
                                    session_state.messages,
                                    session_state.compaction_transcript_boundary,
                                ),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        path = %path.display(),
                                        %run_id,
                                        "could not replay subagent transcript"
                                    );
                                    state.push_notice_to(
                                        Some(run_id),
                                        format!("Could not load subagent transcript: {error}"),
                                    );
                                    state.switch_to_subagent_view(run_id);
                                    continue;
                                }
                            }
                        }
                        None => (Vec::new(), None),
                    };
                    state.load_subagent_messages(run_id, &messages, compaction_boundary);
                }
                state.switch_to_subagent_view(run_id);
            }
            other => prepared.push(other),
        }
    }
    prepared
}

/// Process one [`UiUpdate`]: convert to a [`DomainEvent`] (or handle directly
/// for `ToolError`) and run the resulting effects.
#[expect(clippy::too_many_arguments)]
pub(super) async fn process_one_update(
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
    textarea_ctx: &TextareaContext,
    update: &UiUpdate,
    mouse_drag: &mut MouseDrag,
    clipboard: &mut dyn ClipboardSink,
    pending_run_ids: &mut PendingRunIds,
) -> ActionOutcome {
    pending_run_ids.run_outcome.record_update(update);
    if let UiUpdate::ToolError {
        id,
        error,
        duration,
        content_sections,
        run_id,
    } = update
    {
        state.apply_tool_error(
            *id,
            error.clone(),
            *duration,
            content_sections.clone(),
            *run_id,
        );
        return ActionOutcome::Continue;
    }
    if let UiUpdate::ToolAutoDenied {
        id,
        name,
        title_arguments,
        content_sections,
        reason,
        run_id,
    } = update
    {
        state.apply_tool_auto_denied(
            *id,
            name.clone(),
            title_arguments.clone(),
            content_sections.clone(),
            reason.clone(),
            *run_id,
        );
        return ActionOutcome::Continue;
    }
    if let UiUpdate::Notice { text, run_id } = update {
        state.push_notice_to(*run_id, text.clone());
        return ActionOutcome::Continue;
    }
    pending_run_ids.record_update(update);
    if let Some(domain_event) = ui_update_to_domain_event(update) {
        let effects = presenter::update(state, TuiEvent::Domain(domain_event), textarea_ctx);
        execute_effects(
            state,
            textarea,
            cmd_tx,
            effects,
            mouse_drag,
            clipboard,
            pending_run_ids,
        )
        .await
    } else {
        ActionOutcome::Continue
    }
}

/// Convert the driver's `AppState` to the legacy `TuiState` struct for return.
/// Fields not present in `AppState` are given sensible defaults.
pub(super) fn app_state_to_tui_state(app: AppState) -> TuiState {
    // Extract method results before consuming fields.
    let agent_mode = app.agent_mode();
    let show_help = app.show_help();
    let pending_clarify = if let TuiMode::ClarifyPending {
        question,
        context,
        choices,
        ..
    } = &app.mode
    {
        Some((question.clone(), choices.clone(), context.clone()))
    } else {
        None
    };
    let input_area_rect = app.layout.input_area.rect;
    let chat_area_rect = app.layout.chat_area.rect;
    let slash_overlay_drag = app.slash_overlay_drag_selection();
    let active_subagent_counts = app.active_subagent_counts();
    let subagent_started_at = app.subagent_started_at();
    let context_usage_text = app
        .active_context_usage()
        .map(crate::tui_reducer_state::format_context_usage_readout);
    TuiState {
        compacting: false,
        display_metadata: app.display_metadata,
        parent_agent_mode: agent_mode,
        agent_mode,
        active_subagent_counts,
        chat_entries: app.chat_entries,
        subagent_started_at,
        partial_assistant_text: app.turn_buffer.partial_assistant_text,
        partial_thinking_text: app.turn_buffer.partial_thinking_text,
        thinking_started_at: app.turn_buffer.thinking_started_at,
        partial_thinking_collapsed: app.turn_buffer.partial_thinking_collapsed,
        engine_display: app.engine_display,
        queued_steer_texts: app.queued_steer_texts,
        steer_indicator: app.steer_indicator,
        context_usage_text,
        cost_total_text: crate::llm::CostLedger::process().totals_text(),
        pending_approval: None,
        pending_approval_is_subagent: false,
        prompt_button_focus: None,
        deferred_prompt_count: 0,
        input_history: app.input_history,
        history_index: app.history_index,
        draft: app.draft,
        input_area: input_area_rect,
        toast: app.toast,
        spinner_frame: app.spinner_frame,
        input_scroll_top: app.input_scroll_top,
        slash_overlay: app.slash_overlay,
        slash_overlay_drag,
        slash_overlay_layout: app.layout.slash_overlay,
        help_popup: crate::tui_render_state::HelpPopupState {
            show_help,
            help_scroll_offset: app.help_popup.help_scroll_offset,
            help_scroll_x: app.help_popup.help_scroll_x,
            help_popup_rect: app.layout.help_popup.rect,
            help_content_area: app.layout.help_popup.content_area,
            help_rendered_lines: app.help_popup.help_rendered_lines,
            help_selection: app.help_popup.help_selection,
            help_drag_active: app.help_popup.help_drag_active,
        },
        clarify_banner_state: crate::tui_render_state::ClarifyBannerState {
            pending_clarify,
            clarify_scroll_offset: app.clarify_banner_state.clarify_scroll_offset,
            clarify_banner_drag: None,
            clarify_banner: ratatui::layout::Rect::default(),
            ok_button: ratatui::layout::Rect::default(),
            cancel_button: ratatui::layout::Rect::default(),
        },
        subagent_view: crate::tui_render_state::SubagentViewState {
            subagent_view_bubble: None,
            header_bubble_scroll_offset: 0,
            header_bubble_area: ratatui::layout::Rect::default(),
            back_button_area: ratatui::layout::Rect::default(),
            header_rendered_lines: Vec::new(),
            header_rendered_line_prefix_widths: Vec::new(),
            header_selection: None,
            header_drag_active: false,
        },
        conversation: crate::tui_render_state::ConversationListState {
            scroll_offset: app.conversation.scroll_offset,
            scroll_locked: app.conversation.scroll_locked,
            chat_area_height: app.conversation.chat_area_height,
            chat_area: chat_area_rect,
            chat_scrollbar: app.layout.chat_scrollbar,
            chat_rendered_lines: app.conversation.chat_rendered_lines,
            chat_rendered_line_prefix_widths: app.conversation.chat_rendered_line_prefix_widths,
            chat_selection: app.conversation.chat_selection,
            chat_drag_active: app.conversation.chat_drag_active,
            bubble_title_rows: app.conversation.bubble_title_rows,
            thinking_title_rows: app.conversation.thinking_title_rows,
        },
    }
}
