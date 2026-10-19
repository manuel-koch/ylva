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

//! Initial user-turn selection for a run.

use std::io::IsTerminal;

/// Fallback demo prompt used when neither `--prompt` nor piped stdin supplies
/// the initial user turn.
const DEFAULT_DEMO_PROMPT: &str = "What makes the sky look blue?";

/// The explicitly supplied initial user turn, if any: `--prompt <text>`, else
/// piped stdin read to EOF. Returns `None` when neither was given (or stdin is
/// a terminal / whitespace-only). Used by the TUI, which starts with an empty
/// input box unless the user supplied a turn.
pub(super) fn explicit_prompt(prompt_from_flag: Option<String>) -> Option<String> {
    let prompt = prompt_from_flag.or_else(prompt_from_stdin)?;
    let trimmed = prompt.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The initial user turn sent to the agent in headless mode, resolved by
/// precedence: `--prompt <text>`, else piped stdin read to EOF, else the
/// hardcoded demo prompt. stdin is only consumed when it is **not** a terminal
/// (something is piped in); no interactive prompt-read is attempted in the
/// CLI-only phase. The resolved prompt is trimmed (leading/trailing whitespace
/// removed) before it is echoed and sent.
pub(super) fn initial_prompt(prompt_from_flag: Option<String>) -> String {
    explicit_prompt(prompt_from_flag).unwrap_or_else(|| DEFAULT_DEMO_PROMPT.to_string())
}

/// Read all of stdin when it is not a terminal. Returns `None` when stdin is a
/// terminal (no piped input to consume) or contains only whitespace.
fn prompt_from_stdin() -> Option<String> {
    use std::io::Read;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer).ok()?;
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_prompt_prefers_arg_over_default() {
        assert_eq!(initial_prompt(Some("  hi  ".to_string())), "hi");
    }

    #[test]
    fn explicit_prompt_returns_trimmed_arg() {
        assert_eq!(
            explicit_prompt(Some("  hi  ".to_string())),
            Some("hi".to_string())
        );
    }
}
