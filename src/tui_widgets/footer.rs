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

//! Mode status, keybinding hints and approval controls.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::agent::AgentMode;
use crate::theme::{
    APPROVE_BACKGROUND_COLOR, APPROVE_HOVER_BACKGROUND_COLOR, BUTTON_TEXT_COLOR,
    DENY_BACKGROUND_COLOR, DENY_HOVER_BACKGROUND_COLOR, FOOTER_STATUS_COLOR, MUTED_TEXT_COLOR,
};
use crate::tui_reducer_state::{ActiveSubagentCounts, ChatEntry, SlashOverlay, ToolCallState};
use crate::tui_render_state::TuiState;

use super::{ButtonAreas, ConversationSource, SPINNER_FRAMES};

/// Question-mark animation frames, cycled while waiting for clarify input.
/// Each symbol is repeated so it holds for ~400 ms at a 60 ms tick rate.
const CLARIFY_FRAMES: &[char] = &[
    '?', '?', '?', '?', '?', '?', '?', '¿', '¿', '¿', '¿', '¿', '¿', '¿',
];

const SESSION_PICKER_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("↑/↓", "select"),
    FooterHint::key("←/→", "resume/clone"),
    FooterHint::key("enter", "confirm"),
    FooterHint::key("esc", "cancel"),
];

const SESSION_PICKER_PENDING_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "cancel"),
    FooterHint::key("F1", "help"),
];

pub(super) struct FooterHint {
    key: Option<&'static str>,
    description: &'static str,
}

impl FooterHint {
    const fn key(key: &'static str, description: &'static str) -> Self {
        Self {
            key: Some(key),
            description,
        }
    }

    const fn text(description: &'static str) -> Self {
        Self {
            key: None,
            description,
        }
    }
}

const SUGGESTION_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("↑/↓", "select"),
    FooterHint::key("tab", "complete"),
    FooterHint::key("enter", "run"),
    FooterHint::key("esc", "close"),
    FooterHint::key("F1", "help"),
];
const LOADING_MODELS_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::text("loading models…"),
    FooterHint::key("esc", "close"),
    FooterHint::key("F1", "help"),
];
const MODEL_PICKER_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("↑/↓", "select"),
    FooterHint::key("←/→", "effort"),
    FooterHint::key("enter", "switch"),
    FooterHint::key("esc", "cancel"),
    FooterHint::key("F1", "help"),
];
const HELP_POPUP_IDLE_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "close"),
    FooterHint::key("ctrl-c", "quit"),
    FooterHint::key("F1", "close"),
];
const HELP_POPUP_BUSY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "close"),
    FooterHint::key("ctrl-c", "cancel & quit"),
    FooterHint::key("F1", "close"),
];
const CLARIFY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("enter", "answer"),
    FooterHint::key("click", "choice"),
    FooterHint::key("ctrl-c/esc", "cancel"),
    FooterHint::key("F1", "help"),
];
const CHILD_VIEW_DRAFT_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("ctrl-c/esc", "clear draft"),
    FooterHint::key("F1", "help"),
];
const CHILD_VIEW_BUSY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "back to conversation"),
    FooterHint::key("ctrl-c", "cancel & quit"),
    FooterHint::key("F1", "help"),
];
const CHILD_VIEW_IDLE_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "back to conversation"),
    FooterHint::key("ctrl-c", "quit"),
    FooterHint::key("F1", "help"),
];
const BUSY_DRAFT_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("enter", "send"),
    FooterHint::key("ctrl-c/esc", "clear draft"),
    FooterHint::key("F1", "help"),
];
const BUSY_EMPTY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("enter", "send"),
    FooterHint::key("esc", "cancel"),
    FooterHint::key("ctrl-c", "cancel & quit"),
    FooterHint::key("F1", "help"),
];
const IDLE_DRAFT_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("enter", "send"),
    FooterHint::key("esc", "clear draft"),
    FooterHint::key("ctrl-c", "quit"),
    FooterHint::key("F1", "help"),
];
const IDLE_EMPTY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("enter", "send"),
    FooterHint::key("ctrl-c", "quit"),
    FooterHint::key("F1", "help"),
];

