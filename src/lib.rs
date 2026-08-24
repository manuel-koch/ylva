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
pub mod app;
pub mod app_state;
pub mod build_info;
pub mod cli_driver;
pub mod events;
pub mod llm;
pub mod logging;
pub mod presenter;
pub mod render;
pub mod tui_driver;
pub mod ui;

#[cfg(test)]
mod test_support;

pub use logging::{init_logging, LogConfig, LogFormat, LogGuards};

pub use agent::tools::{default_registry, Tool, ToolCall, ToolCallResult, ToolError, ToolRegistry};
pub use agent::{
    AgentCommand, AgentEvent, AgentLoop, AgentMode, AssembledToolCall, AssistantStatus, Context,
    DeltaChunk, Message, PendingTool, StreamEvent, ToolCallRequest, UiUpdate, UserMessageKind,
    DEFAULT_SYSTEM_PROMPT, DEFAULT_TOOL_TIMEOUT_SECS, MAX_TOOL_USE_ITERATIONS,
    TOOL_PROTOCOL_SYSTEM_PROMPT,
};
pub use app::{ChatEntry, ChatSelection, ToolCallState, TuiState, UserMessageState};
pub use build_info::{BuildInfo, BUILD_INFO};
pub use cli_driver::{run_cli, run_cli_to};
pub use events::AppAction;
pub use llm::{MockEvent, MockProvider, OpenAiProvider, Provider, ToolSchema};
