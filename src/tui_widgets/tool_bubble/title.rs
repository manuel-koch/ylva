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

//! Tool title projection, argument layout and animated chrome.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{content_color, error_color, tool_bubble_border_color};
use crate::tui_reducer_state::ToolCallState;

use super::super::bubbles::BubbleStyle;
use super::super::SPINNER_FRAMES;
use super::ToolBubbleArgs;

/// A `key=value` result summary pair and the style it renders with.
pub(super) struct SummaryPair {
    pub(super) text: String,
    pub(super) style: Style,
}

/// The finished call's `key=value` summary pairs, rendered in the expanded
/// body's `Result:` section rather than in the title.
pub(super) fn summary_pairs(bubble: &ToolBubbleArgs<'_>) -> Vec<SummaryPair> {
    let ToolCallState::Done { summary, .. } = bubble.state else {
        return vec![];
    };
    let is_subagent = bubble.name == crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME;
    let dim_white = Style::default()
        .fg(content_color())
        .add_modifier(Modifier::DIM);
    let failure_status = Style::default()
        .fg(error_color())
        .add_modifier(Modifier::BOLD);
    summary
        .iter()
        .map(|(key, value)| SummaryPair {
            text: format!(
                "{key}={}",
                if bubble.path_like_arguments.contains(&key.as_str()) {
                    crate::home_relative_path::display_path(std::path::Path::new(value))
                } else {
                    value.clone()
                }
            ),
            style: if is_subagent && key == "status" && value == "failure" {
                failure_status
            } else {
                dim_white
            },
        })
        .collect()
}

