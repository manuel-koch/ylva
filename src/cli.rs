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

//! Command-line argument parsing for the `ylva` binary.
//!
//! The derive model here is the single source of truth for every supported
//! flag; `main.rs` consumes the parsed [`Cli`] instead of scanning raw
//! arguments. `--version` is intentionally handled in `main()` before parsing
//! so the rich `BUILD_INFO` rendering pre-empts everything else.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

use crate::agent::tools::AutoApproveTool;
use crate::logging::{LogConfig, LogFormat};

/// Color policy for headless CLI status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliColorMode {
    /// Use colors when stdout is a terminal and the environment allows it.
    Auto,
    /// Never use colors.
    No,
    /// Always use colors.
    Yes,
}

/// Parsed command line for the `ylva` binary.
#[derive(Parser, Debug)]
#[command(about = "Ylva - Agent-Harness", disable_version_flag = true)]
pub struct Cli {
    /// Force headless mode instead of auto-detecting a TUI from stdout
    #[arg(long)]
    pub no_tui: bool,

    /// Color mode for headless CLI output: auto, no, or yes (last flag wins)
    #[arg(
        long,
        value_name = "mode",
        default_value = "auto",
        value_parser = parse_color_mode,
        overrides_with = "color",
    )]
    pub color: CliColorMode,

    /// Initial user prompt (piped stdin is used when this flag is absent;
    /// last flag wins)
    #[arg(long, value_name = "text", overrides_with = "prompt")]
    pub prompt: Option<String>,

    /// Configuration profile to load
    #[arg(long, value_name = "name")]
    pub profile: Option<String>,

    /// Write logs to this file (logging is off without it)
    #[arg(long, value_name = "path")]
    pub log_file: Option<PathBuf>,

    /// Minimum log level: trace, debug, info, warn, or error
    #[arg(
        long,
        value_name = "level",
        default_value = "info",
        value_parser = parse_log_level,
    )]
    pub log_level: tracing::Level,

    /// Log output format: text or json
    #[arg(
        long,
        value_name = "format",
        default_value = "text",
        value_parser = parse_log_format,
    )]
    pub log_format: LogFormat,

    /// Resume an existing session by its uuid
    #[arg(long, value_name = "uuid", value_parser = parse_conversation_id)]
    pub conversation_id: Option<Uuid>,

    /// Auto-approve read tool calls
    #[arg(long)]
    pub auto_approve_read: bool,

    /// Auto-approve write tool calls
    #[arg(long)]
    pub auto_approve_write: bool,

    /// Auto-approve glob tool calls
    #[arg(long)]
    pub auto_approve_glob: bool,

    /// Auto-approve bash tool calls
    #[arg(long)]
    pub auto_approve_bash: bool,

    /// Auto-approve search tool calls
    #[arg(long)]
    pub auto_approve_search: bool,

    /// Auto-approve edit tool calls
    #[arg(long)]
    pub auto_approve_edit: bool,

    /// Auto-approve web_search tool calls
    #[arg(long)]
    pub auto_approve_web_search: bool,

    /// Auto-approve spawning child agents
    #[arg(long)]
    pub auto_approve_spawn: bool,

    /// Hidden alias for the removed `--no-color` flag, rejected after parsing
    /// with a migration hint (see [`Cli::resolved_color_mode`]).
    #[arg(long, hide = true)]
    pub no_color: bool,

    /// Subcommands (`ylva init`)
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

/// Subcommands of the `ylva` binary.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum CliCommand {
    /// Seed Ylva's home or workspace starter files
    Init {
        /// Seed the current workspace instead of the Ylva home
        #[arg(long)]
        workspace: bool,
        /// Overwrite existing starter files
        #[arg(long)]
        force: bool,
    },
}

impl Cli {
    /// The logging configuration implied by the parsed log flags.
    ///
    /// Values are already validated by the clap value parsers, so this only
    /// assembles the [`LogConfig`].
    pub fn log_config(&self) -> LogConfig {
        LogConfig {
            log_file: self.log_file.clone(),
            log_level: self.log_level,
            log_format: self.log_format,
        }
    }

