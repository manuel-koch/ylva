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
fn profile_name_rule_accepts_letters_digits_underscores_and_hyphens() {
    let longest = "a".repeat(MAX_PROFILE_NAME_LENGTH);
    for name in ["default", "work-2", "a_b", "9lives", longest.as_str()] {
        assert!(is_valid_profile_name(name), "{name:?} must be accepted");
    }
}

#[test]
fn profile_name_rule_rejects_names_that_are_not_a_single_safe_component() {
    let too_long = "a".repeat(MAX_PROFILE_NAME_LENGTH + 1);
    for name in [
        "",
        "-x",
        "_x",
        "../x",
        "a/b",
        "a\\b",
        ".",
        "..",
        "has space",
        "my.profile",
        "naïve",
        too_long.as_str(),
    ] {
        assert!(!is_valid_profile_name(name), "{name:?} must be rejected");
    }
}
