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

//! `ctrl-x` leader-key chords.
//!
//! Ylva's extensible keybinding budget. Bare `ctrl-*` letters are reserved
//! by readline and terminal emulators, `ctrl-shift-*` is undeliverable on
//! terminals without the kitty keyboard protocol, and `alt-*` dies on macOS
//! Option-composition — but the bare `ctrl-x` byte reaches the pty
//! unchanged everywhere and collides with nothing Ylva or readline needs.
//! `ctrl-x <key>` chords therefore give effectively unlimited two-key
//! bindings; see the keybinding budget in `docs/implementation-notes.md`.
//!
//! The chord lives in the Driver (outside `AppState`, which `/clear` and
//! `/session` replace) next to the double-press confirm precedent: the
//! leader arm times out on its own, so no timer task is needed — the next
//! key event simply checks the deadline.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::events::AppAction;

/// How long after `ctrl-x` the chord stays armed. Generous enough for
/// slow key hunts, short enough that an abandoned leader does not swallow
/// a deliberate later keypress. The leader is cancelled early by `esc`,
/// `ctrl-c`, or a second `ctrl-x`.
pub const LEADER_TIMEOUT: Duration = Duration::from_secs(2);

/// A `ctrl-x <key>` chord that is currently armed, waiting for its
/// second key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaderChord {
    armed_at: Option<Instant>,
}

impl LeaderChord {
    /// Arm the chord (`ctrl-x` was just pressed). A second `ctrl-x`
    /// re-arms rather than nesting.
    pub fn arm(&mut self, now: Instant) {
        self.armed_at = Some(now);
    }

    /// Disarm without running any action (`esc`, `ctrl-c`, or expiry).
    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    /// Whether a chord is currently armed and has not expired.
    pub fn is_armed(&self, now: Instant) -> bool {
        self.armed_at
            .is_some_and(|armed_at| now.saturating_duration_since(armed_at) < LEADER_TIMEOUT)
    }

    /// Feed the next key event to the armed chord.
    ///
    /// Returns the resolved [`AppAction`] when the chord completed,
    /// `None` when the event is left for normal handling (no chord armed,
    /// the chord expired, or the key disarms it). A completed or cancelled
    /// chord is always disarmed first.
    ///
    /// Currently bound chords:
    /// - `ctrl-x a` → agents popup (same as `/agents`)
    /// - `ctrl-x f` → toggle favorite in the `/models` picker
    /// - `ctrl-x p` → back to the parent conversation (subagent view)
    pub fn key_event(
        &mut self,
        event: crossterm::event::KeyEvent,
        now: Instant,
    ) -> Option<AppAction> {
        if !self.is_armed(now) {
            return None;
        }
        if event.kind != crossterm::event::KeyEventKind::Press {
            return None;
        }
        if event.code == KeyCode::Esc
            || (event.code == KeyCode::Char('c') && event.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.disarm();
            return None;
        }
        if event.code == KeyCode::Char('x') && event.modifiers == KeyModifiers::CONTROL {
            // Re-arm: the user is restarting the chord, not completing it.
            self.arm(now);
            return None;
        }
        self.disarm();
        match event.code {
            KeyCode::Char('a') if event.modifiers == KeyModifiers::NONE => {
                Some(AppAction::LeaderAgents)
            }
            KeyCode::Char('f') if event.modifiers == KeyModifiers::NONE => {
                Some(AppAction::LeaderFavoriteModel)
            }
            KeyCode::Char('p') if event.modifiers == KeyModifiers::NONE => {
                Some(AppAction::LeaderParentView)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, modifiers)
    }

    #[test]
    fn unarmed_chord_leaves_every_key_unhandled() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        assert_eq!(
            chord.key_event(press(KeyCode::Char('a'), KeyModifiers::NONE), now),
            None
        );
        assert!(!chord.is_armed(now));
    }

    #[test]
    fn armed_chord_resolves_a_into_show_agents() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        chord.arm(now);
        assert!(chord.is_armed(now));
        assert_eq!(
            chord.key_event(press(KeyCode::Char('a'), KeyModifiers::NONE), now),
            Some(AppAction::LeaderAgents)
        );
        assert!(!chord.is_armed(now), "a completed chord is disarmed");
    }

    #[test]
    fn chord_arm_times_out_after_two_seconds() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        chord.arm(now);
        let within = now + LEADER_TIMEOUT - Duration::from_millis(1);
        assert!(chord.is_armed(within));
        let after = now + LEADER_TIMEOUT;
        assert!(!chord.is_armed(after));
        assert_eq!(
            chord.key_event(press(KeyCode::Char('a'), KeyModifiers::NONE), after),
            None,
            "an expired chord leaves the key for normal handling"
        );
    }

    #[test]
    fn escape_and_ctrl_c_cancel_the_chord_without_actions() {
        for cancel in [
            press(KeyCode::Esc, KeyModifiers::NONE),
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut chord = LeaderChord::default();
            let now = Instant::now();
            chord.arm(now);
            assert_eq!(chord.key_event(cancel, now), None);
            assert!(!chord.is_armed(now));
        }
    }

    #[test]
    fn a_second_ctrl_x_rearms_instead_of_completing() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        chord.arm(now);
        let leader = press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(chord.key_event(leader, now), None);
        assert!(chord.is_armed(now), "still armed after re-arm");
        assert_eq!(
            chord.key_event(press(KeyCode::Char('a'), KeyModifiers::NONE), now),
            Some(AppAction::LeaderAgents)
        );
    }

    #[test]
    fn unknown_chord_keys_disarm_and_fall_through_to_normal_handling() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        chord.arm(now);
        assert_eq!(
            chord.key_event(press(KeyCode::Char('z'), KeyModifiers::NONE), now),
            None
        );
        assert!(!chord.is_armed(now));
    }

    #[test]
    fn key_release_never_completes_a_chord() {
        let mut chord = LeaderChord::default();
        let now = Instant::now();
        chord.arm(now);
        let mut release = press(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = crossterm::event::KeyEventKind::Release;
        assert_eq!(chord.key_event(release, now), None);
        assert!(chord.is_armed(now), "release must not consume the chord");
    }
}