    /// The color mode selected on the command line.
    ///
    /// Returns the migration error for the removed `--no-color` flag so the
    /// exact historical hint survives the move to clap.
    pub fn resolved_color_mode(&self) -> Result<CliColorMode, String> {
        if self.no_color {
            return Err("--no-color was replaced by --color=no".to_string());
        }
        Ok(self.color)
    }

    /// The tools enabled for auto-approval via `--auto-approve-*` flags, in
    /// canonical order. `clarify` has no auto-approve concept — it always
    /// requires approval.
    pub fn auto_approve_tools(&self) -> Vec<AutoApproveTool> {
        let mut tools = Vec::new();
        if self.auto_approve_read {
            tools.push(AutoApproveTool::Read);
        }
        if self.auto_approve_write {
            tools.push(AutoApproveTool::Write);
        }
        if self.auto_approve_glob {
            tools.push(AutoApproveTool::Glob);
        }
        if self.auto_approve_bash {
            tools.push(AutoApproveTool::Bash);
        }
        if self.auto_approve_search {
            tools.push(AutoApproveTool::Search);
        }
        if self.auto_approve_edit {
            tools.push(AutoApproveTool::Edit);
        }
        if self.auto_approve_web_search {
            tools.push(AutoApproveTool::WebSearch);
        }
        if self.auto_approve_spawn {
            tools.push(AutoApproveTool::Spawn);
        }
        tools
    }
}

/// Parse a `--color` value case-insensitively.
fn parse_color_mode(value: &str) -> Result<CliColorMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Ok(CliColorMode::Auto),
        "no" => Ok(CliColorMode::No),
        "yes" => Ok(CliColorMode::Yes),
        _ => Err(format!(
            "invalid --color mode '{value}': expected auto, no, or yes"
        )),
    }
}

/// Parse a `--log-level` value case-insensitively.
fn parse_log_level(value: &str) -> Result<tracing::Level, String> {
    match value.to_ascii_lowercase().as_str() {
        "trace" => Ok(tracing::Level::TRACE),
        "debug" => Ok(tracing::Level::DEBUG),
        "info" => Ok(tracing::Level::INFO),
        "warn" => Ok(tracing::Level::WARN),
        "error" => Ok(tracing::Level::ERROR),
        _ => Err(format!(
            "invalid --log-level '{value}': expected trace, debug, info, warn, or error"
        )),
    }
}

/// Parse a `--log-format` value case-insensitively.
fn parse_log_format(value: &str) -> Result<LogFormat, String> {
    match value.to_ascii_lowercase().as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => Err(format!(
            "invalid --log-format '{value}': expected text or json"
        )),
    }
}

