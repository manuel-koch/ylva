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

use super::*;

#[test]
fn terminal_title_uses_the_session_title_and_product_suffix() {
    assert_eq!(
        format_terminal_title(Some("Investigate persistence")),
        "Investigate persistence - Ylva Agent Harness"
    );
    assert_eq!(format_terminal_title(None), "Ylva Agent Harness");
}

#[test]
fn terminal_title_removes_control_characters_from_model_output() {
    assert_eq!(
        format_terminal_title(Some("Title\u{001b} \nwith controls")),
        "Title with controls - Ylva Agent Harness"
    );
}