pub(super) fn append_tool_chrome(
    lines: &mut Vec<Line<'static>>,
    bubble: ToolBubbleArgs<'_>,
    style: &BubbleStyle,
    wrap_width: usize,
) -> Vec<crate::agent::tools::Section> {
    let ToolBubbleArgs {
        id: _,
        name,
        state,
        collapsed,
        spinner_frame,
        elapsed_seconds,
        display_title_override,
        subtype_argument,
        path_like_arguments,
    } = bubble;
    let [bar, margin] = style.prefix_spans();

    // Format arguments for the title:
    // - primary arg (schema rank 0): value only, rendered in bold.
    // - remaining args: `key=value` pairs, rendered dim.
    // - body arguments (declared by the tool) are omitted from the title.
    // `Running`'s interim `progress_arguments` (e.g. `spawn_subagent`
    // reporting `status=running`/`queued` + `run_id`) are combined with the
    // call's real `title_arguments` — kept as a separate owned buffer here
    // since `Done` has no equivalent field, so once the call truly finishes
    // these interim values are naturally gone rather than lingering stale.
    let combined_title_arguments: Vec<(String, serde_json::Value)> = match state {
        ToolCallState::Running {
            title_arguments,
            progress_arguments,
            ..
        } => title_arguments
            .iter()
            .chain(progress_arguments.iter())
            .cloned()
            .collect(),
        ToolCallState::Pending {
            title_arguments, ..
        }
        | ToolCallState::Done {
            title_arguments, ..
        }
        | ToolCallState::Denied {
            title_arguments, ..
        }
        | ToolCallState::Error {
            title_arguments, ..
        } => title_arguments.clone(),
        _ => vec![],
    };
    let is_subagent = name == crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME;
    let is_queued_subagent = is_subagent
        && combined_title_arguments
            .iter()
            .any(|(key, value)| key == "status" && value == "queued");
    // Keep the subagent status and run identifier in state for rendering and
    // routing, but do not expose these internal values in the title.
    let title_arguments: Vec<(String, serde_json::Value)> = combined_title_arguments
        .into_iter()
        .filter(|(key, _)| !is_subagent || (key != "run_id" && key != "status"))
        .collect();
    // The tool's own `Tool::title()` label when the registry knows it (e.g.
    // `spawn_subagent` → `Spawn Subagent`, `web_search` → `Web Search
    // (Brave)`), otherwise the title-cased tool name (see
    // `ToolBubbleArgs::display_title_override`), then folded with the call's
    // per-call subtype — `Spawn Subagent (local-discovery)` — which also
    // leaves the props.
    let base_title = display_title_override
        .map(str::to_string)
        .unwrap_or_else(|| crate::agent::tools::title_case_tool_name(name));
    let (display_title, title_arguments) = crate::agent::tools::display_title_with_subtype(
        &base_title,
        subtype_argument,
        &title_arguments,
    );
    let title_arguments = title_arguments.as_slice();
    let display_value = |key: &str, value: String| {
        if path_like_arguments.contains(&key) {
            crate::home_relative_path::display_path(std::path::Path::new(&value))
        } else {
            value
        }
    };
    // Only strings and numbers qualify as the bare (keyless) primary arg.
    // Booleans need their key name to be meaningful, so they stay in props.
    let scalar_primary = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let scalar_prop_value = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    // Multiline scalar values (e.g. edit's `old_string`/`new_string`) would
    // otherwise embed raw newlines into the single-line title/args area,
    // breaking its layout. Replace them with a visible placeholder so the
    // title stays single-line, and stash the untouched value as its own
    // expandable content section so the full text remains readable.
    let sanitize_inline = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '\n' | '\r' => '↵',
                '\t' => ' ',
                c => c,
            })
            .collect()
    };
    let mut extra_content_sections: Vec<crate::agent::tools::Section> = vec![];
    let mut multiline_section = |key: &str, value: &str| {
        extra_content_sections.push(crate::agent::tools::Section::Content(
            crate::agent::tools::ContentSection {
                label: Some(key.to_string()),
                content: value.to_string(),
                file_extension: None,
                line_number_offset: None,
            },
        ));
    };
    let mut iter = title_arguments.iter().peekable();
    // Consume the first entry only if it is a bare-worthy scalar.
    let primary_text: Option<String> =
        iter.next_if(|(_, v)| scalar_primary(v).is_some())
            .and_then(|(k, v)| {
                scalar_primary(v).map(|s| {
                    let s = display_value(k, s);
                    if s.contains('\n') {
                        multiline_section(k, &s);
                        sanitize_inline(&s)
                    } else {
                        s
                    }
                })
            });
    let property_lines: Vec<String> = iter
        .filter_map(|(k, v)| {
            scalar_prop_value(v).map(|value| {
                let value = display_value(k, value);
                let display_value = if value.contains('\n') {
                    multiline_section(k, &value);
                    sanitize_inline(&value)
                } else {
                    value
                };
                format!("{k}={display_value}")
            })
        })
        .collect();

    let content_sections = bubble.content_sections(&extra_content_sections);

    // The expanded arguments are redundant when the body sections already
    // carry every title argument (bash: the `Label`/`Command` sections cover
    // `label`/`command`), so it is skipped; the collapsed title line always
    // keeps the args. Data-driven by section labels — tools whose sections
    // don't cover their title keys (`edit`, `write`, `spawn_subagent`) keep
    // rendering the arguments as before, as do old persisted bubbles whose
    // sections predate this rule.
    let content_section_labels: Vec<String> = content_sections
        .iter()
        .filter_map(|section| match section {
            crate::agent::tools::Section::Content(content) => {
                content.label.as_deref().map(str::to_lowercase)
            }
            _ => None,
        })
        .collect();
    let args_covered_by_sections = !content_sections.is_empty()
        && title_arguments
            .iter()
            .all(|(key, _)| content_section_labels.contains(&key.to_lowercase()));

    let dim_white = Style::default()
        .fg(content_color())
        .add_modifier(Modifier::DIM);

    // The duration is `time_text`, right-aligned in the title row; the
    // `key=value` summary pairs render in the expanded `Result:` section.
    let has_summary_pairs = !summary_pairs(&bubble).is_empty();
    let time_text: String = match state {
        ToolCallState::Running { .. }
            if (is_subagent && !is_queued_subagent)
                || name == crate::agent::tools::bash::BashTool::NAME =>
        {
            elapsed_seconds
                .map(|elapsed_seconds| format!("{elapsed_seconds}s"))
                .unwrap_or_default()
        }
        ToolCallState::Done { duration, .. } => format!("{:.3}s", duration.as_secs_f64()),
        ToolCallState::Error { duration, .. } if duration.as_nanos() > 0 => {
            format!("{:.3}s", duration.as_secs_f64())
        }
        _ => String::new(),
    };
    // The optional tool label (e.g. bash's `label`) accompanies the primary
    // argument in the collapsed title; it is never the primary itself.
    let label_text: Option<String> = title_arguments
        .iter()
        .skip(usize::from(primary_text.is_some()))
        .find(|(key, _)| key == "label")
        .and_then(|(_, value)| scalar_prop_value(value))
        .map(|value| sanitize_inline(&value))
        .filter(|value| !value.is_empty());

    let has_args =
        primary_text.as_ref().is_some_and(|s| !s.is_empty()) || !property_lines.is_empty();
    let has_expandable_body = !content_sections.is_empty()
        || matches!(state, ToolCallState::Done { result_section, .. } if !result_section.content.is_empty())
        || has_args
        || has_summary_pairs;
    let expand_indicator = if has_expandable_body {
        if collapsed {
            "▶"
        } else {
            "▼"
        }
    } else {
        " "
    };

    // Build the indicator + tool-name prefix.
    let title_prefix = format!("{expand_indicator} {display_title}");

    // ── Title rendering ─────────────────────────────────────────────────────
    // Two display modes controlled by `collapsed`:
    //
    //   Collapsed — single line: `{prefix} {primary_arg} {label}  {time}`
    //     The time (running or final duration) is right-aligned. If the left
    //     part exceeds the width left of the time, it is truncated and
    //     terminated with "…" (dim white) so the time always stays visible.
    //
    //   Expanded — sections:
    //     Line 1: `{prefix}  {time}`, prefix in border colour, time right-aligned.
    //     Next lines: the primary arg (bold white), then, under an underlined
    //             `Arguments:` label, the remaining args one `key=value` per
    //             line (dim white), each width-wrapped at `wrap_width - 2`
    //             with a two-space indent. Only when present and not already
    //             fully covered by body sections.
    //   Result summary pairs and tool output render in the body, see `body`.

    let prefix_char_len = title_prefix.chars().count();

    let primary_content_len: usize = primary_text
        .as_ref()
        .map_or(0, |primary| primary.chars().count());

    let bold_white = Style::default()
        .fg(content_color())
        .add_modifier(Modifier::BOLD);

    let spinner_or_margin: Span = if is_queued_subagent {
        // Queued (not yet actually running): a static glyph, not the
        // animated spinner, since nothing is actively executing yet — the
        // subagent is only waiting for the currently running one to finish.
        Span::styled("≡ ", Style::default().fg(tool_bubble_border_color()))
    } else if matches!(state, ToolCallState::Running { .. }) {
        let spinner_char = SPINNER_FRAMES[spinner_frame as usize % SPINNER_FRAMES.len()];
        Span::styled(
            format!("{spinner_char} "),
            Style::default().fg(tool_bubble_border_color()),
        )
    } else {
        margin.clone()
    };

    // Helper: emit one title row with per-region char styling.
    // `line_text` is the display string for the prefix+args portion of the row.
    // `prefix_end` and `primary_end` are char-index boundaries within `line_text`.
    // `truncated` appends a dim TRUNCATION_MARKER when the args were truncated.
    // The time is always rendered right-aligned after the optional marker so it
    // stays visible even when args are truncated.
    const TRUNCATION_MARKER: &str = "…";
    let time_char_len = time_text.chars().count();
    let push_time_aligned = |spans: &mut Vec<Span<'static>>, used_chars: usize| {
        if time_char_len == 0 {
            return;
        }
        let padding = wrap_width.saturating_sub(used_chars + time_char_len).max(1);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(Span::styled(time_text.clone(), dim_white));
    };
    let emit_styled_line = |lines: &mut Vec<Line>,
                            line_text: &str,
                            prefix_end: usize,
                            primary_end: usize,
                            truncated: bool| {
        let chars: Vec<char> = line_text.chars().collect();
        let total = chars.len();
        let pe = prefix_end.min(total);
        let ae = primary_end.min(total);
        let mut spans: Vec<Span> = vec![bar.clone(), spinner_or_margin.clone()];
        if pe > 0 {
            spans.push(Span::styled(
                chars[..pe].iter().collect::<String>(),
                Style::default().fg(tool_bubble_border_color()),
            ));
        }
        if ae > pe {
            spans.push(Span::styled(
                chars[pe..ae].iter().collect::<String>(),
                bold_white,
            ));
        }
        if total > ae {
            spans.push(Span::styled(
                chars[ae..].iter().collect::<String>(),
                dim_white,
            ));
        }
        if truncated {
            spans.push(Span::styled(TRUNCATION_MARKER, dim_white));
        }
        let used_chars = total + usize::from(truncated);
        push_time_aligned(&mut spans, used_chars);
        lines.push(Line::from(spans));
    };

    if collapsed {
        // ── Collapsed: single line, args truncated to always show the time ──
        // Only the primary argument and optional label are shown. Sanitize
        // control characters that would break single-line rendering: newlines
        // and tabs in the bash command must become visible inline glyphs.
        let mut collapsed_args = String::new();
        if let Some(primary) = primary_text.as_ref().filter(|primary| !primary.is_empty()) {
            collapsed_args.push(' ');
            collapsed_args.push_str(primary);
        }
        if let Some(label) = &label_text {
            collapsed_args.push(' ');
            collapsed_args.push_str(label);
        }
        let sanitized_args: String = collapsed_args
            .chars()
            .map(|c| match c {
                '\n' => '↵',
                '\r' => '↵',
                '\t' => ' ',
                c => c,
            })
            .collect();
        let sanitized_args_chars: Vec<char> = sanitized_args.chars().collect();

        // Reserve space (plus one gap column) for the right-aligned time.
        // One extra column is needed when truncating to leave room for
        // TRUNCATION_MARKER.
        let time_reserved = if time_char_len > 0 {
            time_char_len + 1
        } else {
            0
        };
        let available_for_args = wrap_width
            .saturating_sub(prefix_char_len)
            .saturating_sub(time_reserved);
        let args_fit = sanitized_args_chars.len() <= available_for_args;
        let truncated = !args_fit;
        let display_args_len = if truncated {
            available_for_args.saturating_sub(TRUNCATION_MARKER.chars().count())
        } else {
            sanitized_args_chars.len()
        };
        let raw_display_args: String = sanitized_args_chars[..display_args_len].iter().collect();
        // A cut that lands on a space would render `word …` — drop the
        // trailing whitespace so the marker hugs the last visible character.
        let display_args = raw_display_args.trim_end();
        let display_text = format!("{title_prefix}{display_args}");

        // Region boundaries (prefix → border colour, primary arg → bold, rest → dim).
        let display_text_len = display_text.chars().count();
        let prefix_end = prefix_char_len.min(display_text_len);
        // Primary arg starts right after the leading space that `args_after_prefix` prepends.
        let primary_start = prefix_char_len + 1; // skip the space
        let primary_end = if primary_content_len > 0 {
            (primary_start + primary_content_len).min(display_text_len)
        } else {
            prefix_end
        };
        emit_styled_line(lines, &display_text, prefix_end, primary_end, truncated);
    } else {
        // ── Expanded: line 1 = prefix + right-aligned time; then args, summary ──

        // Line 1: prefix in border colour, time right-aligned in dim white.
        let mut first_spans: Vec<Span<'static>> = vec![
            bar.clone(),
            spinner_or_margin.clone(),
            Span::styled(
                title_prefix.clone(),
                Style::default().fg(tool_bubble_border_color()),
            ),
        ];
        push_time_aligned(&mut first_spans, prefix_char_len);
        lines.push(Line::from(first_spans));

        // Lines 2+: the primary argument, then the remaining arguments one
        // `key=value` per line under an underlined `Arguments:` label. Skipped
        // when the body sections already carry every title argument (see
        // `args_covered_by_sections` above).
        if has_args && !args_covered_by_sections {
            let arg_wrap_width = wrap_width.saturating_sub(2).max(1);
            let indented_line = |text: String, text_style: Style| {
                Line::from(vec![
                    bar.clone(),
                    margin.clone(),
                    Span::styled("  ", Style::default()),
                    Span::styled(text, text_style),
                ])
            };
            if let Some(primary) = primary_text.as_ref().filter(|primary| !primary.is_empty()) {
                for chunk in textwrap::wrap(primary, arg_wrap_width) {
                    lines.push(indented_line(chunk.into_owned(), bold_white));
                }
            }
            if !property_lines.is_empty() {
                lines.push(Line::from(vec![bar.clone(), margin.clone()]));
                super::body::push_section_label(lines, style, "Arguments:");
                for property in &property_lines {
                    for chunk in textwrap::wrap(property, arg_wrap_width) {
                        lines.push(indented_line(chunk.into_owned(), dim_white));
                    }
                }
            }
        }
    }
    // Error/Denied second title line (shown even when collapsed).
    let maybe_error = match state {
        ToolCallState::Error { error, .. } | ToolCallState::Denied { error, .. } => Some(error),
        _ => None,
    };
    if let Some(error) = maybe_error {
        // "  " prefix (2 chars) inside the bar+margin prefix → wrap text tighter.
        let error_wrap_width = wrap_width.saturating_sub(2).max(1);
        for wrapped in textwrap::wrap(error, error_wrap_width) {
            let error_label = format!("  {wrapped}");
            lines.push(Line::from(vec![
                bar.clone(),
                margin.clone(),
                Span::styled(error_label, Style::default().fg(error_color())),
            ]));
        }
    }

    extra_content_sections
}
