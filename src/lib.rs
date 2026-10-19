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

pub mod agent;
pub(crate) mod agent_factory;
pub(crate) mod app;
pub(crate) mod bounded_buffer;
pub(crate) mod build_info;
pub(crate) mod cli;
pub mod cli_driver;
pub mod config;
pub(crate) mod conversation_render_cache;
pub(crate) mod events;
pub(crate) mod exit_code;
pub(crate) mod highlight;
pub(crate) mod home_relative_path;
pub(crate) mod http;
pub(crate) mod init_command;
pub mod llm;
pub mod logging;
pub(crate) mod markdown;
pub mod presenter;
pub(crate) mod private_fs;
pub mod profile_command;
pub mod profile_shim;
pub(crate) mod prompt;
pub mod render;
pub(crate) mod run_outcome;
pub(crate) mod sandbox;
pub(crate) mod saturating_cast;
pub mod session_command;
pub(crate) mod session_title;
pub mod skill_command;
pub mod skills;
pub(crate) mod slash_command;
pub mod startup;
pub(crate) mod stop_signal;
pub mod store;
pub(crate) mod theme;
pub mod tui_driver;
pub mod tui_reducer_state;
pub mod tui_render_state;
pub mod tui_widgets;
pub(crate) mod ylva_home;

/// Git metadata entry, which may be a directory or a worktree marker file.
pub const GIT_ENTRY_NAME: &str = ".git";
/// Finder's per-directory metadata file on macOS.
pub const DS_STORE_FILE_NAME: &str = ".DS_Store";

/// Shared YAML parsing options: `no_schema` keeps unquoted numbers, booleans
/// and nulls from being coerced into strings; `with_snippet` off keeps errors
/// single-line.
pub(crate) fn yaml_options() -> serde_saphyr::Options {
    let mut options = serde_saphyr::Options::default();
    options.no_schema = true;
    options.with_snippet = false;
    options
}

#[cfg(test)]
mod test_support;

pub use logging::{init_logging, LogConfig, LogFormat, LogGuards};
pub use run_outcome::RunOutcome;

pub use agent::subagent_manager;
pub use agent::tools::{
    default_registry, spawn_subagent::AgentTypeInfo, AutoApproveTool, Tool, ToolCall,
    ToolCallResult, ToolError, ToolRegistry, ToolRegistryConfig, DEFAULT_CLI_TOOLS,
    DEFAULT_TUI_TOOLS,
};
pub use agent::{
    AgentCommand, AgentEvent, AgentLoop, AgentMode, AssembledToolCall, AssistantStatus, Context,
    ContextUsageSource, DeltaChunk, Message, PendingTool, Role, StreamEvent, SubagentRoleConfig,
    ToolAttachment, ToolCallRequest, ToolResult, UiUpdate, UserMessageKind,
    DEFAULT_MAX_TOOL_USE_ITERATIONS, DEFAULT_TOOL_TIMEOUT_SECS, MAX_TOOL_CALLS_PER_RESPONSE,
    TOOL_PROTOCOL_SYSTEM_PROMPT,
};
pub use agent_factory::AgentFactory;
pub use app::App;
pub use build_info::{BuildInfo, BUILD_INFO};
pub use cli::{Cli, CliColorMode, CliCommand, SkillCommand};
pub use cli_driver::{run_cli, run_cli_to, run_cli_to_with_options, CliOutputOptions, ConsoleApp};
pub use config::compaction_prompt::{load_compaction_prompt, DEFAULT_COMPACTION_PROMPT};
pub use config::instructions::{
    compose_root_system_prompt, load_trusted_instructions, InstructionFile,
    INSTRUCTION_SAFETY_PREAMBLE, MAX_INSTRUCTION_FILE_BYTES, TRUSTED_INSTRUCTION_FILE_NAMES,
};
pub use config::layered::{
    current_workspace_root, discover_workspace_root, ConfigLayer, ResolvedFile,
};
pub use config::soul::{load_soul, DEFAULT_SOUL};
pub use config::{
    default_profiles_root, load_profile_environment, load_profile_environment_at_workspace_root,
    profile_config_path, profile_config_source_path, profile_env_path, AgentCompactionConfig,
    AgentConfig, CompactionConfig, ConfigError, ProfileConfig, ProviderConfig,
    ProviderConnectionConfig, WebSearchConfig, DEFAULT_PROFILE_NAME, PROFILES_DIRECTORY_NAME,
    PROFILE_CONFIG_FILE_NAME, PROFILE_ENV_FILE_NAME,
};
pub use events::AppAction;
pub use exit_code::ExitCode;
pub use highlight::{active_theme, set_active_theme, ThemeName};
pub use init_command::{run_init, run_init_workspace, InitReport};
pub use llm::engine;
pub use llm::engine::{
    CompactionEngineDisplayInfo, Engine, EngineConfig, EngineDisplayInfo, EngineImpl, EngineRouter,
    EngineRuntimeConfig, EngineRuntimeConfigHandle, ModelSelectionTarget, ProviderModelOption,
    DEFAULT_CONTEXT_LENGTH,
};
pub use llm::{
    InputModality, MockEvent, MockProvider, ModelInfo, OllamaProvider, OpenAiCompatibleProvider,
    OpenRouterProvider, Provider, ToolSchema,
};
pub use presenter::{TextareaContext, TuiEvent};
pub use sandbox::{ApprovalCheck, Sandbox, SandboxContext};
pub use slash_command::{
    parse_slash_command, parse_slash_command_invocation, slash_command_suggestions, SlashCommand,
    SlashCommandInfo, SlashCommandInvocation, SLASH_COMMANDS,
};
pub use store::{
    default_chats_root, project_key, session_path, SessionMeta, SessionState, SessionSummary,
    Store, StoreError, StoreEvent,
};
pub use subagent_manager::{FinishResult, SpawnRequest, SpawnStatus, SubagentManager};
pub use tui_driver::TuiApp;
pub use tui_reducer_state::{
    ActiveSubagentCounts, AppLayout, AppState, ChatArea, ChatEntry, ChatSelection, ContextUsage,
    DomainEvent, Effect, Focus, HelpPopupLayout, InputAreaLayout, InputEvent, SlashOverlayLayout,
    ToolCallState, TuiDisplayMetadata, TuiMode, UserMessageState,
};
pub use tui_render_state::TuiState;
pub use ylva_home::{ylva_home, YLVA_DIR_NAME, YLVA_HOME_ENVIRONMENT_VARIABLE};
