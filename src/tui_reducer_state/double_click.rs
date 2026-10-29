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

//! Shared double-click detection for list rows.

use std::time::Instant;

use super::DOUBLE_CLICK_WINDOW;

/// Remembers the last clicked row so a second click on the same row within
/// [`DOUBLE_CLICK_WINDOW`] can be reported as a double-click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoubleClickTracker<Key> {
    last_click: Option<(Key, Instant)>,
}

impl<Key> Default for DoubleClickTracker<Key> {
    fn default() -> Self {
        DoubleClickTracker { last_click: None }
    }
}

impl<Key: Copy + PartialEq> DoubleClickTracker<Key> {
    /// Records a click and returns whether it completed a double-click; the
    /// completing click is consumed so a third click starts a new pair.
    pub fn register(&mut self, key: Key, now: Instant) -> bool {
        let is_double_click = matches!(
            self.last_click,
            Some((last_key, clicked_at))
                if last_key == key && now.duration_since(clicked_at) < DOUBLE_CLICK_WINDOW
        );
        self.last_click = (!is_double_click).then_some((key, now));
        is_double_click
    }

    pub fn reset(&mut self) {
        self.last_click = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn second_click_on_same_key_within_window_is_double_and_consumed() {
        let mut tracker = DoubleClickTracker::default();
        let now = Instant::now();
        assert!(!tracker.register(1, now));
        assert!(tracker.register(1, now + Duration::from_millis(10)));
        assert!(!tracker.register(1, now + Duration::from_millis(20)));
    }

    #[test]
    fn other_key_or_late_click_is_single() {
        let mut tracker = DoubleClickTracker::default();
        let now = Instant::now();
        assert!(!tracker.register(1, now));
        assert!(!tracker.register(2, now + Duration::from_millis(10)));
        assert!(!tracker.register(2, now + DOUBLE_CLICK_WINDOW * 2));
    }
}
