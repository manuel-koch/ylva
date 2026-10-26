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

//! Shared tool layout coordinator for cached, legacy and header views.

mod body;
mod diff;
mod title;

use ratatui::text::Line;

use crate::theme::tool_bubble_border_color;
use crate::tui_reducer_state::ToolCallState;

use super::bubbles::BubbleStyle;

/// Per-bubble inputs for [`append_tool_bubble_lines`].
#[derive(Clone, Copy)]
pub(super) struct ToolBubbleArgs<'a> {
    pub(super) id: uuid::Uuid,
    pub(super) name: &'a str,
    pub(super) state: &'a ToolCallState,
    pub(super) collapsed: bool,
    pub(super) spinner_frame: u8,
    /// Sampled once so the title and its cache key use the same whole seconds.
    pub(super) elapsed_seconds: Option<u64>,
    /// The bubble's display title: the tool's own `Tool::title()` label for
    /// `name`, looked up by the caller in the `ToolRegistry` at render time
    /// (e.g. `Web Search (Brave)`, which names the configured backend).
    /// `None` for a tool the registry does not know, which falls back to the
    /// title-cased tool name.
    ///
    /// The one exception is the drill-in header bubble, which re-renders the
    /// *same* `spawn_subagent` bubble shown in the root conversation but
    /// shortens its title to `"Subagent"` there — the header already sits
    /// directly above the subagent's own transcript, so "Spawn" (relevant
    /// only from the root conversation's point of view) is redundant.
    pub(super) display_title_override: Option<&'a str>,
    /// The tool's `Tool::subtype_argument()` — the title argument naming
    /// which variant of the tool this call is (e.g. `spawn_subagent`'s
    /// `agent_type`), looked up by the caller in the `ToolRegistry`. It is
    /// folded into the title as `({value})` and dropped from the props by
    /// `display_title_with_subtype`; `None` for a tool without one.
    pub(super) subtype_argument: Option<&'a str>,
    /// Declared filesystem argument keys; also identify matching result attributes.
    pub(super) path_like_arguments: &'static [&'static str],
}

impl ToolBubbleArgs<'_> {
    fn content_sections<'a>(
        &'a self,
        extra_content_sections: &'a [crate::agent::tools::Section],
    ) -> Vec<&'a crate::agent::tools::Section> {
        // Compute body sections for the collapsible bubble body.
        let mut content_sections: Vec<&crate::agent::tools::Section> = match self.state {
            ToolCallState::Pending {
                content_sections, ..
            }
            | ToolCallState::Running {
                content_sections, ..
            }
            | ToolCallState::Denied {
                content_sections, ..
            }
            | ToolCallState::Error {
                content_sections, ..
            } => content_sections.iter().collect(),
            ToolCallState::Done {
                content_sections, ..
            } => content_sections.iter().collect(),
            _ => vec![],
        };
        // A state section that already carries a label (e.g. bash's `Command`)
        // supersedes the title-derived multiline extra for the same key, so a
        // multiline command renders once and keeps the state sections' order
        // (`Label` before `Command`) instead of re-appending `command` after them.
        let state_section_labels: Vec<String> = content_sections
            .iter()
            .filter_map(|section| match section {
                crate::agent::tools::Section::Content(content) => {
                    content.label.as_deref().map(str::to_lowercase)
                }
                _ => None,
            })
            .collect();
        content_sections.extend(extra_content_sections.iter().filter(|extra| {
            match extra {
                crate::agent::tools::Section::Content(content) => !content
                    .label
                    .as_deref()
                    .is_some_and(|label| state_section_labels.contains(&label.to_lowercase())),
                _ => true,
            }
        }));

        content_sections
    }
}

pub(super) fn append_tool_bubble_lines(
    lines: &mut Vec<Line<'static>>,
    bubble: ToolBubbleArgs<'_>,
    bubble_title_rows: &mut Vec<(usize, usize, uuid::Uuid)>,
    wrap_width: usize,
) {
    append_tool_layout(lines, bubble, bubble_title_rows, wrap_width, false);
}

pub(super) fn append_tool_layout(
    lines: &mut Vec<Line<'static>>,
    bubble: ToolBubbleArgs<'_>,
    bubble_title_rows: &mut Vec<(usize, usize, uuid::Uuid)>,
    wrap_width: usize,
    chrome_only: bool,
) -> usize {
    let bubble_start_row = lines.len();
    let style = BubbleStyle::new(tool_bubble_border_color());
    let extra_content_sections = title::append_tool_chrome(lines, bubble, &style, wrap_width);
    let body_start = lines.len();
    if chrome_only {
        return body_start;
    }
    body::append_tool_body(lines, bubble, &style, &extra_content_sections, wrap_width);
    let bubble_end_row = lines.len() - 1;
    bubble_title_rows.push((bubble_start_row, bubble_end_row, bubble.id));
    body_start
}

#[cfg(test)]
mod tests;
