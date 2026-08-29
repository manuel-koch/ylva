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
pub mod app_state;
pub mod build_info;
pub mod cli_driver;
pub mod config;
pub mod engine;
pub mod events;
pub mod llm;
pub mod logging;
pub mod presenter;
pub mod render;
pub mod sandbox;
pub mod tui_driver;
pub mod ui;

#[cfg(test)]
mod test_support;

pub use logging::{init_logging, LogConfig, LogFormat, LogGuards};

pub use agent::tools::{
    default_registry, Tool, ToolCall, ToolCallResult, ToolError, ToolRegistry, ToolRegistryConfig,
    DEFAULT_CLI_TOOLS, DEFAULT_TUI_TOOLS,
};
pub use agent::{
    AgentCommand, AgentEvent, AgentLoop, AgentMode, AssembledToolCall, AssistantStatus, Context,
    DeltaChunk, Message, PendingTool, Role, StreamEvent, ToolCallRequest, UiUpdate,
    UserMessageKind, DEFAULT_SYSTEM_PROMPT, DEFAULT_TOOL_TIMEOUT_SECS, MAX_TOOL_USE_ITERATIONS,
    TOOL_PROTOCOL_SYSTEM_PROMPT,
};
pub use agent_factory::AgentFactory;
pub use app::App;
pub use app_state::{
    AppLayout, AppState, ChatArea, ChatEntry, ChatSelection, DomainEvent, Effect, Focus,
    HelpPopupLayout, InputAreaLayout, InputEvent, ToolCallState, TuiMode, TuiState,
    UserMessageState,
};
pub use build_info::{BuildInfo, BUILD_INFO};
pub use cli_driver::{run_cli, run_cli_to, ConsoleApp};
pub use config::{AgentConfig, ProfileConfig};
pub use engine::{Engine, EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH};
pub use events::AppAction;
pub use llm::{MockEvent, MockProvider, ModelInfo, OpenAiProvider, Provider, ToolSchema};
pub use presenter::{TextareaContext, TuiEvent};
pub use sandbox::{ApprovalCheck, Sandbox, SandboxContext};
pub use tui_driver::TuiApp;
