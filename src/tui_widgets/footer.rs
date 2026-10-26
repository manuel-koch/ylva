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
    approve_background_color, approve_hover_background_color, button_text_color,
    deny_background_color, deny_hover_background_color, footer_status_color, muted_text_color,
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
    FooterHint::key("enter", "parameters"),
    FooterHint::key("esc", "cancel"),
    FooterHint::key("F1", "help"),
];
const MODEL_PARAMETER_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("↑/↓", "parameter"),
    FooterHint::key("←/→", "adjust"),
    FooterHint::key("enter", "switch"),
    FooterHint::key("esc", "back"),
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
const SUBAGENT_VIEW_DRAFT_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("ctrl-c/esc", "clear draft"),
    FooterHint::key("F1", "help"),
];
const SUBAGENT_VIEW_BUSY_FOOTER_HINTS: &[FooterHint] = &[
    FooterHint::key("esc", "back to conversation"),
    FooterHint::key("ctrl-c", "cancel & quit"),
    FooterHint::key("F1", "help"),
];
const SUBAGENT_VIEW_IDLE_FOOTER_HINTS: &[FooterHint] = &[
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
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        } => MODEL_PARAMETER_FOOTER_HINTS,
        SlashOverlay::ModelPicker { .. } => MODEL_PICKER_FOOTER_HINTS,
        SlashOverlay::SessionPicker(picker) => {
            if picker.phase == crate::tui_reducer_state::SessionPickerPhase::Ready {
                SESSION_PICKER_FOOTER_HINTS
            } else {
                SESSION_PICKER_PENDING_FOOTER_HINTS
            }
        }
        SlashOverlay::Hidden => {
            let in_subagent_view = state.subagent_view.subagent_view_bubble.is_some();
            let parent_agent_is_busy = state.parent_agent_mode != AgentMode::Idle;
            if state.clarify_banner_state.pending_clarify.is_some() {
                CLARIFY_FOOTER_HINTS
            } else if in_subagent_view && !input_is_empty {
                // Viewing a subagent's drill-in transcript: `enter send` is
                // omitted entirely — the subagent conversation doesn't accept
                // input (only one agent loop is driven at a time), and Enter
                // no longer steers the root agent from here either.
                SUBAGENT_VIEW_DRAFT_FOOTER_HINTS
            } else if in_subagent_view {
                // Esc with an empty draft navigates back to the root
                // conversation instead of acting on the root agent's turn.
                if parent_agent_is_busy {
                    SUBAGENT_VIEW_BUSY_FOOTER_HINTS
                } else {
                    SUBAGENT_VIEW_IDLE_FOOTER_HINTS
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

/// Terminal width (in columns) below which the footer switches from the wide
/// two-row layout to the narrow three-row layout.
pub(super) const FOOTER_NARROW_WIDTH_THRESHOLD: u16 = 70;

/// Number of footer rows for a terminal of the given width.
pub(super) fn footer_height(width: u16) -> u16 {
    if width < FOOTER_NARROW_WIDTH_THRESHOLD {
        3
    } else {
        2
    }
}

/// Footer rows for one frame, split according to the available width.
pub(super) enum FooterRows {
    /// Status text and engine label share the first row; keybinding
    /// hints and context usage share the second.
    Wide { status: Rect, controls: Rect },
    /// Status text on the first row, engine label and context usage
    /// on the second, keybinding hints (or approval buttons) on the third.
    Narrow {
        status: Rect,
        meta: Rect,
        controls: Rect,
    },
}

pub(super) fn split_footer_rows(area: Rect) -> FooterRows {
    if area.width < FOOTER_NARROW_WIDTH_THRESHOLD {
        let [status, meta, controls] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
        FooterRows::Narrow {
            status,
            meta,
            controls,
        }
    } else {
        let [status, controls] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
        FooterRows::Wide { status, controls }
    }
}

/// Active engine label, with an explicit reasoning effort in
/// parentheses. Empty when no engine is active.
fn engine_label_text(state: &TuiState) -> String {
    let model_label = state.engine_display.model.clone();
    let engine_label = if state.engine_display.engine_name.is_empty() {
        model_label
    } else if model_label.is_empty() {
        state.engine_display.engine_name.clone()
    } else {
        format!("{} → {}", state.engine_display.engine_name, model_label)
    };
    if engine_label.is_empty() {
        return engine_label;
    }
    match state
        .engine_display
        .reasoning_effort
        .as_deref()
        .filter(|reasoning_effort| *reasoning_effort != "default")
    {
        Some(reasoning_effort) => format!("{engine_label} ({reasoning_effort})"),
        None => engine_label,
    }
}

/// Characters that delimit label tokens (`openrouter → anthropic/model:tag`).
/// They are never elided; the runs of other characters between them are.
const LABEL_SEPARATOR_CHARS: &[char] = &[' ', '→', '/', ':', '(', ')'];

/// Shorten `text` to at most `max_chars` characters. Separator characters are
/// kept verbatim. Each token between separators is shortened individually: the
/// widest token is shrunk one column at a time until the text fits, so the
/// budget is balanced across tokens. A shortened token keeps a head (about a
/// third of its kept characters) and a tail around a single `…`.
pub(super) fn elide_balanced_tokens(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    // Split into alternating separator and token pieces, remembering the
    // original token length and the display width each token is shrunk to.
    let mut pieces: Vec<Piece> = Vec::new();
    for character in text.chars() {
        let is_separator = LABEL_SEPARATOR_CHARS.contains(&character);
        match pieces.last_mut() {
            Some(Piece::Token { chars, .. }) if !is_separator => chars.push(character),
            _ if is_separator => pieces.push(Piece::Separator(character)),
            _ => pieces.push(Piece::Token {
                chars: vec![character],
                display_width: 0,
            }),
        }
    }
    for piece in &mut pieces {
        if let Piece::Token {
            chars,
            display_width,
        } = piece
        {
            *display_width = chars.len();
        }
    }

    let separator_width = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Separator(_)))
        .count();
    let mut total_width = separator_width
        + pieces
            .iter()
            .map(|piece| match piece {
                Piece::Token { display_width, .. } => *display_width,
                Piece::Separator(_) => 0,
            })
            .sum::<usize>();
    while total_width > max_chars {
        let widest_token = pieces
            .iter_mut()
            .filter_map(|piece| match piece {
                Piece::Token { display_width, .. } if *display_width > 1 => Some(display_width),
                _ => None,
            })
            .reduce(|widest, candidate| {
                if *candidate > *widest {
                    candidate
                } else {
                    widest
                }
            });
        let Some(widest_token) = widest_token else {
            break;
        };
        *widest_token -= 1;
        total_width -= 1;
    }
    if total_width > max_chars {
        // Separators alone do not fit: fall back to a plain head truncation.
        let head: String = text.chars().take(max_chars - 1).collect();
        return format!("{head}…");
    }

    pieces
        .into_iter()
        .map(|piece| match piece {
            Piece::Separator(character) => character.to_string(),
            Piece::Token {
                chars,
                display_width,
            } => elide_token(&chars, display_width),
        })
        .collect()
}

/// One element of an label string: a separator kept verbatim, or a token
/// that may be shortened to `display_width` columns.
enum Piece {
    Separator(char),
    Token {
        chars: Vec<char>,
        display_width: usize,
    },
}

/// Render `chars` in `display_width` columns. Full-width tokens are returned
/// unchanged; narrower ones keep a head and a tail around a single `…`.
fn elide_token(chars: &[char], display_width: usize) -> String {
    if display_width >= chars.len() {
        return chars.iter().collect();
    }
    let kept_chars = display_width.saturating_sub(1);
    let head_count = kept_chars / 3;
    let tail_count = kept_chars - head_count;
    let head: String = chars[..head_count].iter().collect();
    let tail: String = chars[chars.len() - tail_count..].iter().collect();
    format!("{head}…{tail}")
}

fn draw_footer_status(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::styled(text.to_string(), Style::default().fg(color))),
        area,
    );
}

