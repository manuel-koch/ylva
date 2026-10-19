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

//! `web_fetch` tool: fetch www content over HTTP `GET` through the
//! injectable [`WebFetchBackend`] seam.
//!
//! The tool never talks to the network itself — it receives an
//! `Arc<dyn WebFetchBackend>` from its constructor (built from the profile's
//! `[web_fetch]` section in `default_registry`), so tests substitute a stub
//! and the network stays caller-injected (`docs/known-test-issues.md`).
//! Text bodies (HTML converted to markdown, text/json/xml verbatim) are
//! returned in `text()`; recognized media (PNG/JPEG/WebP/GIF/PDF) is
//! returned as standard Base64 in `text()` plus a typed
//! [`ToolAttachment`] — the same shape `read` uses for local media files,
//! with `read`'s whole-file size ceiling and no truncation ever.
//!
//! Approval goes through the sandbox's network-tool rule
//! ([`crate::sandbox::NamedSandboxRule::NetworkRequest`]): user approval by
//! default, a `[tools.web_fetch]` rule to configure it per profile (its
//! optional `target` pattern is matched against the requested URL's
//! `scheme://host[:port]` key), and `--auto-approve-web-fetch` as the appended
//! catch-all. The model-facing description is static; only
//! [`WebFetchTool::title`] (user-facing) hints at the configured backend.

use std::sync::Arc;

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;

use super::backends::web_fetch::{
    network_target, WebFetchBackend, WebFetchBackendKind, WEB_FETCH_MAX_OUTPUT_BYTES,
    WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS,
};
use super::{read::READ_MEDIA_MAX_BYTES, title_case_tool_name, Tool, ToolCall};
use crate::config::WebFetchConfig;
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Model-facing tool description. Deliberately backend-agnostic (only
/// [`WebFetchTool::title`] hints at the configured backend). Built from
/// [`READ_MEDIA_MAX_BYTES`] and [`WEB_FETCH_MAX_OUTPUT_BYTES`] so the
/// advertised caps cannot drift.
static WEB_FETCH_DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "Fetch the web content at `url` (HTTP GET) and return it as text: \
         HTML is converted to markdown, text/JSON/XML is returned verbatim. \
         Recognized media (PNG, JPEG, WebP, GIF, PDF) is returned as a \
         Base64 attachment like a whole-file `read` — complete file only, \
         up to {media_max} bytes (a larger response fails, never a \
         truncated attachment). Text output is capped at {output_max} \
         bytes; longer output is truncated and flagged (`truncated` \
         metadata, `Partial result` label). Only http:// and https:// \
         URLs are accepted.",
        media_max = READ_MEDIA_MAX_BYTES,
        output_max = WEB_FETCH_MAX_OUTPUT_BYTES,
    )
});

// ── Argument schema ───────────────────────────────────────────────────────────

/// Arguments accepted by the `web_fetch` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct WebFetchInputArgs {
    /// Absolute URL to fetch, e.g. `https://docs.rs/htmd/0.5.5/htmd/`. Only
    /// `http://` and `https://` URLs are accepted; redirects are followed.
    pub url: String,
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `web_fetch` tool: fetch www content via a configurable backend.
#[derive(Debug, Clone)]
pub struct WebFetchTool {
    /// Outer timeout (seconds): internal HTTP timeout + grace (see
    /// [`WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS`]).
    timeout_secs: u64,
    /// Human-readable name of the configured backend, shown in the
    /// user-facing [`Tool::title`].
    backend_display_name: String,
    /// The backend this tool fetches through (constructor-injected so tests
    /// can substitute a stub).
    backend: Arc<dyn WebFetchBackend>,
    /// Network-tool approval policy: `[tools.web_fetch]` rules plus the
    /// `--auto-approve-web-fetch` catch-all.
    sandbox_context: SandboxContext,
}

impl WebFetchTool {
    pub const NAME: &'static str = "web_fetch";

    /// Build the tool from the profile's `[web_fetch]` section plus the
    /// sandbox context `default_registry` resolved for it (configured
    /// `[tools.web_fetch]` rules and, when `--auto-approve-web-fetch` was
    /// given, its catch-all rule). The caller passes the backend instance so
    /// tests can substitute a stub.
    pub fn new(
        backend: Arc<dyn WebFetchBackend>,
        config: &WebFetchConfig,
        sandbox_context: SandboxContext,
    ) -> Self {
        let backend_display_name = WebFetchBackendKind::from_name(&config.backend)
            .map(|kind| kind.display_name().to_string())
            .unwrap_or_else(|| config.backend.clone());
        WebFetchTool {
            timeout_secs: config.http_timeout().as_secs() + WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS,
            backend_display_name,
            backend,
            sandbox_context,
        }
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    /// User-facing label: `Web Fetch (Native)` — the parenthetical hints at
    /// *how* content is fetched (the configured backend). The model never
    /// sees this label; it receives [`WEB_FETCH_DESCRIPTION`].
    fn title(&self) -> String {
        format!(
            "{} ({})",
            title_case_tool_name(Self::NAME),
            self.backend_display_name
        )
    }

    fn description(&self) -> &str {
        &WEB_FETCH_DESCRIPTION
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(WebFetchInputArgs)
    }

    /// User approval by default: the sandbox abstains (no floor for network
    /// tools) until a `[tools.web_fetch]` `network_request` rule or the
    /// `--auto-approve-web-fetch` catch-all states an explicit verdict. A
    /// rule's optional `target` pattern is matched against the requested
    /// URL's `scheme://host[:port]` key. A missing/empty/non-`http(s)` `url` argument is
    /// auto-denied here (it can never become a request).
    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let Some(url) = arguments.get("url").and_then(|value| value.as_str()) else {
            return ApprovalCheck::AutoDenied {
                reason: "Required argument 'url' is missing or not a string".to_string(),
            };
        };
        if url.is_empty() {
            return ApprovalCheck::AutoDenied {
                reason: "Required argument 'url' must not be empty".to_string(),
            };
        }
        let target = match network_target(url) {
            Ok(target) => target,
            Err(error) => {
                return ApprovalCheck::AutoDenied {
                    reason: error.to_string(),
                }
            }
        };
        self.sandbox_context
            .check_network_request(&target)
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
        Box::new(WebFetchCall {
            id,
            raw_arguments: arguments,
            tool: self.clone(),
            runtime_output_token_limit: None,
        })
    }
}

mod call;
mod result;

pub use call::WebFetchCall;
pub use result::{WebFetchResult, WebFetchResultMetadata};

#[cfg(test)]
mod tests;
