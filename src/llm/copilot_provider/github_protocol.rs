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

//! Constants defined by GitHub's (partly undocumented) Copilot and OAuth
//! interfaces, not by Ylva. Changing any of them probably breaks the
//! communication with GitHub/Copilot (e.g. `403 token not authorized`).

pub const COPILOT_TOKEN_EXCHANGE_URL: &str = "https://api.github.com/copilot_internal/v2/token";
pub const COPILOT_FALLBACK_API_BASE: &str = "https://api.githubcopilot.com";
pub const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Client id of the VS Code Copilot Chat GitHub app.
pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

// Header values that mimic the VS Code Copilot Chat extension.
pub const INTEGRATION_ID: &str = "vscode-chat";
pub const EDITOR_VERSION: &str = "vscode/1.99.3";
pub const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
pub const USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
pub const OPENAI_INTENT: &str = "conversation-panel";
pub const GITHUB_API_VERSION: &str = "2025-04-01";