/// Render a footer row with the status text on the left and the active
/// engine label aligned to the right.
fn draw_footer_row(
    frame: &mut Frame,
    area: Rect,
    left_text: &str,
    left_color: Color,
    state: &TuiState,
) {
    let engine_label = engine_label_text(state);
    // The status text keeps its full width; the label gets the rest of the
    // row minus one leading separator column and is elided to fit.
    let label_budget = (area.width as usize)
        .saturating_sub(left_text.chars().count())
        .saturating_sub(1);
    if engine_label.is_empty() || label_budget == 0 {
        draw_footer_status(frame, area, left_text, left_color);
        return;
    }

    let label_text = format!(" {}", elide_balanced_tokens(&engine_label, label_budget));
    let label_width = label_text.chars().count() as u16;
    let [left_area, label_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(label_width)])
        .areas(area);
    draw_footer_status(frame, left_area, left_text, left_color);
    frame.render_widget(
        Paragraph::new(Line::styled(
            label_text,
            Style::default().fg(muted_text_color()),
        ))
        .alignment(Alignment::Right),
        label_area,
    );
}

/// Join the cost total and the context usage readout (`0.12 USD · 64.2K (45%)`).
fn join_cost_and_usage(cost: Option<&str>, usage: Option<&str>) -> Option<String> {
    match (cost, usage) {
        (Some(cost), Some(usage)) => Some(format!("{cost} · {usage}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_string()),
        (None, None) => None,
    }
}

/// Narrow-layout meta row: the engine label followed by the context
/// usage, left-aligned. Renders nothing when neither is available.
fn draw_footer_meta(frame: &mut Frame, area: Rect, state: &TuiState) {
    let engine_label = engine_label_text(state);
    let usage = state.context_usage_text.as_deref();
    // One leading column precedes everything; the readout (plus its
    // separator) is reserved at the end. The label is elided to the remaining
    // width. The cost is dropped first when it does not fit.
    let suffix_width = |cost: Option<&str>| {
        join_cost_and_usage(cost, usage).map_or(0, |readout| readout.chars().count() + 1)
    };
    let cost = state
        .cost_total_text
        .as_deref()
        .filter(|cost| (area.width as usize) > 1 + suffix_width(Some(cost)));
    let label_budget = (area.width as usize).saturating_sub(1 + suffix_width(cost));
    let label = (!engine_label.is_empty() && label_budget > 0)
        .then(|| elide_balanced_tokens(&engine_label, label_budget));
    let parts: Vec<String> = label
        .into_iter()
        .chain(join_cost_and_usage(cost, usage))
        .collect();
    if parts.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" {}", parts.join(" ")),
            Style::default().fg(muted_text_color()),
        )),
        area,
    );
}

