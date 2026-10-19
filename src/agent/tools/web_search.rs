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

//! `web_search` tool: web search through the injectable
//! [`WebSearchBackend`] seam.
//!
//! The tool never talks to the network itself — it receives an
//! `Arc<dyn WebSearchBackend>` from its constructor (built from the profile's
//! `[web_search]` section in `default_registry`), so tests substitute a stub
//! and the network stays caller-injected
//! (`docs/known-test-issues.md`). Results are handed back as a numbered
//! title+URL list in `text()` and as `{title, url, snippet}` entries in
//! `metadata()`, with snippets truncated to
//! [`WEB_SEARCH_SNIPPET_MAX_CHARS`].
//!
//! Approval goes through the sandbox's network-tool rule
//! ([`crate::sandbox::NamedSandboxRule::NetworkRequest`]): user approval by
//! default, a `[tools.web_search]` rule to configure it per profile, and
//! `--auto-approve-web-search` as the appended catch-all. The model-facing
//! [`WEB_SEARCH_DESCRIPTION`] is static — the LLM interface is identical
//! whichever backend serves the request; only [`WebSearchTool::title`]
//! (user-facing) hints at the configured backend.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;

use super::backends::web_search::{
    WebSearchBackend, WebSearchBackendKind, WEB_SEARCH_MAX_RESULT_LIMIT,
};
use super::{title_case_tool_name, Tool, ToolCall};
use crate::config::WebSearchConfig;
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Seconds added to the configured internal HTTP timeout to derive the
/// tool's outer timeout, so the outer safety net always fires *after* the
/// backend's own timeout. Sized to cover the worst-case throttle pre-delay
/// ([`WEB_SEARCH_THROTTLE_INTERVAL`] + [`WEB_SEARCH_THROTTLE_JITTER_FRACTION`]
/// of it, rounded up to whole seconds) plus dispatch slack.
pub const WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS: u64 = 10;

/// Minimum spacing between consecutive web-search request **starts**. A call
/// whose previous request started less than this ago sleeps out the remainder
/// plus jitter; calls already spaced farther apart are not delayed, and the
/// first request is never delayed.
pub const WEB_SEARCH_THROTTLE_INTERVAL: Duration = Duration::from_millis(5000);

/// Random extra delay added on top of a throttled wait: zero up to this
/// fraction of [`WEB_SEARCH_THROTTLE_INTERVAL`].
pub const WEB_SEARCH_THROTTLE_JITTER_FRACTION: f64 = 0.10;

/// Model-facing tool description. Deliberately backend-agnostic: the LLM
/// interface is the same whichever backend serves the request (only the
/// implementation *how* changes), so the backend belongs in the user-facing
/// [`WebSearchTool::title`], not here. `limit`'s default is described as
/// "the configured default" so the text stays true when
/// `[web_search] result_limit` overrides it. Built from
/// [`WEB_SEARCH_MAX_RESULT_LIMIT`] so the advertised cap cannot drift.
static WEB_SEARCH_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Search the web for `terms` and return ranked results: \
         `text` lists each title with its URL, and the structured \
         result metadata carries a snippet per result. `limit` \
         caps the number of results (max {}; omit it for the \
         configured default).",
        WEB_SEARCH_MAX_RESULT_LIMIT
    )
});

// ── Argument schema ───────────────────────────────────────────────────────────

