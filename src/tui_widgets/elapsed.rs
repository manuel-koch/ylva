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

//! Shared elapsed-time text for the agents popup and the chat bubbles.

use std::time::Duration;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;

/// Whole-second elapsed time: `45s`, `1m30s`, `1h02m`.
pub(super) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < SECONDS_PER_MINUTE {
        format!("{seconds}s")
    } else if seconds < SECONDS_PER_HOUR {
        format!(
            "{}m{:02}s",
            seconds / SECONDS_PER_MINUTE,
            seconds % SECONDS_PER_MINUTE
        )
    } else {
        format!(
            "{}h{:02}m",
            seconds / SECONDS_PER_HOUR,
            (seconds % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE
        )
    }
}

/// Finished duration: millisecond precision below a minute (`0.123s`),
/// otherwise the whole-second form of [`format_elapsed`].
pub(super) fn format_duration(duration: Duration) -> String {
    if duration.as_secs() < SECONDS_PER_MINUTE {
        format!("{:.3}s", duration.as_secs_f64())
    } else {
        format_elapsed(duration)
    }
}

#[cfg(test)]
mod tests;
