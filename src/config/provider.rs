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

/// Provider implementation kinds accepted in `provider = "..."`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Mock,
    Ollama,
    Openrouter,
    Copilot,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Ollama => "ollama",
            Self::Openrouter => "openrouter",
            Self::Copilot => "copilot",
        }
    }
}

/// A named provider connection.
///
/// The map key in `ProfileConfig::provider_connections` is the connection name shown to
/// users and referenced by an engine. `provider` is the implementation kind;
/// several named connections may use the same kind with different endpoints or
/// credentials.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProviderConnectionConfig {
    /// Provider implementation kind, such as `"ollama"` or `"openrouter"`.
    pub provider: ProviderKind,
    /// Provider base URL. Ollama uses the server origin and derives its
    /// OpenAI-compatible `/v1` and native `/api` paths; OpenRouter uses its
    /// origin and derives the OpenAI-compatible `/api/v1` path. GitHub Copilot
    /// needs none: its API base is derived from the session token exchange.
    #[serde(default)]
    pub api_base_url: Option<String>,
    /// Optional credential for this connection.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Compatibility alias for code that used the shorter provider name.
pub type ProviderConfig = ProviderConnectionConfig;
