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
fn plural_noun_uses_singular_only_for_one() {
    assert_eq!(plural_noun(1, "message", "messages"), "message");
    assert_eq!(plural_noun(0, "message", "messages"), "messages");
    assert_eq!(plural_noun(2, "message", "messages"), "messages");
}
