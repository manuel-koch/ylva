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

//! Cached entry layout and visible-row conversation painting.

use std::sync::Arc;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::agent::tools::ToolRegistry;
use crate::conversation_render_cache::{
    ConversationRenderCache, EntryLayout, EntrySlot, LayoutKey, RowIndex,
};
use crate::highlight::active_theme;
use crate::theme::{
    CHAT_SCROLLBAR_COLOR, CHAT_SELECTION_BG, ERROR_COLOR, MUTED_TEXT_COLOR, NOTICE_COLOR,
    SUBAGENT_VIEW_SEPARATOR_COLOR, THINKING_BORDER_COLOR,
};
use crate::tui_reducer_state::{BubbleExpansionTarget, ChatEntry, ToolCallState, UserMessageState};
use crate::tui_render_state::TuiState;

use super::super::bubbles::{
    append_assistant_lines, append_streaming_lines, append_streaming_thinking_lines,
    append_thinking_lines, append_user_lines, BubbleStyle,
};
use super::super::footer::plural_noun;
use super::super::tool_bubble::{append_tool_layout, ToolBubbleArgs};
use super::super::BorrowedConversation;
use super::{
    append_logo, apply_selection_highlight_to_line, line_alignment_offset, render_chat_scrollbar,
    rendered_line_selection_data,
};

fn finish_layout(mut layout: EntryLayout, width: u16) -> EntryLayout {
    (layout.selection, layout.prefixes) = rendered_line_selection_data(&layout.rows, width);
    layout
}

fn layout_key(slot: EntrySlot, revision: u64, width: u16, collapsed: bool) -> LayoutKey {
    LayoutKey {
        slot,
        revision,
        width,
        collapsed,
        theme: active_theme().syntect_key().to_owned(),
        title: None,
        animation: None,
    }
}

fn cached_tool(
    cache: &mut ConversationRenderCache,
    key: LayoutKey,
    bubble: ToolBubbleArgs<'_>,
    header: bool,
) -> Vec<Arc<EntryLayout>> {
    let width = key.width;
    let wrap_width = (width as usize)
        .saturating_sub(if header { 4 } else { 5 })
        .max(1);
    let mut body_key = key.clone();
    body_key.title = Some(format!(
        "{:?}/{:?}",
        bubble.subtype_argument, bubble.path_like_arguments
    ));
    let body = cache.get_or_build(body_key, || {
        let mut layout = EntryLayout::default();
        let body_start = append_tool_layout(
            &mut layout.rows,
            ToolBubbleArgs { ..bubble },
            &mut layout.tools,
            wrap_width,
            false,
        );
        layout.rows.drain(..body_start);
        layout.tools.clear();
        if !header {
            layout.rows.push(Line::default());
        }
        finish_layout(layout, width)
    });
    let mut chrome_key = key;
    chrome_key.slot = match chrome_key.slot {
        EntrySlot::Header => EntrySlot::HeaderChrome,
        EntrySlot::Entry(index) => EntrySlot::Chrome(index),
        _ => unreachable!("tool slot"),
    };
    if matches!(bubble.state, ToolCallState::Running { .. }) {
        chrome_key.animation = Some((bubble.spinner_frame, bubble.elapsed_seconds.unwrap_or(0)));
    }
    let chrome = cache.get_or_build(chrome_key, || {
        let mut layout = EntryLayout::default();
        append_tool_layout(&mut layout.rows, bubble, &mut Vec::new(), wrap_width, true);
        layout.tools.push((
            0,
            (layout.rows.len() + body.rows.len()).saturating_sub(1 + usize::from(!header)),
            bubble.id,
        ));
        finish_layout(layout, width)
    });
    vec![chrome, body]
}

pub(in crate::tui_widgets) fn prepare_cached_header(
    conversation: &mut BorrowedConversation<'_>,
    state: &mut TuiState,
    registry: &ToolRegistry,
    width: u16,
) {
    let mut layouts = Vec::new();
    if let Some((
        index,
        ChatEntry::ToolBubble {
            id,
            name,
            state: bubble_state,
            collapsed,
        },
    )) = conversation.source.header
    {
        let collapsed = *collapsed
            || conversation.source.expansion_feedback == Some(BubbleExpansionTarget::Tool(*id));
        let tool = registry.get(name);
        let title = if name == crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME {
            Some("Subagent".to_owned())
        } else {
            tool.as_ref().map(|tool| tool.title())
        };
        let subtype = tool.as_ref().and_then(|tool| tool.subtype_argument());
        let mut key = layout_key(
            EntrySlot::Header,
            conversation
                .source
                .revisions
                .get(&(None, index))
                .copied()
                .unwrap_or_default(),
            width,
            collapsed,
        );
        key.title = Some(format!(
            "{title:?}/{subtype:?}/{:?}/{}",
            tool.as_ref()
                .map_or(&[][..], |tool| tool.path_like_arguments()),
            conversation
                .source
                .title_revisions
                .get(id)
                .copied()
                .unwrap_or_default()
        ));
        layouts = cached_tool(
            conversation.cache,
            key,
            ToolBubbleArgs {
                id: *id,
                name,
                state: bubble_state,
                collapsed,
                spinner_frame: state.spinner_frame,
                elapsed_seconds: state
                    .subagent_started_at
                    .get(id)
                    .map(|started| started.elapsed().as_secs()),
                display_title_override: title.as_deref(),
                subtype_argument: subtype,
                path_like_arguments: tool.as_ref().map_or(&[], |tool| tool.path_like_arguments()),
            },
            true,
        );
    }
    update_selection_index(
        &mut conversation.cache.header,
        layouts,
        &mut state.subagent_view.header_rendered_lines,
        &mut state.subagent_view.header_rendered_line_prefix_widths,
        false,
    );
}

