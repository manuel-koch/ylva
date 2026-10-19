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

//! Headless tool-invocation rendering: titles, inline/bracketed arguments and
//! content sections.

use std::io::Write;

#[cfg(test)]
use crate::agent::tools::Tool;
use crate::ToolRegistry;

pub(super) fn capitalize_first(s: &str) -> String {
    crate::agent::tools::title_case_tool_name(s)
}

/// The display title for `tool_name` together with the title arguments that
/// still need rendering: the tool's
/// [`subtype_argument`](Tool::subtype_argument) is folded into the title as
/// `({value})` and dropped from the props (via
/// [`crate::agent::tools::display_title_with_subtype`]), so the call's
/// variant reads as part of the label — `Spawn Subagent (local-discovery) …`
/// — instead of as an `agent_type=…` prop. Falls back to the title-cased
/// tool name for a tool the registry does not know.
pub(super) fn display_title_for(
    registry: &ToolRegistry,
    tool_name: &str,
    title_arguments: &[(String, serde_json::Value)],
) -> (String, Vec<(String, serde_json::Value)>) {
    match registry.get(tool_name) {
        Some(tool) => crate::agent::tools::display_title_with_subtype(
            &tool.title(),
            tool.subtype_argument(),
            title_arguments,
        ),
        None => (capitalize_first(tool_name), title_arguments.to_vec()),
    }
}

pub(super) fn render_invocation_from_title_arguments(
    title: &str,
    title_arguments: &[(String, serde_json::Value)],
) -> String {
    render_invocation_with_display_limits(
        title,
        title_arguments,
        PRIMARY_ARG_DISPLAY_LIMIT,
        STRING_ELIDE_LIMIT,
    )
}

/// Render one tool status line.
///
/// The first-ranked scalar argument is the call's *primary* — its payload
/// (`terms`, `command`, `path`) — and is shown inline, truncated at
/// `primary_display_limit`. Every remaining argument becomes a bracketed
/// `key=value` prop truncated at `prop_display_limit`. Both are truncated
/// with a trailing `…`, never replaced by a length placeholder: the line
/// stays a single line without losing the head of what actually ran.
pub(super) fn render_invocation_with_display_limits(
    title: &str,
    title_arguments: &[(String, serde_json::Value)],
    primary_display_limit: usize,
    prop_display_limit: usize,
) -> String {
    let mut line = title.to_string();
    let scalar_primary = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    };
    let mut iter = title_arguments.iter().peekable();
    // Consume the first entry only if it is a bare-worthy scalar (not a boolean).
    if let Some(primary) = iter
        .next_if(|(_, v)| scalar_primary(v).is_some())
        .and_then(|(_, v)| scalar_primary(v))
    {
        let primary = truncate_with_ellipsis(&primary, primary_display_limit);
        line.push(' ');
        // A literal identifier (path, glob pattern, regex, ...) reads fine
        // bare even after a multi-word title, since it visually contrasts
        // with the surrounding words. Free-form prose doesn't — e.g.
        // `spawn_subagent`'s "Spawn Subagent" title followed by a bare
        // multi-word `label` blurs into one run-on phrase. Quote only when
        // *both* the title and the primary value are multi-word, so the
        // quotes appear exactly where that ambiguity exists.
        let is_multi_word = |s: &str| s.split_whitespace().count() > 1;
        if is_multi_word(title) && is_multi_word(&primary) {
            line.push_str(&format!("\"{primary}\""));
        } else {
            line.push_str(&primary);
        }
    }
    let rest: Vec<String> = iter
        .map(|(k, v)| format!("{k}={}", render_arg_value_with_limit(v, prop_display_limit)))
        .collect();
    if !rest.is_empty() {
        line.push_str(&format!(" [{}]", rest.join(", ")));
    }
    line
}

/// Render a tool invocation for a denial line: every string argument —
/// primary and prop alike — is truncated at [`DENIED_ARG_DISPLAY_LIMIT`] with
/// a trailing `…` instead of being dropped, so the denied command is always
/// visible.
pub(super) fn render_denied_invocation(
    title: &str,
    title_arguments: &[(String, serde_json::Value)],
) -> String {
    render_invocation_with_display_limits(
        title,
        title_arguments,
        DENIED_ARG_DISPLAY_LIMIT,
        DENIED_ARG_DISPLAY_LIMIT,
    )
}

