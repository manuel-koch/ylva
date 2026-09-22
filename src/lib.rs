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
pub mod agent_factory;
pub mod app;
pub mod build_info;
pub mod cli_driver;
pub mod config;
pub mod events;
pub mod highlight;
pub mod init_command;
pub mod llm;
pub mod logging;
pub mod markdown;
pub mod presenter;
pub mod render;
pub mod sandbox;
pub(crate) mod session_title;
pub mod slash_command;
pub mod store;
pub(crate) mod theme;
pub mod tui_driver;
pub mod tui_reducer_state;
pub mod tui_render_state;
pub mod tui_widgets;
pub mod ylva_home;

#[cfg(test)]
mod test_support;

pub use logging::{init_logging, LogConfig, LogFormat, LogGuards};

pub use agent::child_agent_manager;
pub use agent::tools::{
    default_registry, spawn_subagent::AgentTypeInfo, AutoApproveTool, Tool, ToolCall,
    ToolCallResult, ToolError, ToolRegistry, ToolRegistryConfig, DEFAULT_CLI_TOOLS,
    DEFAULT_TUI_TOOLS,
};
pub use agent::{
    AgentCommand, AgentEvent, AgentLoop, AgentMode, AssembledToolCall, AssistantStatus,
    ChildRoleConfig, Context, DeltaChunk, Message, PendingTool, Role, StreamEvent, ToolAttachment,
    ToolCallRequest, ToolResult, UiUpdate, UserMessageKind, DEFAULT_TOOL_TIMEOUT_SECS,
    MAX_TOOL_USE_ITERATIONS, TOOL_PROTOCOL_SYSTEM_PROMPT,
};
pub use agent_factory::AgentFactory;
pub use app::App;
pub use build_info::{BuildInfo, BUILD_INFO};
pub use child_agent_manager::{ChildAgentManager, FinishResult, SpawnRequest, SpawnStatus};
pub use cli_driver::{run_cli, run_cli_to, run_cli_to_with_options, CliOutputOptions, ConsoleApp};
pub use config::layered::{
    current_workspace_root, discover_workspace_root, ConfigLayer, ResolvedFile,
};
pub use config::soul::{load_soul, DEFAULT_SOUL};
pub use config::{
    default_profiles_root, load_profile_environment, load_profile_environment_at_workspace_root,
    profile_config_path, profile_config_source_path, profile_env_path, AgentConfig, ConfigError,
    ProfileConfig, ProviderConfig, ProviderConnectionConfig, DEFAULT_PROFILE_NAME,
    PROFILES_DIRECTORY_NAME, PROFILE_CONFIG_FILE_NAME, PROFILE_ENV_FILE_NAME,
};
pub use events::AppAction;
pub use highlight::{active_theme, set_active_theme, ThemeName};
pub use init_command::{run_init, run_init_workspace, InitReport};
pub use llm::engine;
pub use llm::engine::{
    Engine, EngineConfig, EngineDisplayInfo, EngineImpl, EngineRouter, EngineRuntimeConfig,
    EngineRuntimeConfigHandle, ProviderModelOption, DEFAULT_CONTEXT_LENGTH,
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
    default_chats_root, project_key, session_path, SessionMeta, SessionSummary, Store, StoreError,
    StoreEvent,
};
pub use tui_driver::TuiApp;
pub use tui_reducer_state::{
    ActiveSubagentCounts, AppLayout, AppState, ChatArea, ChatEntry, ChatSelection, DomainEvent,
    Effect, Focus, HelpPopupLayout, InputAreaLayout, InputEvent, SlashOverlayLayout, ToolCallState,
    TuiDisplayMetadata, TuiMode, UserMessageState,
};
pub use tui_render_state::TuiState;
pub use ylva_home::{ylva_home, YLVA_DIR_NAME, YLVA_HOME_ENVIRONMENT_VARIABLE};