fn update_selection_index(
    index: &mut RowIndex,
    layouts: Vec<Arc<EntryLayout>>,
    selection: &mut Vec<String>,
    prefixes: &mut Vec<usize>,
    trim_blank: bool,
) -> bool {
    if index.layouts.is_empty() {
        selection.clear();
        prefixes.clear();
    }
    if index.layouts.len() == layouts.len() {
        let mut offset = 0;
        for (old, new) in index.layouts.iter().zip(&layouts) {
            if !Arc::ptr_eq(old, new) {
                selection.splice(
                    offset..offset + old.selection.len(),
                    new.selection.iter().cloned(),
                );
                prefixes.splice(
                    offset..offset + old.prefixes.len(),
                    new.prefixes.iter().copied(),
                );
            }
            offset += new.selection.len();
        }
    } else {
        selection.clear();
        prefixes.clear();
        for layout in &layouts {
            selection.extend(layout.selection.iter().cloned());
            prefixes.extend_from_slice(&layout.prefixes);
        }
    }
    index.replace(layouts, trim_blank)
}

pub(super) fn build_entry(
    entry: &ChatEntry,
    index: usize,
    width: u16,
    collapsed: bool,
) -> EntryLayout {
    let mut layout = EntryLayout::default();
    let body_width = (width as usize).saturating_sub(5).max(1);
    let wrap_width = (width as usize).saturating_sub(2).max(1);
    match entry {
        ChatEntry::UserMessage { text, state } => {
            append_user_lines(&mut layout.rows, text, *state, body_width)
        }
        ChatEntry::AssistantMessage { text, interrupted } => {
            append_assistant_lines(&mut layout.rows, text, *interrupted, body_width)
        }
        ChatEntry::ThinkingMessage { text, duration, .. } => append_thinking_lines(
            &mut layout.rows,
            text,
            collapsed,
            *duration,
            index,
            &mut layout.thinking,
            body_width,
        ),
        ChatEntry::ErrorLine { message } => {
            for (index, wrapped) in textwrap::wrap(message, wrap_width.saturating_sub(6).max(1))
                .into_iter()
                .enumerate()
            {
                layout.rows.push(Line::styled(
                    if index == 0 {
                        format!("  ⚠️  {wrapped}")
                    } else {
                        format!("      {wrapped}")
                    },
                    Style::default().fg(ERROR_COLOR),
                ));
            }
        }
        ChatEntry::CompactionMarker {
            summarized_messages,
        } => {
            layout.rows.push(Line::styled(
                format!(
                    "  ── compaction · {summarized_messages} {} summarized ──",
                    plural_noun(*summarized_messages, "message", "messages")
                ),
                Style::default()
                    .fg(MUTED_TEXT_COLOR)
                    .add_modifier(Modifier::DIM),
            ));
        }
        ChatEntry::Notice { text } => {
            for (index, line) in text.lines().enumerate() {
                let wrapped = textwrap::wrap(line, wrap_width.saturating_sub(6).max(1));
                if wrapped.is_empty() {
                    layout.rows.push(Line::default());
                }
                for (wrap_index, wrapped) in wrapped.into_iter().enumerate() {
                    layout.rows.push(Line::styled(
                        if index == 0 && wrap_index == 0 {
                            format!("  ⓘ  {wrapped}")
                        } else {
                            format!("     {wrapped}")
                        },
                        Style::default().fg(NOTICE_COLOR),
                    ));
                }
            }
        }
        ChatEntry::ToolBubble { .. } => unreachable!("tools have independently animated chrome"),
    }
    layout.rows.push(Line::default());
    layout
}

