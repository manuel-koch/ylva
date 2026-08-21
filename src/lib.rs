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
pub mod build_info;
pub mod events;
pub mod headless_runner;
pub mod llm;
pub mod logging;
pub mod tui_runner;
pub mod ui;

#[cfg(test)]
mod test_support;

pub use logging::{init_logging, LogConfig, LogFormat, LogGuards};

pub use agent::tools::{default_registry, Tool, ToolError, ToolOutput, ToolRegistry};
pub use agent::{
    AgentCommand, AgentEvent, AgentLoop, AgentMode, AssembledToolCall, AssistantStatus, Context,
    DeltaChunk, Message, PendingTool, StreamEvent, ToolCallRequest, UiUpdate, UserMessageKind,
    DEFAULT_SYSTEM_PROMPT, DEFAULT_TOOL_TIMEOUT_SECS, MAX_ITERATIONS, TOOL_PROTOCOL_SYSTEM_PROMPT,
};
pub use app::{ChatEntry, ChatSelection, ToolBubbleState, TuiState, UserMessageState};
pub use build_info::{BuildInfo, BUILD_INFO};
pub use events::AppAction;
pub use headless_runner::{run_headless, run_headless_to};
pub use llm::{MockEvent, MockProvider, OpenAiProvider, Provider, ToolSchema};