pub(super) fn footer_keybinding_hints(
    state: &TuiState,
    input_is_empty: bool,
) -> &'static [FooterHint] {
    match &state.slash_overlay {
        SlashOverlay::Suggestions { .. } => SUGGESTION_FOOTER_HINTS,
        SlashOverlay::LoadingModels => LOADING_MODELS_FOOTER_HINTS,
        SlashOverlay::ModelPicker { .. } => MODEL_PICKER_FOOTER_HINTS,
        SlashOverlay::SessionPicker(picker) => {
            if picker.phase == crate::tui_reducer_state::SessionPickerPhase::Ready {
                SESSION_PICKER_FOOTER_HINTS
            } else {
                SESSION_PICKER_PENDING_FOOTER_HINTS
            }
        }
        SlashOverlay::Hidden => {
            let in_child_view = state.child_view.child_view_bubble.is_some();
            let parent_agent_is_busy = state.parent_agent_mode != AgentMode::Idle;
            if state.clarify_banner_state.pending_clarify.is_some() {
                CLARIFY_FOOTER_HINTS
            } else if in_child_view && !input_is_empty {
                // Viewing a subagent's drill-in transcript: `enter send` is
                // omitted entirely — the child conversation doesn't accept
                // input (only one agent loop is driven at a time), and Enter
                // no longer steers the root agent from here either.
                CHILD_VIEW_DRAFT_FOOTER_HINTS
            } else if in_child_view {
                // Esc with an empty draft navigates back to the root
                // conversation instead of acting on the root agent's turn.
                if parent_agent_is_busy {
                    CHILD_VIEW_BUSY_FOOTER_HINTS
                } else {
                    CHILD_VIEW_IDLE_FOOTER_HINTS
                }
            } else if state.agent_mode != AgentMode::Idle && !input_is_empty {
                BUSY_DRAFT_FOOTER_HINTS
            } else if state.agent_mode != AgentMode::Idle {
                // Busy with an empty draft: Esc stops the turn, while Ctrl-C
                // stops the turn and quits the TUI.
                BUSY_EMPTY_FOOTER_HINTS
            } else if !input_is_empty {
                // Idle with a draft: Esc clears it; Ctrl-C quits on confirm.
                IDLE_DRAFT_FOOTER_HINTS
            } else {
                IDLE_EMPTY_FOOTER_HINTS
            }
        }
    }
}

pub(super) fn help_popup_footer_hints(state: &TuiState) -> &'static [FooterHint] {
    if state.parent_agent_mode == AgentMode::Idle {
        HELP_POPUP_IDLE_FOOTER_HINTS
    } else {
        HELP_POPUP_BUSY_FOOTER_HINTS
    }
}

/// Render a footer row with the status text on the left and the active
/// named-engine/model identity aligned to the right.
fn draw_footer_row(
    frame: &mut Frame,
    area: Rect,
    left_text: &str,
    left_color: Color,
    state: &TuiState,
) {
    let model_identity = state.engine_display.model.clone();
    let engine_identity = if state.engine_display.engine_name.is_empty() {
        model_identity
    } else if model_identity.is_empty() {
        state.engine_display.engine_name.clone()
    } else {
        format!("{} → {}", state.engine_display.engine_name, model_identity)
    };
    if engine_identity.is_empty() || area.width == 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                left_text.to_string(),
                Style::default().fg(left_color),
            )),
            area,
        );
        return;
    }

    let engine_identity = if !engine_identity.is_empty() {
        match state
            .engine_display
            .reasoning_effort
            .as_deref()
            .filter(|reasoning_effort| *reasoning_effort != "default")
        {
            Some(reasoning_effort) => format!("{engine_identity} ({reasoning_effort})"),
            None => engine_identity,
        }
    } else {
        engine_identity
    };
    let identity_text = format!(" {engine_identity}");
    let identity_width = (identity_text.chars().count() as u16).min(area.width);
    let [left_area, identity_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(identity_width)])
        .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            left_text.to_string(),
            Style::default().fg(left_color),
        )),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            identity_text,
            Style::default().fg(MUTED_TEXT_COLOR),
        ))
        .alignment(Alignment::Right),
        identity_area,
    );
}