/// Arguments accepted by the `web_search` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct WebSearchInputArgs {
    /// Search terms to look up on the web, e.g. `rust async traits`.
    pub terms: String,
    /// Maximum number of results to return, capped at
    /// [`super::backends::web_search::WEB_SEARCH_MAX_RESULT_LIMIT`]. Omit it for the
    /// configured default ([`super::backends::web_search::WEB_SEARCH_DEFAULT_RESULT_LIMIT`],
    /// overridable via `[web_search] result_limit`).
    ///
    /// The schema `maximum` and field description are injected in
    /// [`WebSearchTool::input_schema`] from
    /// [`WEB_SEARCH_MAX_RESULT_LIMIT`] so they cannot drift.
    #[serde(default)]
    pub limit: Option<usize>,
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `web_search` tool: web search via a configurable backend.
#[derive(Debug, Clone)]
pub struct WebSearchTool {
    /// Outer timeout (seconds): internal HTTP timeout + grace (see
    /// [`WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS`]).
    timeout_secs: u64,
    /// Result count used when the call omits `limit`.
    default_result_limit: usize,
    /// Human-readable name of the configured backend, shown in the
    /// user-facing [`Tool::title`].
    backend_display_name: String,
    /// The backend this tool searches through (constructor-injected so tests
    /// can substitute a stub).
    backend: Arc<dyn WebSearchBackend>,
    /// Network-tool approval policy: `[tools.web_search]` rules plus the
    /// `--auto-approve-web-search` catch-all.
    sandbox_context: SandboxContext,
    /// Start time of the most recent request, shared by every call this tool
    /// creates (`None` until the first request starts). The registry holds
    /// one tool instance per process, so the throttle is process-wide;
    /// tests build fresh tools, which keeps them isolated from each other.
    last_request_started: Arc<Mutex<Option<Instant>>>,
}

impl WebSearchTool {
    pub const NAME: &'static str = "web_search";

    /// Build the tool from the profile's `[web_search]` section plus the
    /// sandbox context `default_registry` resolved for it (configured
    /// `[tools.web_search]` rules and, when `--auto-approve-web-search` was
    /// given, its catch-all rule). The caller passes the backend instance so
    /// tests can substitute a stub.
    pub fn new(
        backend: Arc<dyn WebSearchBackend>,
        config: &WebSearchConfig,
        sandbox_context: SandboxContext,
    ) -> Self {
        let default_result_limit = config.effective_result_limit();
        let backend_display_name = WebSearchBackendKind::from_name(&config.backend)
            .map(|kind| kind.display_name().to_string())
            .unwrap_or_else(|| config.backend.clone());
        WebSearchTool {
            timeout_secs: config.http_timeout().as_secs() + WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS,
            default_result_limit,
            backend_display_name,
            backend,
            sandbox_context,
            last_request_started: Arc::new(Mutex::new(None)),
        }
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    /// User-facing label: `Web Search (DuckDuckGo)` — the parenthetical hints
    /// at *how* results are obtained (the configured backend). The model
    /// never sees this label; it receives [`WEB_SEARCH_DESCRIPTION`].
    fn title(&self) -> String {
        format!(
            "{} ({})",
            title_case_tool_name(Self::NAME),
            self.backend_display_name
        )
    }

    fn description(&self) -> &str {
        &WEB_SEARCH_DESCRIPTION
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        let mut schema = schema_for!(WebSearchInputArgs);
        if let Some(object) = schema.schema.object.as_mut() {
            if let Some(schemars::schema::Schema::Object(limit)) =
                object.properties.get_mut("limit")
            {
                limit.number().maximum = Some(WEB_SEARCH_MAX_RESULT_LIMIT as f64);
                limit.metadata().description = Some(format!(
                    "Maximum number of results to return (max {}; omit it for the configured default).",
                    WEB_SEARCH_MAX_RESULT_LIMIT
                ));
            }
        }
        schema
    }

    /// User approval by default: the sandbox abstains (no floor for network
    /// tools) until a `[tools.web_search]` `network_request` rule or the
    /// `--auto-approve-web-search` catch-all states an explicit verdict. A
    /// rule's optional `target` pattern is matched against the configured
    /// backend endpoint's `scheme://host[:port]` key. The tool has neither path nor
    /// command arguments, so no path/command rule can ever apply to it.
    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        self.sandbox_context
            .check_network_request(&self.backend.network_target())
            .unwrap_or(ApprovalCheck::UserApprovalRequired)
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(WebSearchCall {
            id,
            raw_arguments: arguments,
            tool: self.clone(),
            runtime_output_token_limit: None,
        })
    }
}

mod call;
mod result;
mod throttle;

pub use call::WebSearchCall;
pub use result::WebSearchResult;

#[cfg(test)]
mod tests;
