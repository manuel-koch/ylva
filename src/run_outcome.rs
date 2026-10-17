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

//! Terminal outcome of one run, shared by the headless and TUI drivers.

use crate::agent::UiUpdate;
use crate::exit_code::ExitCode;

/// Facts recorded while a run is active, mapped to one [`ExitCode`] at the end.
///
/// Precedence, highest first: interruption, general error, approval denial,
/// success. Tool results never set the outcome: a tool's own failure, kill, or
/// non-zero exit is not a run failure.
///
/// A cancelled stream alone is not an interruption: the stream is cancelled by
/// a stop, and only an external SIGINT or SIGTERM records the interruption.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunOutcome {
    interrupted: bool,
    failed: bool,
    approval_denied: bool,
}

impl RunOutcome {
    /// Record the outcome-relevant part of a driver event.
    ///
    /// A root-level error (`run_id: None`) counts as a failure. Subagent errors and
    /// tool results do not affect the outcome.
    pub fn record_update(&mut self, update: &UiUpdate) {
        if let UiUpdate::Error { run_id: None, .. } = update {
            self.record_root_error();
        }
    }

    /// An external SIGINT or SIGTERM aborted the run. A stop requested by the user
    /// (Ctrl+C or Esc in the TUI, the cancelled stream itself) does not call this.
    pub fn record_interruption(&mut self) {
        self.interrupted = true;
    }

    /// A root-level error ended the run, such as a provider or stream failure.
    pub fn record_root_error(&mut self) {
        self.failed = true;
    }

    /// A tool required approval and was denied. The run continues.
    pub fn record_approval_denied(&mut self) {
        self.approval_denied = true;
    }

    /// The single exit code for everything recorded so far.
    pub fn exit_code(self) -> ExitCode {
        if self.interrupted {
            ExitCode::Interrupted
        } else if self.failed {
            ExitCode::GeneralError
        } else if self.approval_denied {
            ExitCode::ApprovalDenied
        } else {
            ExitCode::Success
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn root_error() -> UiUpdate {
        UiUpdate::Error {
            message: "provider failed".into(),
            run_id: None,
        }
    }

    #[test]
    fn nothing_recorded_is_success() {
        assert_eq!(RunOutcome::default().exit_code(), ExitCode::Success);
    }

    #[test]
    fn interruption_alone_is_interrupted() {
        let mut outcome = RunOutcome::default();
        outcome.record_interruption();
        assert_eq!(outcome.exit_code(), ExitCode::Interrupted);
    }

    #[test]
    fn root_error_alone_is_general_error() {
        let mut outcome = RunOutcome::default();
        outcome.record_root_error();
        assert_eq!(outcome.exit_code(), ExitCode::GeneralError);
    }

    #[test]
    fn approval_denial_alone_is_approval_denied() {
        let mut outcome = RunOutcome::default();
        outcome.record_approval_denied();
        assert_eq!(outcome.exit_code(), ExitCode::ApprovalDenied);
    }

    #[test]
    fn general_error_takes_precedence_over_approval_denial() {
        let mut outcome = RunOutcome::default();
        outcome.record_approval_denied();
        outcome.record_root_error();
        assert_eq!(outcome.exit_code(), ExitCode::GeneralError);
    }

    #[test]
    fn interruption_takes_precedence_over_every_other_outcome() {
        let mut outcome = RunOutcome::default();
        outcome.record_approval_denied();
        outcome.record_root_error();
        outcome.record_interruption();
        assert_eq!(outcome.exit_code(), ExitCode::Interrupted);

        let mut interrupted_after_error = RunOutcome::default();
        interrupted_after_error.record_root_error();
        interrupted_after_error.record_interruption();
        assert_eq!(interrupted_after_error.exit_code(), ExitCode::Interrupted);
    }

    #[test]
    fn stream_cancelled_update_alone_is_not_an_interruption() {
        let mut outcome = RunOutcome::default();
        outcome.record_update(&UiUpdate::StreamCancelled);
        assert_eq!(outcome.exit_code(), ExitCode::Success);
    }

    #[test]
    fn root_error_update_records_a_general_error() {
        let mut outcome = RunOutcome::default();
        outcome.record_update(&root_error());
        assert_eq!(outcome.exit_code(), ExitCode::GeneralError);
    }

    #[test]
    fn subagent_error_update_does_not_affect_the_outcome() {
        let mut outcome = RunOutcome::default();
        outcome.record_update(&UiUpdate::Error {
            message: "subagent failed".into(),
            run_id: Some(Uuid::nil()),
        });
        assert_eq!(outcome, RunOutcome::default());
    }

    #[test]
    fn tool_results_never_set_the_outcome() {
        let mut outcome = RunOutcome::default();
        outcome.record_update(&UiUpdate::ToolError {
            id: Uuid::nil(),
            error: "bash exited with status 1".into(),
            duration: std::time::Duration::ZERO,
            content_sections: Vec::new(),
            run_id: None,
        });
        outcome.record_update(&UiUpdate::ToolKilled {
            id: Uuid::nil(),
            run_id: None,
        });
        assert_eq!(outcome.exit_code(), ExitCode::Success);
    }
}