pub(super) fn draw_normal_footer(
    frame: &mut Frame,
    row_1: Rect,
    row_2: Rect,
    state: &TuiState,
    footer_hints: &[FooterHint],
    hint: Option<&str>,
    source: Option<&ConversationSource<'_>>,
) {
    let thinking = source.map_or(!state.partial_thinking_text.is_empty(), |source| {
        !source.turn.partial_thinking_text.is_empty()
    });
    let responding = source.map_or(!state.partial_assistant_text.is_empty(), |source| {
        !source.turn.partial_assistant_text.is_empty()
    });
    let header_state = if let Some(source) = source {
        source.header.and_then(|(_, entry)| {
            if let ChatEntry::ToolBubble { state, .. } = entry {
                Some(state.as_ref())
            } else {
                None
            }
        })
    } else {
        state
            .child_view
            .child_view_bubble
            .as_ref()
            .map(|(_, _, state, _)| state)
    };
    let mode_word = match state.agent_mode {
        AgentMode::Idle => "Idle",
        AgentMode::Pending => "Sending…",
        AgentMode::Streaming => {
            if thinking && !responding {
                "Thinking…"
            } else {
                "Streaming…"
            }
        }
        AgentMode::ToolPending => "Awaiting approval…",
        AgentMode::ToolRunning => "Running tool…",
        AgentMode::ClarifyPending => "Waiting for answer…",
        AgentMode::Aborting => "Aborting…",
    };
    // While drilled into a subagent's transcript, `agent_mode` follows the
    // visible child and `parent_agent_mode` retains the root loop's status.
    // Once the child bubble reaches a terminal state (Done/Error/Denied/
    // Killed) but the root is still busy (e.g. producing its final reply or
    // running another child), the plain mode word would misleadingly read
    // "Idle" or "Streaming…" without saying which agent is active. Override
    // with an explicit "done, but parent busy" message in that case; once the
    // root also goes `Idle`, the plain `mode_word` above is unambiguous.
    let mode_word = match header_state {
        Some(bubble_state)
            if state.parent_agent_mode != AgentMode::Idle
                && matches!(
                    bubble_state,
                    ToolCallState::Done { .. }
                        | ToolCallState::Error { .. }
                        | ToolCallState::Denied { .. }
                        | ToolCallState::Killed
                ) =>
        {
            "Subagent done, parent working…"
        }
        // The child bubble is still `Running` (the subagent itself hasn't
        // finished) — `state.agent_mode` reports `ToolRunning` for the whole
        // lifetime of a live child (see `AppState::agent_mode`), which reads
        // correctly on the *root* footer ("a tool — the subagent — is
        // running") but is misleading here: while looking at the child's own
        // transcript, "Running tool…" implies the *child itself* is
        // executing a tool call, when most of the time it's just
        // streaming/thinking. Only keep the plain "Running tool…" word when
        // the child's own transcript (already projected into
        // `state.chat_entries` while viewing it) shows one of *its own* tool
        // bubbles still `Running`; otherwise say explicitly whether the
        // subagent is thinking, streaming, or (before its first token
        // arrives) just generically working — `state.partial_thinking_text`/
        // `state.partial_assistant_text` are the *child's own* accumulators
        // here (see `render::project_for_draw`'s child projection), not the
        // root's.
        Some(ToolCallState::Running { .. }) => {
            let entries = source.map_or(state.chat_entries.as_slice(), |source| source.entries);
            let child_running_own_tool = entries.iter().any(|entry| {
                matches!(
                    entry,
                    ChatEntry::ToolBubble { state, .. } if matches!(state.as_ref(), ToolCallState::Running { .. })
                )
            });
            if child_running_own_tool {
                "Running tool…"
            } else if thinking && !responding {
                "Subagent thinking…"
            } else if responding {
                "Subagent streaming…"
            } else {
                "Subagent working…"
            }
        }
        _ => mode_word,
    };
    let spinner_mode = if state.agent_mode == AgentMode::Idle {
        state.parent_agent_mode
    } else {
        state.agent_mode
    };
    let spinner_prefix = if spinner_mode == AgentMode::ClarifyPending {
        let index = state.spinner_frame as usize % CLARIFY_FRAMES.len();
        format!("{} ", CLARIFY_FRAMES[index])
    } else if spinner_mode != AgentMode::Idle {
        let index = state.spinner_frame as usize % SPINNER_FRAMES.len();
        format!("{} ", SPINNER_FRAMES[index])
    } else {
        String::new()
    };
    let row_1_text = if let Some(hint) = hint {
        hint.to_string()
    } else {
        let mut row_1_text = format!(" {spinner_prefix}{mode_word}");
        if state.compacting {
            row_1_text.push_str(" · Compacting…");
        }
        if let Some(active_subagent_label) =
            active_subagent_footer_label(state.active_subagent_counts)
        {
            row_1_text.push_str(" · ");
            row_1_text.push_str(&active_subagent_label);
        }
        if state.steer_indicator {
            let queued_count =
                source.map_or(state.queued_steer_texts.len(), |source| source.queued.len());
            // "Queued" is the state the user actually cares about (not yet seen
            // by the LLM); "steer" is internal jargon for how such a message
            // reaches the model, so it's kept out of user-facing text.
            let queued_label = format!(
                "{queued_count} {} queued",
                plural_noun(queued_count, "message", "messages")
            );
            row_1_text.push_str(" ⋮ ");
            row_1_text.push_str(&queued_label);
        }
        row_1_text
    };
    draw_footer_row(frame, row_1, &row_1_text, FOOTER_STATUS_COLOR, state);
    draw_footer_hints(
        frame,
        row_2,
        footer_hints,
        state.context_usage_text.as_deref(),
    );
}

