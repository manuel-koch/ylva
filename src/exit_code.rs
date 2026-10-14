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

//! Process exit codes for terminal outcomes.
//!
//! The table in [`ExitCode`] is the single source of truth. `docs/cli-options.md`
//! documents it, and the `--help` epilog is built from [`ExitCode::ALL`] and
//! [`ExitCode::description`], so the listed codes cannot drift from the enum.

/// Exit code of a finished `ylva` process.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// The run finished without a recorded failure, interruption, or denied approval.
    Success = 0,
    /// General execution error, such as a provider or stream failure, or a
    /// partial session-maintenance failure.
    GeneralError = 1,
    /// Usage or startup error. No run started.
    UsageError = 2,
    /// An approval was required and denied.
    ApprovalDenied = 3,
    /// The user interrupted the run (SIGINT), following the shell convention `128 + 2`.
    Interrupted = 130,
}

impl ExitCode {
    /// Every code, in the order the help epilog lists them.
    pub const ALL: [ExitCode; 5] = [
        ExitCode::Success,
        ExitCode::GeneralError,
        ExitCode::UsageError,
        ExitCode::ApprovalDenied,
        ExitCode::Interrupted,
    ];

    /// Meaning of this code, shown in the `--help` epilog.
    pub const fn description(self) -> &'static str {
        match self {
            ExitCode::Success => "Successful run",
            ExitCode::GeneralError => "General execution error",
            ExitCode::UsageError => "Usage or startup error; no run started",
            ExitCode::ApprovalDenied => "Approval required but denied",
            ExitCode::Interrupted => "User interruption (SIGINT)",
        }
    }
}

impl From<ExitCode> for u8 {
    fn from(code: ExitCode) -> Self {
        code as u8
    }
}

/// Exit-code section appended to `--help`, built from [`ExitCode::ALL`].
pub fn help_epilog() -> String {
    let rows: Vec<String> = ExitCode::ALL
        .iter()
        .map(|code| format!("  {:<4}{}", u8::from(*code), code.description()))
        .collect();
    format!("Exit codes:\n{}", rows.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_pins_its_documented_numeric_value() {
        assert_eq!(u8::from(ExitCode::Success), 0);
        assert_eq!(u8::from(ExitCode::GeneralError), 1);
        assert_eq!(u8::from(ExitCode::UsageError), 2);
        assert_eq!(u8::from(ExitCode::ApprovalDenied), 3);
        assert_eq!(u8::from(ExitCode::Interrupted), 130);
    }

    #[test]
    fn all_lists_every_variant_once_with_distinct_values() {
        let mut values: Vec<u8> = ExitCode::ALL.iter().map(|code| u8::from(*code)).collect();
        values.sort_unstable();
        values.dedup();
        assert_eq!(values, vec![0, 1, 2, 3, 130]);
    }

    #[test]
    fn help_epilog_lists_each_code_with_its_description() {
        let epilog = help_epilog();
        for code in ExitCode::ALL {
            let row = format!("  {:<4}{}", u8::from(code), code.description());
            assert!(epilog.contains(&row), "epilog lacks {row:?}: {epilog}");
        }
    }
}
