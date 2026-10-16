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

use crate::theme::{CONTENT_COLOR, ERROR_COLOR, TOOL_BUBBLE_BORDER_COLOR};
use crate::tui_reducer_state::ToolCallState;

use super::super::bubbles::BubbleStyle;
use super::super::SPINNER_FRAMES;
use super::ToolBubbleArgs;

struct SummarySegment {
    text: String,
    style: Style,
}

fn summary_spans(segments: &[SummarySegment]) -> Vec<Span<'static>> {
    segments
        .iter()
        .map(|segment| Span::styled(segment.text.clone(), segment.style))
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
    // Keep the child status and run identifier in state for rendering and
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
    let props_text: String = iter
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
        .collect::<Vec<_>>()
        .join(" ");

    let content_sections = bubble.content_sections(&extra_content_sections);

    // The expanded args line is redundant when the body sections already
    // carry every title argument (bash: the `Label`/`Command` sections cover
    // `label`/`command`), so it is skipped; the collapsed title line always
    // keeps the args. Data-driven by section labels — tools whose sections
    // don't cover their title keys (`edit`, `write`, `spawn_subagent`) keep
    // rendering the args line as before, as do old persisted bubbles whose
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

    let has_args = primary_text.as_ref().is_some_and(|s| !s.is_empty()) || !props_text.is_empty();
    let has_expandable_body = !content_sections.is_empty()
        || matches!(state, ToolCallState::Done { result_section, .. } if !result_section.content.is_empty())
        || has_args;
    let expand_indicator = if has_expandable_body {
        if collapsed {
            "▶"
        } else {
            "▼"
        }
    } else {
        " "
    };

    let tool_ran = matches!(state, ToolCallState::Done { .. });
    let dim_white = Style::default()
        .fg(CONTENT_COLOR)
        .add_modifier(Modifier::DIM);
    let failure_status = Style::default()
        .fg(ERROR_COLOR)
        .add_modifier(Modifier::BOLD);

    // The caller assigns each segment's style explicitly so summary
    // rendering does not need to infer special text from the final string.
    let mut summary_segments: Vec<SummarySegment> = vec![];
    let summary_text: String = match state {
        ToolCallState::Running { .. } if is_subagent && !is_queued_subagent => {
            if let Some(elapsed_seconds) = elapsed_seconds {
                let duration_formatted = format!("{elapsed_seconds}s");
                summary_segments.push(SummarySegment {
                    text: duration_formatted.clone(),
                    style: dim_white,
                });
                duration_formatted
            } else {
                String::new()
            }
        }
        ToolCallState::Done {
            summary, duration, ..
        } => {
            let duration_seconds = duration.as_secs_f64();
            let duration_formatted = format!("{duration_seconds:.3}s");
            let mut parts: Vec<String> = summary
                .iter()
                .map(|(key, value)| format!("{key}={}", display_value(key, value.clone())))
                .collect();
            parts.push(duration_formatted);
            for (index, (key, value)) in summary.iter().enumerate() {
                if index > 0 {
                    summary_segments.push(SummarySegment {
                        text: " ".to_string(),
                        style: dim_white,
                    });
                }
                summary_segments.push(SummarySegment {
                    text: format!("{key}={}", display_value(key, value.clone())),
                    style: if is_subagent && key == "status" && value == "failure" {
                        failure_status
                    } else {
                        dim_white
                    },
                });
            }
            if !summary.is_empty() {
                summary_segments.push(SummarySegment {
                    text: " ".to_string(),
                    style: dim_white,
                });
            }
            summary_segments.push(SummarySegment {
                text: format!("{duration_seconds:.3}s"),
                style: dim_white,
            });
            parts.join(" ")
        }
        ToolCallState::Error { duration, .. } if duration.as_nanos() > 0 => {
            summary_segments.push(SummarySegment {
                text: format!("{:.3}s", duration.as_secs_f64()),
                style: dim_white,
            });
            format!("{:.3}s", duration.as_secs_f64())
        }
        _ => String::new(),
    };

    // Build the indicator + tool-name prefix.
    let title_prefix = format!("{expand_indicator} {display_title}");

    // ── Title rendering ─────────────────────────────────────────────────────
    // Two display modes controlled by `collapsed`:
    //
    //   Collapsed — single line: `{prefix} {primary_arg} {props} [→ {summary}]`
    //     If the full text exceeds `wrap_width`, the line is truncated and
    //     terminated with "…" (dim white) so the bubble always fits in the
    //     terminal width. Region styling still applies to the visible chars.
    //
    //   Expanded — two sections:
    //     Line 1: `{prefix} [→ {summary}]`
    //             prefix in border colour, summary in dim white.
    //     Lines 2+: args (`{primary_arg} {props}`) only when args are present
    //             and not already fully covered by body sections.
    //             Hard-broken on `\n`, then width-wrapped at `wrap_width - 2`
    //             with a two-space indent. Primary arg chars → bold white;
    //             props chars → dim white.

    let prefix_char_len = title_prefix.chars().count();

    // ── Build the args portion (shared between modes) ──────────────────────
    // `args_after_prefix` starts with a space when non-empty.
    let mut args_after_prefix = String::new();
    let primary_content_len: usize; // chars in the primary arg value (no leading space)
    if let Some(ref primary) = primary_text {
        if !primary.is_empty() {
            args_after_prefix.push(' ');
            args_after_prefix.push_str(primary);
            primary_content_len = primary.chars().count();
        } else {
            primary_content_len = 0;
        }
    } else {
        primary_content_len = 0;
    }
    if !props_text.is_empty() {
        args_after_prefix.push(' ');
        args_after_prefix.push_str(&props_text);
    }

    // Summary suffix, used in both modes.
    let summary_prefix = if has_args && tool_ran { " → " } else { " " };
    if !summary_text.is_empty() {
        summary_segments.insert(
            0,
            SummarySegment {
                text: summary_prefix.to_string(),
                style: dim_white,
            },
        );
    }
    let summary_suffix: String = if !summary_text.is_empty() {
        if has_args && tool_ran {
            format!(" → {summary_text}")
        } else {
            format!(" {summary_text}")
        }
    } else {
        String::new()
    };

    let bold_white = Style::default()
        .fg(CONTENT_COLOR)
        .add_modifier(Modifier::BOLD);

    let spinner_or_margin: Span = if is_queued_subagent {
        // Queued (not yet actually running): a static glyph, not the
        // animated spinner, since nothing is actively executing yet — the
        // child is only waiting for the currently running one to finish.
        Span::styled("≡ ", Style::default().fg(TOOL_BUBBLE_BORDER_COLOR))
    } else if matches!(state, ToolCallState::Running { .. }) {
        let spinner_char = SPINNER_FRAMES[spinner_frame as usize % SPINNER_FRAMES.len()];
        Span::styled(
            format!("{spinner_char} "),
            Style::default().fg(TOOL_BUBBLE_BORDER_COLOR),
        )
    } else {
        margin.clone()
    };

    // Helper: emit one title row with per-region char styling.
    // `line_text` is the display string for the prefix+args portion of the row.
    // `prefix_end` and `primary_end` are char-index boundaries within `line_text`.
    // `truncated` appends a dim TRUNCATION_MARKER when the args were truncated.
    // Summary segments are always rendered after the optional marker so the
    // summary / duration stays visible even when args are truncated.
    const TRUNCATION_MARKER: &str = "…";
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
                Style::default().fg(TOOL_BUBBLE_BORDER_COLOR),
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
        if !summary_segments.is_empty() {
            spans.extend(summary_spans(&summary_segments));
        }
        lines.push(Line::from(spans));
    };

    if collapsed {
        // ── Collapsed: single line, args truncated to always show summary ──
        // Sanitize control characters that would break single-line rendering:
        // newlines and tabs in the bash command must become visible inline glyphs.
        let sanitized_args: String = args_after_prefix
            .chars()
            .map(|c| match c {
                '\n' => '↵',
                '\r' => '↵',
                '\t' => ' ',
                c => c,
            })
            .collect();
        let sanitized_args_chars: Vec<char> = sanitized_args.chars().collect();
        let summary_char_len = summary_suffix.chars().count();

        // Reserve space for the summary suffix so it is always visible.
        // The available columns for the args portion are: total width minus
        // the prefix and the summary. One extra column is needed when
        // truncating to leave room for TRUNCATION_MARKER.
        let available_for_args = wrap_width
            .saturating_sub(prefix_char_len)
            .saturating_sub(summary_char_len);
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
        // ── Expanded: line 1 = prefix + summary; lines 2+ = args ──────────

        // Line 1: prefix in border colour, summary in dim white.
        let first_line = format!("{title_prefix}{summary_suffix}");
        let first_chars: Vec<char> = first_line.chars().collect();
        let first_total = first_chars.len();
        let mut first_spans: Vec<Span> = vec![bar.clone(), spinner_or_margin.clone()];
        let pe = prefix_char_len.min(first_total);
        if pe > 0 {
            first_spans.push(Span::styled(
                first_chars[..pe].iter().collect::<String>(),
                Style::default().fg(TOOL_BUBBLE_BORDER_COLOR),
            ));
        }
        if first_total > pe {
            first_spans.extend(summary_spans(&summary_segments));
        }
        lines.push(Line::from(first_spans));

        // Lines 2+: args, hard-broken then width-wrapped. Skipped when the
        // body sections already carry every title argument (see
        // `args_covered_by_sections` above).
        if !args_after_prefix.is_empty() && !args_covered_by_sections {
            // Strip the leading space that `args_after_prefix` carries.
            let args_text = args_after_prefix.trim_start_matches(' ');
            let arg_wrap_width = wrap_width.saturating_sub(2).max(1);
            let hard_segments: Vec<&str> = args_text.split('\n').collect();
            let mut arg_lines: Vec<String> = Vec::new();
            for segment in &hard_segments {
                for chunk in textwrap::wrap(segment, arg_wrap_width) {
                    arg_lines.push(chunk.to_string());
                }
            }

            // Track char offset within `args_text` to preserve bold/dim boundary.
            let mut chars_consumed: usize = 0;
            for arg_line in &arg_lines {
                let chunk_chars: Vec<char> = arg_line.chars().collect();
                let chunk_len = chunk_chars.len();

                // Primary region: first `primary_content_len` chars of `args_text`.
                let primary_end_in_chunk = primary_content_len
                    .saturating_sub(chars_consumed)
                    .min(chunk_len);

                let indent = "  ";

                let mut cont_spans: Vec<Span> = vec![bar.clone(), margin.clone()];
                cont_spans.push(Span::styled(indent, Style::default()));
                if primary_end_in_chunk > 0 {
                    cont_spans.push(Span::styled(
                        chunk_chars[..primary_end_in_chunk]
                            .iter()
                            .collect::<String>(),
                        bold_white,
                    ));
                }
                if chunk_len > primary_end_in_chunk {
                    cont_spans.push(Span::styled(
                        chunk_chars[primary_end_in_chunk..]
                            .iter()
                            .collect::<String>(),
                        dim_white,
                    ));
                }
                lines.push(Line::from(cont_spans));

                chars_consumed += chunk_len;
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
                Span::styled(error_label, Style::default().fg(ERROR_COLOR)),
            ]));
        }
    }

    extra_content_sections
}
