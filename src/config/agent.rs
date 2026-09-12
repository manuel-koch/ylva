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

use serde::Deserialize;

/// Per-agent configuration within a profile.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Name of the engine to use for this agent type. When `None`, resolves to
    /// `ProfileConfig::default_engine`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Optional model-facing summary of the tasks this agent type is suited
    /// for. The root agent receives this in the `spawn_subagent` tool
    /// description when choosing a child type.
    #[serde(default)]
    pub description: Option<String>,
    /// Subset of tool names the agent is permitted to invoke. When `None`, all
    /// registered tools are available. Tool filtering is implemented in Phase 6.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Maximum wall-clock seconds the agent may run before being cancelled.
    /// `None` means no timeout is enforced.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Optional persona instruction prepended to the system prompt.
    #[serde(default)]
    pub persona: Option<String>,
    /// When `true` the agent is permitted to spawn child agents via
    /// `AgentFactory::create` with `Role::Child`. Checked in Phase 6.
    #[serde(default)]
    pub can_spawn: bool,
}