/// Parse `--conversation-id <uuid>`; an unparseable id is a hard error so a
/// resume attempt never silently degrades into a fresh session.
fn parse_conversation_id(value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|error| format!("invalid --conversation-id '{value}': {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `arguments` as if invoked as `ylva <arguments>`.
    fn parse<const N: usize>(arguments: [&str; N]) -> Result<Cli, clap::Error> {
        let mut full_arguments = Vec::with_capacity(N + 1);
        full_arguments.push("ylva");
        full_arguments.extend_from_slice(&arguments);
        Cli::try_parse_from(full_arguments)
    }

    #[test]
    fn profile_is_none_without_the_flag() {
        let cli = parse(["--no-tui", "--prompt", "hi"]).expect("parses");
        assert_eq!(cli.profile, None);
    }

    #[test]
    fn profile_returns_the_flags_value() {
        let cli = parse(["--profile", "work"]).expect("parses");
        assert_eq!(cli.profile.as_deref(), Some("work"));
    }

    #[test]
    fn profile_without_a_value_is_an_error() {
        let error = parse(["--profile"]).expect_err("a missing value must not be silently ignored");
        assert!(error.to_string().contains("--profile"), "{error}");
    }

    #[test]
    fn no_tui_is_absent_by_default_and_set_when_given() {
        assert!(!parse([]).expect("parses").no_tui);
        assert!(parse(["--no-tui"]).expect("parses").no_tui);
    }

    #[test]
    fn logging_defaults_to_no_file_info_and_text() {
        let config = parse([]).expect("parses").log_config();
        assert_eq!(config.log_file, None);
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn logging_accepts_mixed_case_values() {
        let config = parse(["--log-level", "iNFo", "--log-format", "Text"])
            .expect("mixed-case values accepted")
            .log_config();
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn logging_accepts_all_levels_case_insensitively() {
        for (raw, level) in [
            ("Trace", tracing::Level::TRACE),
            ("DEBUG", tracing::Level::DEBUG),
            ("InFo", tracing::Level::INFO),
            ("Warn", tracing::Level::WARN),
            ("Error", tracing::Level::ERROR),
        ] {
            let config = parse(["--log-level", raw])
                .expect("valid level")
                .log_config();
            assert_eq!(config.log_level, level, "for input {raw:?}");
        }
    }

    #[test]
    fn logging_accepts_json_case_insensitively() {
        let config = parse(["--log-format", "JSON"])
            .expect("valid format")
            .log_config();
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn logging_rejects_unknown_values() {
        let bad_level = parse(["--log-level", "verbose"]);
        assert!(bad_level
            .expect_err("unknown level is rejected")
            .to_string()
            .contains("expected trace, debug, info, warn, or error"));
        let bad_format = parse(["--log-format", "xml"]);
        assert!(bad_format
            .expect_err("unknown format is rejected")
            .to_string()
            .contains("expected text or json"));
    }

    #[test]
    fn logging_file_flag_takes_a_path() {
        let config = parse(["--log-file", "/tmp/ylva.log"])
            .expect("parses")
            .log_config();
        assert_eq!(
            config.log_file.as_deref(),
            Some(std::path::Path::new("/tmp/ylva.log"))
        );
    }

    #[test]
    fn logging_file_without_a_value_is_an_error() {
        parse(["--log-file"]).expect_err("a missing value must not be silently ignored");
    }

    #[test]
    fn conversation_id_is_absent_without_the_flag() {
        let cli = parse(["--no-tui"]).expect("parses");
        assert_eq!(cli.conversation_id, None);
    }

    #[test]
    fn conversation_id_is_parsed_from_the_flag() {
        let session_id = uuid::Uuid::new_v4();
        let cli =
            parse(["--conversation-id", &session_id.to_string()]).expect("a valid uuid parses");
        assert_eq!(cli.conversation_id, Some(session_id));
    }

    #[test]
    fn conversation_id_without_a_value_is_an_error() {
        let error =
            parse(["--conversation-id"]).expect_err("a missing value must not be silently ignored");
        assert!(error.to_string().contains("--conversation-id"), "{error}");
    }

    #[test]
    fn conversation_id_that_is_not_a_uuid_is_an_error() {
        let error = parse(["--conversation-id", "nonsense"])
            .expect_err("an unparseable id must not start a fresh session");
        assert!(
            error.to_string().contains("invalid --conversation-id"),
            "{error}"
        );
    }

    #[test]
    fn prompt_reads_value_after_flag() {
        let cli = parse(["--prompt", "hello"]).expect("parses");
        assert_eq!(cli.prompt, Some("hello".to_string()));
        let cli = parse(["--no-tui"]).expect("parses");
        assert_eq!(cli.prompt, None);
    }

    #[test]
    fn later_prompt_flags_override_earlier_prompts() {
        let cli = parse(["--prompt", "first", "--prompt", "second"]).expect("parses");
        assert_eq!(cli.prompt, Some("second".to_string()));
    }

    #[test]
    fn prompt_without_a_value_is_an_error() {
        parse(["--prompt"]).expect_err("a missing value must not be silently ignored");
    }

    #[test]
    fn color_mode_defaults_to_auto_and_parses_supported_modes() {
        assert_eq!(parse([]).expect("parses").color, CliColorMode::Auto);
        assert_eq!(
            parse(["--color=auto"]).expect("parses").color,
            CliColorMode::Auto
        );
        assert_eq!(
            parse(["--color=no"]).expect("parses").color,
            CliColorMode::No
        );
        assert_eq!(
            parse(["--color=yes"]).expect("parses").color,
            CliColorMode::Yes
        );
        assert_eq!(
            parse(["--color=NO"]).expect("parses").color,
            CliColorMode::No
        );
    }

    #[test]
    fn color_mode_rejects_invalid_syntax() {
        assert!(parse(["--color=maybe"])
            .expect_err("unknown mode should be rejected")
            .to_string()
            .contains("expected auto, no, or yes"));
        parse(["--color"]).expect_err("missing mode should be rejected");
    }

    #[test]
    fn later_color_mode_flags_override_earlier_modes() {
        let cli = parse(["--color=no", "--color=yes"]).expect("parses");
        assert_eq!(cli.color, CliColorMode::Yes);
    }

    #[test]
    fn removed_no_color_flag_reports_the_migration_hint() {
        let cli = parse(["--no-color"]).expect("hidden flag parses");
        assert_eq!(
            cli.resolved_color_mode()
                .expect_err("the removed flag must not be silently accepted"),
            "--no-color was replaced by --color=no"
        );
        assert_eq!(
            parse(["--color=no"])
                .expect("parses")
                .resolved_color_mode()
                .expect("valid mode"),
            CliColorMode::No
        );
    }

    #[test]
    fn help_is_rendered_for_the_help_flag() {
        let error = parse(["--help"]).expect_err("help is reported as an error kind");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(!error.use_stderr(), "help goes to stdout");
        let rendered = error.to_string();
        for expected in [
            "--no-tui",
            "--color",
            "--prompt",
            "--profile",
            "--log-file",
            "--log-level",
            "--log-format",
            "--conversation-id",
            "--auto-approve-read",
            "--auto-approve-spawn",
            "init",
        ] {
            assert!(rendered.contains(expected), "help must list {expected}");
        }
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let error = parse(["--definitely-not-a-flag"])
            .expect_err("unknown flags must not be silently ignored");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(
            error.to_string().contains("--definitely-not-a-flag"),
            "{error}"
        );
    }

    #[test]
    fn auto_approve_flags_are_absent_by_default_and_map_to_tools() {
        let cli = parse([]).expect("parses");
        assert!(cli.auto_approve_tools().is_empty());

        let cli = parse([
            "--auto-approve-read",
            "--auto-approve-write",
            "--auto-approve-glob",
            "--auto-approve-bash",
            "--auto-approve-search",
            "--auto-approve-edit",
            "--auto-approve-web-search",
            "--auto-approve-spawn",
        ])
        .expect("parses");
        assert_eq!(
            cli.auto_approve_tools(),
            vec![
                AutoApproveTool::Read,
                AutoApproveTool::Write,
                AutoApproveTool::Glob,
                AutoApproveTool::Bash,
                AutoApproveTool::Search,
                AutoApproveTool::Edit,
                AutoApproveTool::WebSearch,
                AutoApproveTool::Spawn,
            ]
        );
    }

    #[test]
    fn init_parses_as_a_subcommand() {
        let cli = parse(["init"]).expect("parses");
        assert_eq!(
            cli.command,
            Some(CliCommand::Init {
                workspace: false,
                force: false
            })
        );
    }

    #[test]
    fn init_accepts_its_flags() {
        let cli = parse(["init", "--workspace", "--force"]).expect("parses");
        assert_eq!(
            cli.command,
            Some(CliCommand::Init {
                workspace: true,
                force: true
            })
        );
    }

    #[test]
    fn init_is_accepted_after_global_flags() {
        let cli = parse(["--no-tui", "init", "--workspace"]).expect("parses");
        assert_eq!(
            cli.command,
            Some(CliCommand::Init {
                workspace: true,
                force: false
            })
        );
        assert!(cli.no_tui);
    }

    #[test]
    fn init_rejects_unknown_arguments() {
        parse(["init", "--bogus"]).expect_err("unknown init argument is rejected");
    }

    #[test]
    fn no_subcommand_parses_with_no_command() {
        let cli = parse(["--no-tui"]).expect("parses");
        assert_eq!(cli.command, None);
    }
}