fn active_subagent_footer_label(counts: ActiveSubagentCounts) -> Option<String> {
    let mut status_labels = Vec::with_capacity(2);
    if counts.running > 0 {
        status_labels.push(format!("{} running", counts.running));
    }
    if counts.queued > 0 {
        status_labels.push(format!("{} queued", counts.queued));
    }

    (!status_labels.is_empty()).then(|| format!("Subagents: {}", status_labels.join(", ")))
}

pub(super) fn plural_noun<'a>(
    count: usize,
    singular_noun: &'a str,
    plural_noun: &'a str,
) -> &'a str {
    if count == 1 {
        singular_noun
    } else {
        plural_noun
    }
}

/// Draw the keybinding hints on the footer's second row, with the model-view
/// readout (`64.2K (45%)`) pinned to the right end of the same row when one
/// has been reported. On a row too narrow to hold both without colliding,
/// the readout is dropped rather than allowed to overwrite the hints.
fn draw_footer_hints(
    frame: &mut Frame,
    area: Rect,
    hints: &[FooterHint],
    context_usage_text: Option<&str>,
) {
    let hints_area = draw_footer_context_usage(frame, area, context_usage_text, 9);

    let normal_style = Style::default().fg(MUTED_TEXT_COLOR);
    let key_style = normal_style.add_modifier(Modifier::BOLD);
    let mut spans = Vec::with_capacity(hints.len() * 4 + 1);
    spans.push(Span::styled(" ", normal_style));

    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", normal_style));
        }
        if let Some(key) = hint.key {
            spans.push(Span::styled(key, key_style));
            if !hint.description.is_empty() {
                spans.push(Span::styled(" ", normal_style));
            }
        }
        spans.push(Span::styled(hint.description, normal_style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), hints_area);
}

