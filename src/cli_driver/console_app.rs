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

//! Headless application host wiring an [`AgentLoop`] to the CLI runner.

use std::io::Write;
use std::sync::Arc;

use tracing::Instrument;

use super::output::CliOutputOptions;
use super::run::run_cli_to_with_title;
use super::title_driver::TitleDriver;
#[cfg(test)]
use crate::agent::tools::ToolRegistryConfig;
use crate::agent::Role;
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::exit_code::ExitCode;
use crate::session_title::TitleAttempt;

/// Top-level headless (non-terminal) application entry point.
///
/// Constructs an [`AgentLoop`] via [`AgentFactory`] and delegates to
/// [`run_cli_to`] with the configured writer.
pub struct ConsoleApp {
    factory: Arc<AgentFactory>,
    profile: ProfileConfig,
    agent_type: String,
    prompt: String,
    writer: Box<dyn Write + Send>,
    output_options: CliOutputOptions,
    exit_snapshot: Option<crate::app::SessionExitHandle>,
}

impl ConsoleApp {
    pub fn new(
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
        agent_type: String,
        prompt: String,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        ConsoleApp {
            factory,
            profile,
            agent_type,
            prompt,
            writer,
            output_options: CliOutputOptions::plain(),
            exit_snapshot: None,
        }
    }

    /// Enable or disable ANSI colors for the CLI status/header lines.
    pub fn with_color_output(mut self, color: bool) -> Self {
        self.output_options = CliOutputOptions::new(color);
        self
    }

    pub fn with_exit_snapshot(mut self, snapshot: crate::app::SessionExitHandle) -> Self {
        self.exit_snapshot = Some(snapshot);
        self
    }
}

/// Profile-level registry configuration for CLI display tests.
/// Production uses the executing agent's registry directly.
#[cfg(test)]
pub(super) fn display_registry_config(profile: &ProfileConfig) -> ToolRegistryConfig {
    let tool_sandbox_rules = profile
        .tools
        .iter()
        .filter_map(|(tool_name, tool_config)| {
            Some((tool_name.clone(), tool_config.rules.clone()?))
        })
        .collect();
    ToolRegistryConfig {
        tool_sandbox_rules,
        bash_timeout_secs: profile
            .effective_tool_timeout_secs(None, "bash")
            .unwrap_or(crate::agent::tools::bash::DEFAULT_BASH_TIMEOUT_SECS),
        web_search_timeout_secs: profile
            .effective_tool_timeout_secs(None, "web_search")
            .unwrap_or(crate::config::DEFAULT_WEB_SEARCH_TIMEOUT_SECS),
        web_fetch_timeout_secs: profile
            .effective_tool_timeout_secs(None, "web_fetch")
            .unwrap_or(crate::config::DEFAULT_WEB_FETCH_TIMEOUT_SECS),
        web_search: profile.web_search.clone(),
        web_fetch: profile.web_fetch.clone(),
        ..ToolRegistryConfig::default()
    }
}

impl App for ConsoleApp {
    async fn run(mut self) -> anyhow::Result<u8> {
        if let Some(notice) = self.profile.config_warnings_notice() {
            eprintln!("warning: {notice}");
        }
        let (agent, ui_rx) = self
            .factory
            .create(&self.profile, &self.agent_type, Role::Root)?;
        let registry = agent.registry().clone();
        let _exit_guard = agent.exit_guard(self.exit_snapshot);
        let session_span = agent
            .store()
            .map(|store| store.span())
            .unwrap_or_else(tracing::Span::current);
        let title_driver = Some(TitleDriver {
            factory: Arc::clone(&self.factory),
            profile: self.profile.clone(),
            attempt: TitleAttempt::default(),
            pending_tasks: std::sync::Mutex::new(Vec::new()),
        });
        match run_cli_to_with_title(
            agent,
            ui_rx,
            registry,
            self.prompt,
            &mut *self.writer,
            self.output_options,
            title_driver,
        )
        .instrument(session_span)
        .await
        {
            Ok(outcome) => {
                tracing::debug!(?outcome, "headless run finished");
                Ok(u8::from(outcome.exit_code()))
            }
            // The run had already started, so a failure while driving it is a
            // general execution error, not a startup error (exit code 2).
            Err(error) => {
                tracing::error!(%error, "headless run failed");
                eprintln!("error: {error}");
                Ok(u8::from(ExitCode::GeneralError))
            }
        }
    }
}