pub(super) fn draw_normal_footer(
    frame: &mut Frame,
    footer_area: Rect,
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
            .subagent_view
            .subagent_view_bubble
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
    // visible subagent and `parent_agent_mode` retains the root loop's status.
    // Once the subagent bubble reaches a terminal state (Done/Error/Denied/
    // Killed) but the root is still busy (e.g. producing its final reply or
    // running another subagent), the plain mode word would misleadingly read
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
        // The subagent bubble is still `Running` (the subagent itself hasn't
        // finished) — `state.agent_mode` reports `ToolRunning` for the whole
        // lifetime of a live subagent (see `AppState::agent_mode`), which reads
        // correctly on the *root* footer ("a tool — the subagent — is
        // running") but is misleading here: while looking at the subagent's own
        // transcript, "Running tool…" implies the *subagent itself* is
        // executing a tool call, when most of the time it's just
        // streaming/thinking. Only keep the plain "Running tool…" word when
        // the subagent's own transcript (already projected into
        // `state.chat_entries` while viewing it) shows one of *its own* tool
        // bubbles still `Running`; otherwise say explicitly whether the
        // subagent is thinking, streaming, or (before its first token
        // arrives) just generically working — `state.partial_thinking_text`/
        // `state.partial_assistant_text` are the *subagent's own* accumulators
        // here (see `render::project_for_draw`'s subagent projection), not the
        // root's.
        Some(ToolCallState::Running { .. }) => {
            let entries = source.map_or(state.chat_entries.as_slice(), |source| source.entries);
            let subagent_running_own_tool = entries.iter().any(|entry| {
                matches!(
                    entry,
                    ChatEntry::ToolBubble { state, .. } if matches!(state.as_ref(), ToolCallState::Running { .. })
                )
            });
            if subagent_running_own_tool {
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
    match split_footer_rows(footer_area) {
        FooterRows::Wide { status, controls } => {
            draw_footer_row(frame, status, &row_1_text, footer_status_color(), state);
            draw_footer_hints(
                frame,
                controls,
                footer_hints,
                state.cost_total_text.as_deref(),
                state.context_usage_text.as_deref(),
            );
        }
        FooterRows::Narrow {
            status,
            meta,
            controls,
        } => {
            draw_footer_status(frame, status, &row_1_text, footer_status_color());
            draw_footer_meta(frame, meta, state);
            draw_footer_hints(frame, controls, footer_hints, None, None);
        }
    }
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
/// readout (`64.2K (45%)`), preceded by the cost total, pinned to the right
/// end of the same row when reported. On a row too narrow to hold everything
/// without colliding, the cost is dropped first, then the readout, rather
/// than allowed to overwrite the hints.
fn draw_footer_hints(
    frame: &mut Frame,
    area: Rect,
    hints: &[FooterHint],
    cost_total_text: Option<&str>,
    context_usage_text: Option<&str>,
) {
    let hints_area = draw_footer_context_usage(frame, area, cost_total_text, context_usage_text, 9);

    let normal_style = Style::default().fg(muted_text_color());
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
    cost_total_text: Option<&str>,
    context_usage_text: Option<&str>,
    minimum_controls_width: u16,
) -> Rect {
    let combined_text =
        cost_total_text.and_then(|cost| join_cost_and_usage(Some(cost), context_usage_text));
    let fits = |text: &str| {
        let width = text.chars().count() as u16 + 1;
        area.width >= width.saturating_add(minimum_controls_width)
    };
    let readout = combined_text
        .as_deref()
        .filter(|text| fits(text))
        .or(context_usage_text.filter(|text| fits(text)));
    if let Some(usage) = readout {
        let usage_width = usage.chars().count() as u16 + 1;
        let [controls_area, usage_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(usage_width)]).areas(area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {usage}"),
                Style::default().fg(muted_text_color()),
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
    footer_area: Rect,
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
    let controls_area = match split_footer_rows(footer_area) {
        FooterRows::Wide { status, controls } => {
            draw_footer_row(frame, status, &row_1_text, footer_status_color(), state);
            draw_footer_context_usage(
                frame,
                controls,
                state.cost_total_text.as_deref(),
                state.context_usage_text.as_deref(),
                1 + approve_width + 2 + deny_width,
            )
        }
        FooterRows::Narrow {
            status,
            meta,
            controls,
        } => {
            draw_footer_status(frame, status, &row_1_text, footer_status_color());
            draw_footer_meta(frame, meta, state);
            controls
        }
    };
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
            .fg(muted_text_color())
            .add_modifier(Modifier::DIM)
    } else if approve_hovered {
        Style::default()
            .fg(button_text_color())
            .bg(approve_hover_background_color())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(button_text_color())
            .bg(approve_background_color())
    };
    let deny_style = if deny_hovered {
        Style::default()
            .fg(button_text_color())
            .bg(deny_hover_background_color())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(button_text_color())
            .bg(deny_background_color())
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
