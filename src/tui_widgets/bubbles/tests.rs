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

#[test]
fn assistant_table_stays_inside_bubble_width() {
    use super::append_assistant_lines;

    let mut lines = Vec::new();
    append_assistant_lines(
        &mut lines,
        "| abc | def |\n|-----|-----|\n| abc | def |",
        false,
        8,
    );

    assert!(
        lines.iter().all(|line| line.width() <= 12),
        "assistant bubble row exceeded its width: {lines:?}"
    );
}

#[test]
fn assistant_heading_with_emoji_stays_inside_bubble_width() {
    use super::append_assistant_lines;

    for heading in [
        "### 🐺 Project Summary: Ylva",
        "### 📅 Implementation Progress Status",
    ] {
        let mut lines = Vec::new();
        append_assistant_lines(&mut lines, heading, false, 36);

        for line in &lines {
            assert!(
                line.width() <= 40,
                "assistant heading {heading:?} row exceeded its width ({width}): {lines:?}",
                width = line.width()
            );
        }
    }
}