pub(super) fn render_content_sections(
    out: &mut dyn Write,
    sections: &[crate::agent::tools::Section],
) -> anyhow::Result<()> {
    for section in sections {
        match section {
            crate::agent::tools::Section::Content(content_section) => {
                if let Some(label) = &content_section.label {
                    writeln!(out, "  --- {label} ---")?;
                }
                let line_count = content_section.content.lines().count();
                let last_line_num = content_section.line_number_offset.unwrap_or(0) + line_count;
                let digits = last_line_num.to_string().len();
                for (i, line) in content_section.content.lines().enumerate() {
                    if let Some(offset) = content_section.line_number_offset {
                        write!(out, "  {:>digits$}: ", offset + i + 1)?;
                    } else {
                        write!(out, "  ")?;
                    }
                    writeln!(out, "{line}")?;
                }
            }
            crate::agent::tools::Section::Diff(diff_section) => {
                if let Some(label) = &diff_section.label {
                    writeln!(out, "  --- {label} ---")?;
                }
                for line in diff_section.content.lines() {
                    writeln!(out, "  {line}")?;
                }
            }
            crate::agent::tools::Section::SearchResult(search_section) => {
                writeln!(out, "  --- {} ---", search_section.file_path)?;
                let gutter_width = search_section
                    .lines
                    .iter()
                    .map(|l| l.line_number)
                    .max()
                    .map(|n| n.to_string().len())
                    .unwrap_or(1);
                for line in &search_section.lines {
                    if line.is_match {
                        writeln!(
                            out,
                            "  {:>gutter_width$}: {}",
                            line.line_number, line.content
                        )?;
                    } else {
                        writeln!(
                            out,
                            "  {:>gutter_width$}- {}",
                            line.line_number, line.content
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Byte budget for a bracketed `key=value` prop on a normal tool status
/// line: past it the value is truncated with a trailing `…` (never replaced
/// by a length placeholder), so the line stays one line but still shows what
/// the argument starts with.
pub(super) const STRING_ELIDE_LIMIT: usize = 40;

/// Byte budget for the inline *primary* (first-ranked) argument of a normal
/// tool status line — the call's payload (`terms`, `command`, `path`).
/// Deliberately as generous as [`DENIED_ARG_DISPLAY_LIMIT`]: a payload the
/// user is about to approve deserves at least the visibility of one already
/// denied. Past it the value is truncated with a trailing `…`.
pub(super) const PRIMARY_ARG_DISPLAY_LIMIT: usize = 80;

/// Byte length beyond which a string argument on a CLI denial line is
/// truncated with a trailing `…`. The denied command must always stay
/// visible, but the line must not flood with multi-hundred-byte commands.
pub(super) const DENIED_ARG_DISPLAY_LIMIT: usize = 80;

/// Truncate `text` to at most `max_bytes` UTF-8 bytes (counting whole
/// characters only), appending `…` when truncation occurs.
pub(super) fn truncate_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const ELLIPSIS: &str = "…";
    let byte_budget = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut truncated = String::new();
    for character in text.chars() {
        if truncated.len() + character.len_utf8() > byte_budget {
            break;
        }
        truncated.push(character);
    }
    // A cut that lands on a space would render `word …` — drop the trailing
    // whitespace so the marker hugs the last visible character.
    let trimmed_len = truncated.trim_end().len();
    truncated.truncate(trimmed_len);
    truncated.push_str(ELLIPSIS);
    truncated
}

/// Render a tool invocation for the CLI status line, e.g.
/// `Read docs/features.md [limit=32]`.
///
/// This is **host-side presentation policy** (CLI-only in Phase 3): the App
/// decides how a tool is shown, using the tool's [`title`](Tool::title) as the
/// label, the tool's own `argument_order` to pick the primary (inline)
/// argument, and its `non_default_arguments` for the bracketed rest. The line
/// shape itself (inline primary, bracketed props, `…` truncation) lives in
/// [`render_invocation_with_display_limits`], shared with the production path
/// that renders `title_arguments`; this helper differs only in how it
/// collects arguments — it includes body arguments, which the production
/// path shows as content sections instead. Both paths also fold the tool's
/// [`subtype_argument`](Tool::subtype_argument) into the label through
/// [`crate::agent::tools::display_title_with_subtype`].
#[cfg(test)]
pub(super) fn render_tool_invocation(tool: &dyn Tool, arguments: &serde_json::Value) -> String {
    let mut non_default: Vec<(String, serde_json::Value)> = tool
        .non_default_arguments(arguments)
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect();
    non_default.sort_by(|(key_a, _), (key_b, _)| {
        (tool.argument_order(key_a), key_a).cmp(&(tool.argument_order(key_b), key_b))
    });
    let (title, non_default) = crate::agent::tools::display_title_with_subtype(
        &tool.title(),
        tool.subtype_argument(),
        &non_default,
    );
    render_invocation_with_display_limits(
        &title,
        &non_default,
        PRIMARY_ARG_DISPLAY_LIMIT,
        STRING_ELIDE_LIMIT,
    )
}

pub(super) fn render_arg_value_with_limit(
    value: &serde_json::Value,
    string_limit: usize,
) -> String {
    match value {
        serde_json::Value::String(text) => truncate_with_ellipsis(text, string_limit),
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "?".to_string()),
    }
}
