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

//! The UI colour palette.
//!
//! Every colour is a function of the active [`ThemeName`] (see
//! [`crate::highlight::active_theme`]): `dark` targets dark terminal
//! backgrounds, `light` targets light ones (dark text, pale tinted
//! backgrounds, darker accents that stay readable on white). Colours are
//! resolved at call time, so nothing may cache one across a theme change.

use ratatui::style::Color;

use crate::highlight::{active_theme, ThemeName};

fn themed(dark: Color, light: Color) -> Color {
    match active_theme() {
        ThemeName::Dark => dark,
        ThemeName::Light => light,
    }
}

/// Declares one `pub(crate) fn` per colour, `name(dark, light)`, plus a test
/// table of every `(name, dark, light)` triple.
macro_rules! themed_colors {
    ($($(#[$meta:meta])* $name:ident($dark:expr, $light:expr);)*) => {
        $($(#[$meta])* pub(crate) fn $name() -> Color { themed($dark, $light) })*

        #[cfg(test)]
        pub(crate) const THEMED_COLORS: &[(&str, Color, Color)] =
            &[$((stringify!($name), $dark, $light)),*];
    };
}

/// Amber that stays readable as text on a light background.
const LIGHT_AMBER: Color = Color::Rgb(176, 124, 0);
/// Teal replacing cyan, which washes out on a light background.
const LIGHT_TEAL: Color = Color::Rgb(0, 115, 135);

themed_colors! {
    // User message palette.
    /// User-message border color.
    user_message_border_color(Color::Green, Color::Green);
    /// Soft background shade applied to the input compose field.
    user_message_bg_color(Color::Rgb(20, 20, 40), Color::Rgb(226, 230, 246));

    // Assistant response palette.
    /// Assistant-response border color.
    assistant_stream_border_color(Color::Blue, Color::Blue);
    /// Thinking border color.
    thinking_border_color(Color::Blue, Color::Blue);

    // Tool palette.
    /// Tool bubble color.
    tool_bubble_border_color(Color::Magenta, Color::Magenta);

    // Error and interruption palette.
    /// Error color.
    error_color(Color::Red, Color::Red);
    /// Interrupted/cancelled color.
    interrupted_color(Color::DarkGray, Color::DarkGray);

    // Markdown palette.
    /// Foreground color used for `inline code` spans.
    inline_code_fg(Color::Rgb(224, 168, 82), Color::Rgb(150, 84, 0));

    // Conversation layout palette.
    separator_line_color(Color::White, Color::Black);
    /// Dim rose tint behind the subagent drill-in header bubble and the divider below it.
    subagent_view_header_bg_color(Color::Rgb(40, 20, 34), Color::Rgb(252, 241, 247));
    chat_selection_bg(Color::Rgb(60, 80, 120), Color::Rgb(184, 206, 245));

    // Clarify interaction palette.
    /// Clarify banner foreground and selection colors.
    clarify_banner_fg_color(Color::Yellow, LIGHT_AMBER);
    clarify_banner_selection_fg_color(Color::Black, Color::Black);
    clarify_banner_scroll_indicator_color(Color::DarkGray, Color::DarkGray);

    // Help and notification palette.
    /// Help popup border, title, section heading, and scroll-indicator colors.
    help_popup_border_color(Color::Blue, Color::Blue);
    help_popup_heading_color(Color::Blue, Color::Blue);
    toast_bg_color(Color::Blue, Color::Blue);
    toast_fg_color(Color::White, Color::White);
    notice_color(Color::Cyan, LIGHT_TEAL);

    // General content and status palette.
    content_color(Color::White, Color::Black);
    muted_text_color(Color::DarkGray, Color::DarkGray);
    logo_color(Color::Blue, Color::Blue);
    footer_status_color(Color::Yellow, LIGHT_AMBER);
    /// Orange warning for a compaction engine that differs from the picker choice.
    compaction_mismatch_color(Color::Rgb(255, 165, 0), Color::Rgb(190, 95, 0));

    // Diff palette.
    diff_added_color(Color::Green, Color::Green);
    diff_removed_color(Color::Red, Color::Red);

    // Selection and approval-control palette.
    list_selection_foreground_color(Color::Black, Color::Black);
    list_selection_background_color(Color::Cyan, Color::Cyan);
    list_text_color(Color::Gray, Color::Rgb(95, 95, 95));
    list_border_color(Color::Cyan, LIGHT_TEAL);
    /// Foreground of the `/models` row for the model currently serving the conversation.
    active_model_color(Color::Green, Color::Rgb(0, 140, 40));
    overlay_selection_color(Color::Yellow, LIGHT_AMBER);
    button_text_color(Color::Black, Color::Black);
    approve_hover_background_color(Color::LightGreen, Color::LightGreen);
    approve_background_color(Color::Green, Color::Green);
    deny_hover_background_color(Color::LightRed, Color::LightRed);
    deny_background_color(Color::Red, Color::Red);
}

// Queued and steered messages keep separate roles even while they currently
// share the sent-message color, so a future theme can distinguish them.
pub(crate) fn user_message_queued_border_color() -> Color {
    user_message_border_color()
}
pub(crate) fn user_message_steered_border_color() -> Color {
    user_message_border_color()
}
pub(crate) fn user_message_cancelled_border_color() -> Color {
    interrupted_color()
}
pub(crate) fn input_border_color() -> Color {
    user_message_border_color()
}
/// Foreground color used for blockquote `│` prefixes and fenced code blocks.
pub(crate) fn markdown_dim_fg() -> Color {
    muted_text_color()
}
/// Subagent drill-in separators use the tool-bubble color as a persistent cue.
pub(crate) fn subagent_view_separator_color() -> Color {
    tool_bubble_border_color()
}
/// Conversation-list scrollbar parts share the root separator color.
pub(crate) fn chat_scrollbar_color() -> Color {
    separator_line_color()
}
pub(crate) fn build_info_color() -> Color {
    muted_text_color()
}
pub(crate) fn diff_context_color() -> Color {
    muted_text_color()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_light_background(color: Color) -> bool {
        matches!(color, Color::Rgb(red, green, blue) if u16::from(red) + u16::from(green) + u16::from(blue) > 600)
    }

    #[test]
    fn light_palette_inverts_text_and_tints_backgrounds_pale() {
        let light_of = |name: &str| {
            THEMED_COLORS
                .iter()
                .find(|(candidate, ..)| *candidate == name)
                .map(|(_, _, light)| *light)
                .unwrap_or_else(|| panic!("unknown color {name}"))
        };
        assert_eq!(light_of("content_color"), Color::Black);
        for background in [
            "user_message_bg_color",
            "subagent_view_header_bg_color",
            "chat_selection_bg",
        ] {
            assert!(
                is_light_background(light_of(background)),
                "{background} must be pale in the light theme"
            );
        }
    }

    #[test]
    fn light_palette_avoids_colors_that_vanish_on_white() {
        for (name, _, light) in THEMED_COLORS {
            // Backgrounds that carry their own contrasting foreground.
            if matches!(*name, "toast_fg_color" | "list_selection_background_color") {
                continue;
            }
            assert!(
                !matches!(
                    light,
                    Color::White | Color::Yellow | Color::Cyan | Color::Gray
                ),
                "{name} would be unreadable on a light background"
            );
        }
    }
}
