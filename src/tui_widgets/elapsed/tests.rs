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
fn elapsed_formats_seconds_minutes_and_hours() {
    assert_eq!(format_elapsed(Duration::from_secs(5)), "5s");
    assert_eq!(format_elapsed(Duration::from_secs(125)), "2m05s");
    assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h02m");
}

#[test]
fn duration_keeps_milliseconds_below_a_minute_only() {
    assert_eq!(format_duration(Duration::from_millis(1234)), "1.234s");
    assert_eq!(format_duration(Duration::from_millis(59_999)), "59.999s");
    assert_eq!(format_duration(Duration::from_millis(90_500)), "1m30s");
}
