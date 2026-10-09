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

//! Parsing of the TUI's slash commands; see `docs/features.md`.
//!
//! Deliberately a *closed* allowlist rather than an extensible registry: an
//! input line is a command only when it starts with `/` **as its very first
//! character** and its first word matches one of the names below exactly.
//! Everything else — `/foo`, `what about /models?`, a leading space — stays
//! ordinary chat text and is sent to the model unchanged, so the parser can
//! never swallow a user's message.

/// A recognized slash command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommand {
    /// List the provider's models and switch to the selected one.
    Models,
    /// Show the tools the running agent may call.
    Tools,
    /// Drop the in-memory conversation and start a fresh persisted session.
    Clear,
    /// Resume or clone another persisted root conversation.
    Session,
    /// Summarize a completed prefix of the root model view.
    Compact,
}

/// Static description of one entry in the slash-command allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommandInfo {
    /// The canonical, typed name including the leading slash.
    pub name: &'static str,
    /// Alternative spellings that select the same command.
    pub aliases: &'static [&'static str],
    /// One-line help shown in the suggestion overlay.
    pub description: &'static str,
    pub command: SlashCommand,
    /// Whether text after the command name is a valid argument rather than
    /// a parse error. Declared per-command so accepting one is never a
    /// hardcoded special case elsewhere in the parser.
    pub accepts_argument: bool,
}

/// A recognized slash command together with its optional argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommandInvocation {
    /// The command selected by the input.
    pub command: SlashCommand,
    /// The trimmed argument supplied after the command, if any.
    pub argument: Option<String>,
}

/// The complete allowlist, in the order the suggestion overlay lists them.
pub const SLASH_COMMANDS: &[SlashCommandInfo] = &[
    SlashCommandInfo {
        name: "/clear",
        aliases: &["/reset"],
        description: "Clear the conversation and start a new session",
        command: SlashCommand::Clear,
        accepts_argument: false,
    },
    SlashCommandInfo {
        name: "/models",
        aliases: &[],
        description: "List available models and switch the active one",
        command: SlashCommand::Models,
        accepts_argument: true,
    },
    SlashCommandInfo {
        name: "/tools",
        aliases: &[],
        description: "Show the tools the agent may call",
        command: SlashCommand::Tools,
        accepts_argument: false,
    },
    SlashCommandInfo {
        name: "/compact",
        aliases: &[],
        description: "Compact the root conversation without deleting its transcript",
        command: SlashCommand::Compact,
        accepts_argument: false,
    },
    SlashCommandInfo {
        name: "/session",
        aliases: &[],
        description: "Resume or clone a recent session",
        command: SlashCommand::Session,
        accepts_argument: false,
    },
];

/// Return the command `input` invokes, or `None` when `input` is ordinary chat.
///
/// `input` is the raw draft as typed. Trailing whitespace is tolerated because
/// it is invisible. Whether trailing text is accepted as an argument (versus
/// rejecting the whole input as unrecognized) is declared per-command via
/// [`SlashCommandInfo::accepts_argument`].
pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    parse_slash_command_invocation(input).map(|invocation| invocation.command)
}

/// Return the command and optional argument represented by `input`.
///
/// Only commands with `accepts_argument: true` (currently `/models`, whose
/// argument is passed to the model picker as a case-insensitive filter) may
/// carry trailing text; the rest remain exact no-argument commands.
pub fn parse_slash_command_invocation(input: &str) -> Option<SlashCommandInvocation> {
    if !starts_with_slash(input) {
        return None;
    }

    let mut parts = input.trim_end().splitn(2, char::is_whitespace);
    let word = parts.next()?;
    let argument = parts
        .next()
        .map(str::trim)
        .filter(|argument| !argument.is_empty())
        .map(str::to_owned);
    let info = SLASH_COMMANDS
        .iter()
        .find(|info| info.name == word || info.aliases.contains(&word))?;

    if argument.is_some() && !info.accepts_argument {
        return None;
    }

    Some(SlashCommandInvocation {
        command: info.command,
        argument,
    })
}

