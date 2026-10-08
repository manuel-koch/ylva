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
fn help_text_uses_shared_confirmation_and_scroll_values() {
    let sections = help_sections();
    let entries = sections
        .iter()
        .flat_map(|section| section.entries.iter())
        .collect::<Vec<_>>();
    let confirmation_label = format!("ctrl-c (x2, <={}s)", super::CONFIRM_TIMEOUT.as_secs());
    assert!(entries.iter().any(|(key, _)| key == &confirmation_label));
    assert!(entries.iter().any(|(_, description)| {
        description.contains(&format!(
            "by {} columns",
            super::HELP_HORIZONTAL_SCROLL_COLUMNS
        ))
    }));
}
