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

use ratatui::style::Color;

// User message palette.
/// User-message border color.
pub(crate) const USER_MESSAGE_BORDER_COLOR: Color = Color::Green;
// Queued and steered messages keep separate roles even while they currently
// share the sent-message color, so a future theme can distinguish them.
pub(crate) const USER_MESSAGE_QUEUED_BORDER_COLOR: Color = USER_MESSAGE_BORDER_COLOR;
pub(crate) const USER_MESSAGE_STEERED_BORDER_COLOR: Color = USER_MESSAGE_BORDER_COLOR;
pub(crate) const USER_MESSAGE_CANCELLED_BORDER_COLOR: Color = INTERRUPTED_COLOR;
/// Soft background shade applied to the input compose field.
pub(crate) const USER_MESSAGE_BG_COLOR: Color = Color::Rgb(20, 20, 40);
pub(crate) const INPUT_BORDER_COLOR: Color = USER_MESSAGE_BORDER_COLOR;

// Assistant response palette.
/// Assistant-response border color.
pub(crate) const ASSISTANT_STREAM_BORDER_COLOR: Color = Color::Blue;
/// Thinking border color.
pub(crate) const THINKING_BORDER_COLOR: Color = Color::Blue;

// Tool palette.
/// Tool bubble color.
pub(crate) const TOOL_BUBBLE_BORDER_COLOR: Color = Color::Magenta;

// Error and interruption palette.
/// Error color.
pub(crate) const ERROR_COLOR: Color = Color::Red;
/// Interrupted/cancelled color.
pub(crate) const INTERRUPTED_COLOR: Color = Color::DarkGray;

// Markdown palette.
/// Foreground color used for `inline code` spans.
pub(crate) const INLINE_CODE_FG: Color = Color::Rgb(224, 168, 82);
/// Foreground color used for blockquote `│` prefixes and fenced code blocks.
pub(crate) const MARKDOWN_DIM_FG: Color = MUTED_TEXT_COLOR;

// Conversation layout palette.
pub(crate) const SEPARATOR_LINE_COLOR: Color = Color::White;
/// Subagent drill-in separators use the tool-bubble color as a persistent cue.
pub(crate) const SUBAGENT_VIEW_SEPARATOR_COLOR: Color = TOOL_BUBBLE_BORDER_COLOR;
/// Dim rose tint behind the subagent drill-in header bubble and the divider below it.
pub(crate) const SUBAGENT_VIEW_HEADER_BG_COLOR: Color = Color::Rgb(40, 20, 34);
/// Conversation-list scrollbar parts share the root separator color.
pub(crate) const CHAT_SCROLLBAR_COLOR: Color = SEPARATOR_LINE_COLOR;
pub(crate) const CHAT_SELECTION_BG: Color = Color::Rgb(60, 80, 120);

// Clarify interaction palette.
/// Clarify banner foreground and selection colors.
pub(crate) const CLARIFY_BANNER_FG_COLOR: Color = Color::Yellow;
pub(crate) const CLARIFY_BANNER_SELECTION_FG_COLOR: Color = Color::Black;
pub(crate) const CLARIFY_BANNER_SCROLL_INDICATOR_COLOR: Color = Color::DarkGray;

// Help and notification palette.
/// Help popup border, title, section heading, and scroll-indicator colors.
pub(crate) const HELP_POPUP_BORDER_COLOR: Color = Color::Blue;
pub(crate) const HELP_POPUP_HEADING_COLOR: Color = Color::Blue;
pub(crate) const TOAST_BG_COLOR: Color = Color::Blue;
pub(crate) const TOAST_FG_COLOR: Color = Color::White;
pub(crate) const NOTICE_COLOR: Color = Color::Cyan;

// General content and status palette.
pub(crate) const CONTENT_COLOR: Color = Color::White;
pub(crate) const MUTED_TEXT_COLOR: Color = Color::DarkGray;
pub(crate) const LOGO_COLOR: Color = Color::Blue;
pub(crate) const BUILD_INFO_COLOR: Color = MUTED_TEXT_COLOR;
pub(crate) const FOOTER_STATUS_COLOR: Color = Color::Yellow;
/// Orange warning for a compaction engine that differs from the picker choice.
pub(crate) const COMPACTION_MISMATCH_COLOR: Color = Color::Rgb(255, 165, 0);

// Diff palette.
pub(crate) const DIFF_ADDED_COLOR: Color = Color::Green;
pub(crate) const DIFF_REMOVED_COLOR: Color = Color::Red;
pub(crate) const DIFF_CONTEXT_COLOR: Color = MUTED_TEXT_COLOR;

// Selection and approval-control palette.
pub(crate) const LIST_SELECTION_FOREGROUND_COLOR: Color = Color::Black;
pub(crate) const LIST_SELECTION_BACKGROUND_COLOR: Color = Color::Cyan;
pub(crate) const LIST_TEXT_COLOR: Color = Color::Gray;
pub(crate) const LIST_BORDER_COLOR: Color = Color::Cyan;
/// Foreground of the `/models` row for the model currently serving the conversation.
pub(crate) const ACTIVE_MODEL_COLOR: Color = Color::Green;
pub(crate) const OVERLAY_SELECTION_COLOR: Color = Color::Yellow;
pub(crate) const BUTTON_TEXT_COLOR: Color = Color::Black;
pub(crate) const APPROVE_HOVER_BACKGROUND_COLOR: Color = Color::LightGreen;
pub(crate) const APPROVE_BACKGROUND_COLOR: Color = Color::Green;
pub(crate) const DENY_HOVER_BACKGROUND_COLOR: Color = Color::LightRed;
pub(crate) const DENY_BACKGROUND_COLOR: Color = Color::Red;