/// Return the commands whose name starts with the partially typed `input`.
///
/// Drives the overlay shown *while typing*: `/` lists everything, `/m` narrows
/// to `/models`, and a completed `/models` still matches itself so the overlay
/// stays visible until the line is submitted or edited away.
pub fn slash_command_suggestions(input: &str) -> Vec<SlashCommandInfo> {
    if !starts_with_slash(input) {
        return Vec::new();
    }
    let word = input.trim_end();
    if word.contains(char::is_whitespace) {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .filter(|info| {
            info.name.starts_with(word) || info.aliases.iter().any(|alias| alias.starts_with(word))
        })
        .copied()
        .collect()
}

/// Whether `input`'s very first character is the command sigil.
fn starts_with_slash(input: &str) -> bool {
    input.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_allowlisted_name_parses_to_its_command() {
        assert_eq!(parse_slash_command("/models"), Some(SlashCommand::Models));
        assert_eq!(parse_slash_command("/tools"), Some(SlashCommand::Tools));
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
        for (name, command) in [
            ("/session", SlashCommand::Session),
            ("/compact", SlashCommand::Compact),
        ] {
            assert_eq!(parse_slash_command(name), Some(command));
            assert_eq!(parse_slash_command(&format!("{name} \n")), Some(command));
            assert_eq!(parse_slash_command(&format!("{name} extra")), None);
            assert_eq!(parse_slash_command(&format!(" {name}")), None);
        }
        assert_eq!(
            slash_command_suggestions("/c")
                .iter()
                .map(|info| info.command)
                .collect::<Vec<_>>(),
            [SlashCommand::Clear, SlashCommand::Compact]
        );
    }

    #[test]
    fn reset_is_an_alias_for_clear() {
        assert_eq!(parse_slash_command("/reset"), Some(SlashCommand::Clear));
    }

    #[test]
    fn trailing_whitespace_is_tolerated() {
        assert_eq!(parse_slash_command("/tools  "), Some(SlashCommand::Tools));
        assert_eq!(parse_slash_command("/tools\n"), Some(SlashCommand::Tools));
    }

    #[test]
    fn an_unknown_command_stays_ordinary_chat() {
        assert_eq!(parse_slash_command("/foo"), None);
        assert_eq!(parse_slash_command("/modelsx"), None);
    }

    #[test]
    fn models_accepts_an_optional_filter_argument() {
        assert_eq!(
            parse_slash_command_invocation("/models gpt-4"),
            Some(SlashCommandInvocation {
                command: SlashCommand::Models,
                argument: Some("gpt-4".to_string()),
            })
        );
        assert_eq!(
            parse_slash_command("/models gpt-4"),
            Some(SlashCommand::Models)
        );
    }

    #[test]
    fn other_commands_reject_arguments() {
        assert_eq!(parse_slash_command("/tools extra"), None);
        assert_eq!(parse_slash_command("/clear extra"), None);
    }

    #[test]
    fn a_slash_that_is_not_the_first_character_stays_ordinary_chat() {
        assert_eq!(parse_slash_command(" /models"), None);
        assert_eq!(parse_slash_command("please run /models"), None);
        assert_eq!(parse_slash_command("path/to/tools"), None);
    }

    #[test]
    fn a_bare_slash_suggests_every_command() {
        let suggestions = slash_command_suggestions("/");
        assert_eq!(suggestions.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn a_prefix_narrows_the_suggestions() {
        let suggestions = slash_command_suggestions("/mo");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].command, SlashCommand::Models);
    }

    #[test]
    fn an_alias_prefix_suggests_its_command() {
        let suggestions = slash_command_suggestions("/re");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].command, SlashCommand::Clear);
    }

    #[test]
    fn ordinary_text_suggests_nothing() {
        assert!(slash_command_suggestions("hello").is_empty());
        assert!(slash_command_suggestions("/models please").is_empty());
        assert!(slash_command_suggestions("/zzz").is_empty());
    }
}
