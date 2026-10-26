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

use super::super::output::*;
use crate::theme::{thinking_border_color, user_message_border_color};

#[test]
fn styled_cli_line_stays_plain_when_colors_are_disabled() {
    let mut output = Vec::new();

    write_cli_styled_line(
        &mut output,
        "---[ User ]---",
        user_message_border_color(),
        false,
        CliOutputOptions::plain(),
    )
    .expect("plain output succeeds");

    assert_eq!(output, b"---[ User ]---\n");
}

#[test]
fn styled_cli_line_uses_the_shared_tui_color_and_resets_ansi_state() {
    let mut output = Vec::new();

    write_cli_styled_line(
        &mut output,
        "---[ Assistant Thinking ]---",
        thinking_border_color(),
        true,
        CliOutputOptions::new(true),
    )
    .expect("colored output succeeds");

    assert_eq!(output, b"\x1b[2;34m---[ Assistant Thinking ]---\x1b[0m\n");
}

#[test]
fn flushing_response_writes_its_terminating_newline() {
    let mut output = Vec::new();
    let mut current_section = StreamSection::Response;
    let mut thinking_started_at = None;

    flush_stream_section(
        &mut output,
        &mut current_section,
        &mut thinking_started_at,
        CliOutputOptions::plain(),
    )
    .expect("response flush succeeds");

    assert_eq!(output, b"\n");
    assert!(current_section == StreamSection::None);
}
