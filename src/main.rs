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

use clap::Parser;

use ylva::logging::init_logging;
use ylva::startup::{cli_color_enabled, run_init_command, version_report};
use ylva::{Cli, CliCommand, ExitCode};

fn main() {
    // `--version` pre-empts everything, including parse errors: the rich
    // BUILD_INFO rendering replaces clap's own version flag
    // (`disable_version_flag` on `Cli`).
    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--version")
    {
        println!("{}", version_report());
        return;
    }

    // Parse errors exit before logging is initialised, so there is no log
    // guard to flush on this path — mirroring the historical color/logging
    // validation order.
    let cli = match Cli::try_parse_from(std::env::args()) {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            eprintln!("{error}");
            exit_with(ExitCode::UsageError);
        }
        Err(error) => {
            print!("{error}");
            exit_with(ExitCode::Success);
        }
    };

    if let Some(CliCommand::Init { workspace, force }) = &cli.command {
        match run_init_command(*workspace, *force) {
            Ok(report) => println!("{}", report.summary()),
            Err(message) => {
                eprintln!("error: {message}");
                exit_with(ExitCode::UsageError);
            }
        }
        return;
    }

    let cli_color_mode = match cli.resolved_color_mode() {
        Ok(color_mode) => color_mode,
        Err(message) => {
            eprintln!("error: {message}");
            exit_with(ExitCode::UsageError);
        }
    };
    let log_config = cli.log_config();
    // The guard must outlive the whole run so buffered log events are flushed
    // on exit. Logging is opt-in: `init_logging` installs nothing and returns
    // `None` when no `--log-file` was given.
    let mut log_guard = match init_logging(log_config) {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("error: {message}");
            exit_with(ExitCode::UsageError);
        }
    };

    tracing::info!(
        version = ylva::BUILD_INFO.version,
        commit = ylva::BUILD_INFO.git_commit,
        built = ylva::BUILD_INFO.build_timestamp_utc,
        profile = ylva::BUILD_INFO.profile,
        "build info"
    );

    if let Some(CliCommand::Session { command }) = &cli.command {
        match ylva::session_command::run(command, &mut std::io::stdout(), &mut std::io::stderr()) {
            Ok(0) => return,
            Ok(code) => {
                drop(log_guard.take());
                std::process::exit(i32::from(code));
            }
            Err(error) => exit_with_error(&mut log_guard, error),
        }
    }

    if let Some(CliCommand::Profile { command }) = &cli.command {
        match ylva::profile_command::run(command, &mut std::io::stdout(), &mut std::io::stderr()) {
            Ok(0) => return,
            Ok(code) => {
                drop(log_guard.take());
                std::process::exit(i32::from(code));
            }
            Err(error) => exit_with_error(&mut log_guard, error),
        }
    }

    if let Some(CliCommand::Skill { command }) = &cli.command {
        let options = ylva::skill_command::SkillListOptions {
            color: cli_color_enabled(cli_color_mode),
            width: crossterm::terminal::size().map_or(
                ylva::skill_command::SkillListOptions::default().width,
                |(width, _)| usize::from(width),
            ),
        };
        match ylva::skill_command::run_with_options(
            command,
            options,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        ) {
            Ok(_) => return,
            Err(error) => {
                tracing::error!(%error, "skill command failed");
                eprintln!("error: {error}");
                drop(log_guard.take());
                exit_with(ExitCode::UsageError);
            }
        }
    }

    let tokio_runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
    let startup_run = match ylva::startup::run(&cli, cli_color_mode, &tokio_runtime) {
        Ok(startup_run) => startup_run,
        Err(message) => exit_with_error(&mut log_guard, message),
    };
    let final_span = startup_run
        .session_id
        .map(|session_id| tracing::info_span!(parent: None, "session", session_id = %session_id));
    let _final_scope = final_span.as_ref().map(tracing::Span::enter);
    match startup_run.result {
        Ok(exit_code) => {
            if exit_code != 0 {
                drop(log_guard.take());
                std::process::exit(i32::from(exit_code));
            }
        }
        Err(error) => {
            exit_with_error(&mut log_guard, error);
        }
    }
}

/// Report a fatal startup error after logging has been initialized.
///
/// `std::process::exit` skips destructors, so explicitly dropping the
/// non-blocking log guard is required before exiting or the diagnostic can
/// remain buffered and never reach the selected logfile.
fn exit_with_error(log_guard: &mut Option<ylva::LogGuards>, message: impl std::fmt::Display) -> ! {
    let message = message.to_string();
    tracing::error!(%message, "fatal error");
    eprintln!("error: {message}");
    drop(log_guard.take());
    exit_with(ExitCode::UsageError)
}

/// Terminate with a documented exit code. Callers drop any log guard first.
fn exit_with(code: ExitCode) -> ! {
    std::process::exit(i32::from(u8::from(code)))
}