pub(in crate::tui_widgets) fn draw_cached_chat(
    frame: &mut Frame,
    area: Rect,
    state: &mut TuiState,
    registry: &ToolRegistry,
    conversation: &mut BorrowedConversation<'_>,
) {
    let [content_area, scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area);
    state.conversation.chat_area = content_area;
    let width = content_area.width;
    let source = conversation.source;
    let cache = &mut conversation.cache;
    let intro = cache.get_or_build(
        {
            let mut key = layout_key(
                EntrySlot::Intro,
                u64::from(source.entries.is_empty()),
                width,
                false,
            );
            key.title = Some(state.display_metadata.profile_name.clone());
            key
        },
        || {
            let mut layout = EntryLayout::default();
            if source.entries.is_empty() && source.header.is_none() {
                append_logo(&mut layout.rows, &state.display_metadata.profile_name);
            } else {
                layout.rows.push(Line::default());
            }
            finish_layout(layout, width)
        },
    );
    let mut layouts = vec![intro];
    let mut deferred = Vec::new();
    for (index, entry) in source.entries.iter().enumerate() {
        let revision = source
            .revisions
            .get(&(source.run_id, index))
            .copied()
            .unwrap_or_default();
        let collapsed = matches!(
            entry,
            ChatEntry::ToolBubble {
                collapsed: true,
                ..
            } | ChatEntry::ThinkingMessage {
                collapsed: true,
                ..
            }
        ) || match entry {
            ChatEntry::ToolBubble { id, .. } => {
                source.expansion_feedback == Some(BubbleExpansionTarget::Tool(*id))
            }
            ChatEntry::ThinkingMessage { .. } => {
                source.expansion_feedback == Some(BubbleExpansionTarget::Thinking(index))
            }
            _ => false,
        };
        let key = layout_key(EntrySlot::Entry(index), revision, width, collapsed);
        if let ChatEntry::ToolBubble {
            id,
            name,
            state: bubble_state,
            ..
        } = entry
        {
            let tool = registry.get(name);
            let title = tool.as_ref().map(|tool| tool.title());
            let subtype = tool.as_ref().and_then(|tool| tool.subtype_argument());
            let mut key = key;
            key.title = Some(format!(
                "{title:?}/{subtype:?}/{:?}/{}",
                tool.as_ref()
                    .map_or(&[][..], |tool| tool.path_like_arguments()),
                source.title_revisions.get(id).copied().unwrap_or_default()
            ));
            let rows = cached_tool(
                cache,
                key,
                ToolBubbleArgs {
                    id: *id,
                    name,
                    state: bubble_state,
                    collapsed,
                    spinner_frame: state.spinner_frame,
                    elapsed_seconds: state
                        .subagent_started_at
                        .get(id)
                        .map(|started| started.elapsed().as_secs()),
                    display_title_override: title.as_deref(),
                    subtype_argument: subtype,
                    path_like_arguments: tool
                        .as_ref()
                        .map_or(&[], |tool| tool.path_like_arguments()),
                },
                false,
            );
            if matches!(bubble_state.as_ref(), ToolCallState::Pending { .. }) {
                deferred.extend(rows);
            } else {
                layouts.extend(rows);
            }
        } else {
            layouts.push(cache.get_or_build(key, || {
                finish_layout(build_entry(entry, index, width, collapsed), width)
            }));
        }
    }
    let body_width = (width as usize).saturating_sub(5).max(1);
    if !source.turn.partial_thinking_text.is_empty() {
        let collapsed = source.turn.partial_thinking_collapsed
            || source.expansion_feedback
                == Some(BubbleExpansionTarget::Thinking(
                    crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX,
                ));
        let body = cache.get_or_build(
            layout_key(
                EntrySlot::Thinking,
                source.stream_revision,
                width,
                collapsed,
            ),
            || {
                let mut layout = EntryLayout::default();
                append_streaming_thinking_lines(
                    &mut layout.rows,
                    &source.turn.partial_thinking_text,
                    source.turn.thinking_started_at,
                    collapsed,
                    &mut Vec::new(),
                    body_width,
                );
                layout.rows.remove(0);
                layout.rows.push(Line::default());
                finish_layout(layout, width)
            },
        );
        let mut key = layout_key(
            EntrySlot::ThinkingChrome,
            source.stream_revision,
            width,
            collapsed,
        );
        let elapsed_seconds = source
            .turn
            .thinking_started_at
            .map(|started| started.elapsed().as_secs())
            .unwrap_or_default();
        key.animation = Some((0, elapsed_seconds));
        let chrome = cache.get_or_build(key, || {
            let mut layout = EntryLayout::default();
            let indicator = if collapsed { "▶" } else { "▼" };
            let dim = Style::default().add_modifier(Modifier::DIM);
            layout
                .rows
                .push(BubbleStyle::new(THINKING_BORDER_COLOR).content_line(
                    vec![Span::styled(
                        format!("{indicator} Thinking… ({elapsed_seconds}s)"),
                        dim,
                    )],
                    dim,
                ));
            layout.thinking.push((
                0,
                body.rows.len().saturating_sub(1),
                crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX,
            ));
            finish_layout(layout, width)
        });
        layouts.extend([chrome, body]);
    }
    if !source.turn.partial_assistant_text.is_empty() {
        layouts.push(cache.get_or_build(
            layout_key(EntrySlot::Response, source.stream_revision, width, false),
            || {
                let mut layout = EntryLayout::default();
                append_streaming_lines(
                    &mut layout.rows,
                    &source.turn.partial_assistant_text,
                    body_width,
                );
                layout.rows.push(Line::default());
                finish_layout(layout, width)
            },
        ));
    }
    for (index, text) in source.queued.iter().enumerate() {
        layouts.push(cache.get_or_build(
            layout_key(
                EntrySlot::Queued(index),
                source.queued_revision,
                width,
                false,
            ),
            || {
                let mut layout = EntryLayout::default();
                append_user_lines(&mut layout.rows, text, UserMessageState::Queued, body_width);
                layout.rows.push(Line::default());
                finish_layout(layout, width)
            },
        ));
    }
    layouts.extend(deferred);
    let changed = update_selection_index(
        &mut cache.conversation,
        layouts,
        &mut state.conversation.chat_rendered_lines,
        &mut state.conversation.chat_rendered_line_prefix_widths,
        true,
    );
    if changed {
        state.conversation.bubble_title_rows.clear();
        state.conversation.thinking_title_rows.clear();
        let mut offset = 0;
        for layout in &cache.conversation.layouts {
            state.conversation.bubble_title_rows.extend(
                layout
                    .tools
                    .iter()
                    .map(|(start, end, id)| (start + offset, end + offset, *id)),
            );
            state.conversation.thinking_title_rows.extend(
                layout
                    .thinking
                    .iter()
                    .map(|(start, end, index)| (start + offset, end + offset, *index)),
            );
            offset += layout.rows.len();
        }
    }
    let mut slots: Vec<_> = (0..source.entries.len())
        .flat_map(|index| [EntrySlot::Entry(index), EntrySlot::Chrome(index)])
        .collect();
    slots.push(EntrySlot::Intro);
    if source.header.is_some() {
        slots.extend([EntrySlot::Header, EntrySlot::HeaderChrome]);
    }
    if !source.turn.partial_assistant_text.is_empty() {
        slots.push(EntrySlot::Response);
    }
    if !source.turn.partial_thinking_text.is_empty() {
        slots.extend([EntrySlot::Thinking, EntrySlot::ThinkingChrome]);
    }
    slots.extend((0..source.queued.len()).map(EntrySlot::Queued));
    cache.retain_slots(&slots);
    let total = cache.conversation.height;
    let visible = content_area.height as usize;
    let maximum = total.saturating_sub(visible);
    state.conversation.chat_scrollbar = crate::tui_reducer_state::ChatScrollbarLayout {
        rect: if maximum > 0 {
            scrollbar_area
        } else {
            Rect::default()
        },
        maximum_scroll_offset: maximum,
    };
    let offset = if state.conversation.scroll_locked {
        maximum
    } else {
        state.conversation.scroll_offset.min(maximum)
    };
    state.conversation.scroll_offset = offset;
    state.conversation.scroll_locked |= offset == maximum;
    let mut rows = cache.conversation.visible_rows(offset, visible);
    cache.painted_rows += rows.len();
    highlight_visible_rows(&mut rows, offset, state.conversation.chat_selection, width);
    frame.render_widget(Paragraph::new(rows), content_area);
    if total > visible {
        render_chat_scrollbar(
            frame,
            scrollbar_area,
            offset,
            maximum,
            total,
            visible,
            if source.header.is_some() {
                SUBAGENT_VIEW_SEPARATOR_COLOR
            } else {
                CHAT_SCROLLBAR_COLOR
            },
        );
    }
}

pub(super) fn highlight_visible_rows(
    rows: &mut [Line<'static>],
    offset: usize,
    selection: Option<crate::tui_reducer_state::ChatSelection>,
    width: u16,
) {
    if let Some(selection) = selection.filter(|selection| !selection.is_empty()) {
        let (start, start_col, end, end_col) = selection.normalized();
        for (index, row) in rows.iter_mut().enumerate() {
            let index = index + offset;
            if index >= start && index <= end {
                let alignment = line_alignment_offset(row, width);
                apply_selection_highlight_to_line(
                    row,
                    if index == start {
                        start_col.saturating_sub(alignment)
                    } else {
                        0
                    },
                    if index == end {
                        end_col.saturating_sub(alignment)
                    } else {
                        usize::MAX
                    },
                    CHAT_SELECTION_BG,
                );
            }
        }
    }
}