fn draw_footer_context_usage(
    frame: &mut Frame,
    area: Rect,
    context_usage_text: Option<&str>,
    minimum_controls_width: u16,
) -> Rect {
    if let Some(usage) = context_usage_text {
        let usage_width = usage.chars().count() as u16 + 1;
        if area.width < usage_width.saturating_add(minimum_controls_width) {
            return area;
        }
        let [controls_area, usage_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(usage_width)]).areas(area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {usage}"),
                Style::default().fg(MUTED_TEXT_COLOR),
            ))
            .alignment(Alignment::Right),
            usage_area,
        );
        return controls_area;
    }
    area
}

/// Returns the button `Rect`s for mouse hit-testing.
#[expect(clippy::too_many_arguments)]
pub(super) fn draw_approval_footer(
    frame: &mut Frame,
    row_1: Rect,
    row_2: Rect,
    state: &TuiState,
    tool_name: &str,
    is_subagent: bool,
    input_is_empty: bool,
    mouse_pos: Option<(u16, u16)>,
    hint: Option<&str>,
) -> Option<ButtonAreas> {
    let row_1_text = if let Some(hint_text) = hint {
        hint_text.to_string()
    } else if is_subagent {
        // The pending tool call belongs to a subagent, not the root agent —
        // this can be shown while viewing the root conversation (the user
        // navigated back via Esc (empty draft) while the subagent's own tool call is
        // still awaiting approval), where the pending call is otherwise
        // invisible, so make the title unambiguous about which agent it's
        // asking on behalf of.
        format!(" Subagent tool '{tool_name}' requests your approval:")
    } else {
        format!(" Tool '{tool_name}' requests your approval:")
    };
    draw_footer_row(frame, row_1, &row_1_text, FOOTER_STATUS_COLOR, state);

    // When the user is typing a denial reason, Enter will deny rather than
    // approve — reflect this by disabling the Approve button and adding
    // "(enter)" to the Deny button label.
    let approve_label = " [ ✓ Approve (enter) ] ";
    let deny_label = if input_is_empty {
        " [ ✗ Deny (esc) ] "
    } else {
        " [ ✗ Deny (enter/esc) ] "
    };
    let approve_width = approve_label.chars().count() as u16;
    let deny_width = deny_label.chars().count() as u16;
    let controls_area = draw_footer_context_usage(
        frame,
        row_2,
        state.context_usage_text.as_deref(),
        1 + approve_width + 2 + deny_width,
    );

    // Indent buttons by 1 char so they align with the leading space of the
    // approval title above.
    let [_, buttons_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Fill(1)])
        .areas(controls_area);
    let [approve_area, _, deny_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(approve_width),
            Constraint::Length(2),
            Constraint::Length(deny_width),
        ])
        .areas(buttons_area);

    let pointer = mouse_pos.map(|(col, row)| ratatui::layout::Position::new(col, row));
    let approve_hovered = pointer.is_some_and(|pos| approve_area.contains(pos));
    let deny_hovered = pointer.is_some_and(|pos| deny_area.contains(pos));

    // Approve button is disabled (greyed out, no hover) when the user has
    // typed a denial reason — pressing Enter at that point denies instead.
    let approve_style = if !input_is_empty {
        Style::default()
            .fg(MUTED_TEXT_COLOR)
            .add_modifier(Modifier::DIM)
    } else if approve_hovered {
        Style::default()
            .fg(BUTTON_TEXT_COLOR)
            .bg(APPROVE_HOVER_BACKGROUND_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(BUTTON_TEXT_COLOR)
            .bg(APPROVE_BACKGROUND_COLOR)
    };
    let deny_style = if deny_hovered {
        Style::default()
            .fg(BUTTON_TEXT_COLOR)
            .bg(DENY_HOVER_BACKGROUND_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(BUTTON_TEXT_COLOR)
            .bg(DENY_BACKGROUND_COLOR)
    };

    frame.render_widget(
        Paragraph::new(Line::styled(approve_label, approve_style)).alignment(Alignment::Left),
        approve_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(deny_label, deny_style)).alignment(Alignment::Left),
        deny_area,
    );

    Some(ButtonAreas {
        approve: approve_area,
        deny: deny_area,
    })
}

#[cfg(test)]
mod tests;
