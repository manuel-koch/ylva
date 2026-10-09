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
use tracing::Instrument;

use super::backends::web_search::{
    WebSearchBackend, WebSearchBackendKind, WebSearchHit, WEB_SEARCH_MAX_RESULT_LIMIT,
    WEB_SEARCH_SNIPPET_MAX_CHARS,
};
use super::{
    title_case_tool_name, ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError,
};
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

// ── Result ────────────────────────────────────────────────────────────────────

/// Successful output from a `web_search` invocation.
#[derive(Debug)]
pub struct WebSearchResult {
    /// Ranked hits with snippets truncated to
    /// [`WEB_SEARCH_SNIPPET_MAX_CHARS`].
    hits: Vec<WebSearchHit>,
    /// Pre-formatted text for the LLM: a numbered title+URL list.
    text: String,
}

impl WebSearchResult {
    fn new(mut hits: Vec<WebSearchHit>) -> Self {
        for hit in &mut hits {
            hit.snippet = truncate_chars(&hit.snippet, WEB_SEARCH_SNIPPET_MAX_CHARS);
        }
        let text = Self::format_text(&hits);
        WebSearchResult { hits, text }
    }

    fn format_text(hits: &[WebSearchHit]) -> String {
        hits.iter()
            .enumerate()
            .map(|(index, hit)| format!("{}. {}\n   {}", index + 1, hit.title, hit.url))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drop the lowest-ranked hits until the text fits the per-request context
    /// allowance, keeping metadata and text consistent.
    fn fit_to_token_budget(&mut self, max_output_tokens: usize) {
        let estimator = crate::llm::TokenEstimator::Tiktoken;
        while estimator.estimate_tokens(&self.text) > max_output_tokens && self.hits.pop().is_some()
        {
            self.text = Self::format_text(&self.hits);
        }
    }
}

impl ToolCallResult for WebSearchResult {
    fn text(&self) -> String {
        self.text.clone()
    }

    fn metadata(&self) -> Option<Value> {
        serde_json::to_value(&self.hits).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schema_for!(Vec<WebSearchHit>))
    }

    fn content_sections(&self) -> Vec<Section> {
        if self.hits.is_empty() {
            return vec![];
        }
        vec![Section::Content(ContentSection {
            label: None,
            content: self.text.clone(),
            file_extension: None,
            line_number_offset: None,
        })]
    }
}

/// Cut `text` to at most `max_chars` Unicode scalar values without panicking
/// on multi-byte input.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

// ── Throttle ──────────────────────────────────────────────────────────────────

/// Delay before the next request may start, computed from how long ago the
/// previous request started: no request yet, or the interval already
/// elapsed → no delay; otherwise the remainder of the interval plus `jitter`.
fn throttle_delay(
    elapsed_since_last_start: Option<Duration>,
    interval: Duration,
    jitter: Duration,
) -> Duration {
    let Some(elapsed) = elapsed_since_last_start else {
        return Duration::ZERO;
    };
    if elapsed >= interval {
        Duration::ZERO
    } else {
        interval - elapsed + jitter
    }
}

/// The random component of a throttled wait: zero up to
/// [`WEB_SEARCH_THROTTLE_JITTER_FRACTION`] of `interval`.
fn throttle_jitter(interval: Duration) -> Duration {
    crate::http::jitter_duration(interval, WEB_SEARCH_THROTTLE_JITTER_FRACTION)
}

/// Gate one request start: sleep out the remainder of
/// [`WEB_SEARCH_THROTTLE_INTERVAL`] plus jitter when the previous request
/// started too recently, then record *this* request's start time.
///
/// The timestamp is recorded after the wait — i.e. at request start —
/// regardless of how the request turns out, so a failed search throttles the
/// next one exactly like a successful one. The sleep runs inside the tool's
/// `run()` future, so dropping that future (outer timeout or
/// `CancellationToken`) aborts it without extra cancellation code.
async fn wait_before_request(last_request_started: &Mutex<Option<Instant>>) {
    let delay = {
        let guard = last_request_started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_delay(
            guard.as_ref().map(|started| started.elapsed()),
            WEB_SEARCH_THROTTLE_INTERVAL,
            throttle_jitter(WEB_SEARCH_THROTTLE_INTERVAL),
        )
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let mut guard = last_request_started
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(Instant::now());
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `web_search` tool call.
///
/// Carries a clone of the creating tool so schema-derived argument ranking
/// and the tool's settings stay in sync without re-deriving them here.
pub struct WebSearchCall {
    id: uuid::Uuid,
    raw_arguments: Value,
    tool: WebSearchTool,
    runtime_output_token_limit: Option<usize>,
}

impl ToolCall for WebSearchCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        WebSearchTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        self.tool.argument_order(key)
    }

    fn timeout_secs(&self) -> u64 {
        self.tool.timeout_secs()
    }

    fn set_runtime_output_token_limit(&mut self, max_output_tokens: Option<usize>) {
        self.runtime_output_token_limit = max_output_tokens;
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let WebSearchCall {
                    raw_arguments,
                    tool,
                    runtime_output_token_limit,
                    ..
                } = *self;
                let args = serde_json::from_value::<WebSearchInputArgs>(raw_arguments).map_err(
                    |error| {
                        ToolError::failed(
                            WebSearchTool::NAME,
                            format!("invalid arguments: {error}"),
                        )
                    },
                )?;
                let limit = args
                    .limit
                    .unwrap_or(tool.default_result_limit)
                    .min(WEB_SEARCH_MAX_RESULT_LIMIT);
                wait_before_request(&tool.last_request_started).await;
                let hits = tool
                    .backend
                    .search(&args.terms, limit)
                    .await
                    .map_err(|error| ToolError::failed(WebSearchTool::NAME, error.to_string()))?;
                let mut result = WebSearchResult::new(hits);
                if let Some(max_output_tokens) = runtime_output_token_limit {
                    result.fit_to_token_budget(max_output_tokens);
                }
                Ok(Box::new(result) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::agent::tools::backends::web_search::{
        WebSearchError, WEB_SEARCH_DEFAULT_RESULT_LIMIT,
    };
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule, SandboxRule, SandboxRuleConfig};

    /// Stub backend that records the `limit` it was asked for and replays a
    /// canned response, so tests never touch the network.
    #[derive(Debug)]
    struct StubBackend {
        hits: Vec<WebSearchHit>,
        error: Option<String>,
        requested_limit: Mutex<Option<usize>>,
        requested_terms: Mutex<Option<String>>,
    }

    impl StubBackend {
        fn returning(hits: Vec<WebSearchHit>) -> Self {
            StubBackend {
                hits,
                error: None,
                requested_limit: Mutex::new(None),
                requested_terms: Mutex::new(None),
            }
        }

        fn failing(message: &str) -> Self {
            StubBackend {
                hits: Vec::new(),
                error: Some(message.to_string()),
                requested_limit: Mutex::new(None),
                requested_terms: Mutex::new(None),
            }
        }

        fn requested_limit(&self) -> Option<usize> {
            *self.requested_limit.lock().expect("stub lock")
        }

        fn requested_terms(&self) -> Option<String> {
            self.requested_terms.lock().expect("stub lock").clone()
        }
    }

    impl WebSearchBackend for StubBackend {
        fn network_target(&self) -> String {
            "https://stub.example".to_string()
        }

        fn search<'a>(
            &'a self,
            terms: &'a str,
            limit: usize,
        ) -> futures::future::BoxFuture<'a, Result<Vec<WebSearchHit>, WebSearchError>> {
            *self.requested_limit.lock().expect("stub lock") = Some(limit);
            *self.requested_terms.lock().expect("stub lock") = Some(terms.to_string());
            let hits = self.hits.clone();
            let error = self.error.clone();
            Box::pin(async move {
                match error {
                    Some(message) => Err(WebSearchError::Request(message)),
                    None => Ok(hits),
                }
            })
        }
    }

    fn hit(title: &str, url: &str, snippet: &str) -> WebSearchHit {
        WebSearchHit {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
        }
    }

    /// A sandbox context with no `[tools.web_search]` config: the sandbox
    /// abstains, so approval falls back to the tool's own default.
    fn default_sandbox_context() -> SandboxContext {
        SandboxContext::new(Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        )))
    }

    /// The context `default_registry` builds when `--auto-approve-web-search`
    /// is given: `sandbox_context_for` appends this catch-all rule last.
    fn auto_approve_catch_all_context() -> SandboxContext {
        default_sandbox_context().with_rule(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: glob::Pattern::new("**").expect("`**` is a valid glob"),
        })
    }

    fn tool_with(backend: Arc<StubBackend>) -> (WebSearchTool, Arc<StubBackend>) {
        let tool = WebSearchTool::new(
            backend.clone(),
            &WebSearchConfig::default(),
            default_sandbox_context(),
        );
        (tool, backend)
    }

    /// Execute one call to completion (the async core shared by the sync
    /// wrappers below).
    async fn execute(
        tool: &WebSearchTool,
        arguments: Value,
    ) -> Result<Box<dyn ToolCallResult>, ToolError> {
        tool.create_call(uuid::Uuid::new_v4(), arguments)
            .run()
            .await
    }

    /// Run one call to completion on a current-thread Tokio runtime while holding
    /// the tracing subscriber lock (the established unit-test pattern).
    fn run_call(
        tool: &WebSearchTool,
        arguments: Value,
    ) -> Result<Box<dyn ToolCallResult>, ToolError> {
        let mut outcome = None;
        crate::test_support::run_locked(async {
            outcome = Some(execute(tool, arguments).await);
        });
        outcome.expect("the async body must run")
    }

    #[test]
    fn input_schema_fields_have_descriptions() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        let schema = tool.input_schema();
        crate::test_support::assert_all_properties_have_descriptions(&schema, "WebSearchInputArgs");
    }

    #[test]
    fn limit_schema_maximum_matches_the_documented_cap() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        let value = serde_json::to_value(tool.input_schema()).expect("schema serializes");
        assert_eq!(
            value["properties"]["limit"]["maximum"].as_f64(),
            Some(f64::from(WEB_SEARCH_MAX_RESULT_LIMIT as u32)),
            "the schema must advertise the max"
        );
        let description = value["properties"]["limit"]["description"]
            .as_str()
            .expect("limit description");
        assert!(
            description.contains(&WEB_SEARCH_MAX_RESULT_LIMIT.to_string()),
            "limit description must advertise WEB_SEARCH_MAX_RESULT_LIMIT: {description}"
        );
        assert_eq!(
            WEB_SEARCH_MAX_RESULT_LIMIT, 20,
            "the constant's documented default must not drift"
        );
    }

    #[test]
    fn text_lists_numbered_titles_and_urls() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(vec![
            hit("First", "https://one.example", "one"),
            hit("Second", "https://two.example", "two"),
        ])));
        let result = run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");
        assert_eq!(
            result.text(),
            "1. First\n   https://one.example\n2. Second\n   https://two.example"
        );
    }

    #[test]
    fn metadata_carries_title_url_and_truncated_snippet() {
        let long_snippet = "x".repeat(WEB_SEARCH_SNIPPET_MAX_CHARS + 50);
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(vec![hit(
            "Title",
            "https://example.com",
            &long_snippet,
        )])));
        let result = run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");

        let metadata = result.metadata().expect("metadata is present");
        assert!(metadata.is_array(), "metadata is an array of results");
        let entries = metadata.as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["title"], "Title");
        assert_eq!(entries[0]["url"], "https://example.com");
        let snippet = entries[0]["snippet"].as_str().expect("snippet string");
        assert_eq!(
            snippet.chars().count(),
            WEB_SEARCH_SNIPPET_MAX_CHARS,
            "snippets are truncated to the documented cap"
        );

        let schema = result.metadata_schema().expect("metadata schema");
        let schema_value = serde_json::to_value(schema).expect("schema serializes");
        assert_eq!(
            schema_value["type"], "array",
            "the schema describes the result array"
        );
    }

    #[test]
    fn content_sections_render_the_result_list() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(vec![hit(
            "Title",
            "https://example.com",
            "snippet",
        )])));
        let result = run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        match &sections[0] {
            Section::Content(content) => {
                assert_eq!(content.content, result.text());
                assert!(content.file_extension.is_none());
            }
            other => panic!("expected a content section, got {other:?}"),
        }
    }

    #[test]
    fn empty_results_have_no_content_sections() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        let result = run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");
        assert_eq!(result.text(), "");
        assert!(result.content_sections().is_empty());
        assert_eq!(
            result
                .metadata()
                .and_then(|value| value.as_array().map(Vec::len)),
            Some(0),
            "an empty result still reports an empty metadata array"
        );
    }

    #[test]
    fn default_limit_is_used_when_omitted() {
        let (tool, backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");
        assert_eq!(
            backend.requested_limit(),
            Some(WEB_SEARCH_DEFAULT_RESULT_LIMIT)
        );
        assert_eq!(backend.requested_terms().as_deref(), Some("query"));
    }

    #[test]
    fn configured_result_limit_replaces_the_builtin_default() {
        let config = WebSearchConfig {
            result_limit: 4,
            ..WebSearchConfig::default()
        };
        let backend = Arc::new(StubBackend::returning(Vec::new()));
        let tool = WebSearchTool::new(backend.clone(), &config, default_sandbox_context());
        run_call(&tool, serde_json::json!({ "terms": "query" })).expect("ok");
        assert_eq!(backend.requested_limit(), Some(4));
    }

    #[test]
    fn explicit_limit_overrides_the_default_and_is_capped() {
        let (tool, backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        run_call(&tool, serde_json::json!({ "terms": "query", "limit": 3 })).expect("ok");
        assert_eq!(backend.requested_limit(), Some(3));

        // Schema validation rejects values above the max before execution;
        // `run` still caps defensively for callers that bypass validation.
        run_call(&tool, serde_json::json!({ "terms": "query", "limit": 999 })).expect("ok");
        assert_eq!(backend.requested_limit(), Some(WEB_SEARCH_MAX_RESULT_LIMIT));
    }

    #[test]
    fn backend_failure_surfaces_as_a_tool_error_without_results() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::failing("HTTP 403")));
        let error =
            run_call(&tool, serde_json::json!({ "terms": "query" })).expect_err("fails fast");
        assert!(
            error
                .to_string()
                .contains("web search request failed: HTTP 403"),
            "{error}"
        );
    }

    #[test]
    fn invalid_arguments_are_rejected_before_any_search() {
        let (tool, backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        let error =
            run_call(&tool, serde_json::json!({ "limit": 3 })).expect_err("`terms` is required");
        assert!(error.to_string().contains("invalid arguments"), "{error}");
        assert!(
            backend.requested_limit().is_none(),
            "the backend must not be called for invalid arguments"
        );
    }

    #[test]
    fn approval_requires_the_user_until_a_rule_or_flag_states_a_verdict() {
        let config = WebSearchConfig::default();
        let arguments = serde_json::json!({ "terms": "query" });

        // No [tools.web_search] config, no --auto-approve-web-search: the
        // sandbox abstains and the tool's own default applies.
        let default_tool = WebSearchTool::new(
            Arc::new(StubBackend::returning(Vec::new())),
            &config,
            default_sandbox_context(),
        );
        assert!(matches!(
            default_tool.needs_approval(&arguments),
            ApprovalCheck::UserApprovalRequired
        ));

        // A configured `network_request` rule is the per-profile approval switch.
        let configured_context =
            default_sandbox_context().with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            }]);
        let configured_tool = WebSearchTool::new(
            Arc::new(StubBackend::returning(Vec::new())),
            &config,
            configured_context,
        );
        assert!(matches!(
            configured_tool.needs_approval(&arguments),
            ApprovalCheck::AutoApproved
        ));

        // `--auto-approve-web-search` appends the catch-all, which wins over
        // configured rules (same precedence as the other tools' flags).
        let flagged_tool = WebSearchTool::new(
            Arc::new(StubBackend::returning(Vec::new())),
            &config,
            auto_approve_catch_all_context(),
        );
        assert!(matches!(
            flagged_tool.needs_approval(&arguments),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn outer_timeout_exceeds_the_internal_http_timeout() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(
            Arc::new(StubBackend::returning(Vec::new())),
            &config,
            default_sandbox_context(),
        );
        assert_eq!(
            tool.timeout_secs(),
            config.http_timeout().as_secs() + WEB_SEARCH_OUTER_TIMEOUT_GRACE_SECS,
            "outer = internal + grace"
        );
        assert_eq!(tool.timeout_secs(), 40, "documented default outer timeout");
    }

    #[test]
    fn description_is_backend_agnostic_and_title_names_the_backend() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(
            Arc::new(StubBackend::returning(Vec::new())),
            &config,
            default_sandbox_context(),
        );

        // The LLM interface is identical whichever backend serves the
        // request — the backend belongs in the user-facing title only.
        assert_eq!(tool.description(), WEB_SEARCH_DESCRIPTION.as_str());
        assert!(tool
            .description()
            .contains(&WEB_SEARCH_MAX_RESULT_LIMIT.to_string()));
        assert!(
            !tool
                .description()
                .to_ascii_lowercase()
                .contains("duckduckgo"),
            "the model-facing description must not name the backend: {}",
            tool.description()
        );

        assert_eq!(
            tool.title(),
            "Web Search (DuckDuckGo)",
            "the title hints at how results are obtained"
        );
    }

    #[test]
    fn title_arguments_carry_terms_then_limit() {
        let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
        let call = tool.create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({ "terms": "query", "limit": 2 }),
        );
        assert_eq!(
            call.title_arguments(),
            vec![
                ("terms".to_string(), serde_json::json!("query")),
                ("limit".to_string(), serde_json::json!(2)),
            ],
            "arguments follow the schema's declaration order"
        );
    }

    // ── throttle ──────────────────────────────────────────────────────────────

    #[test]
    fn throttle_delay_is_zero_without_a_previous_request() {
        assert_eq!(
            throttle_delay(
                None,
                WEB_SEARCH_THROTTLE_INTERVAL,
                Duration::from_millis(500)
            ),
            Duration::ZERO,
            "the first request is never delayed"
        );
    }

    #[test]
    fn throttle_delay_is_zero_once_the_interval_has_elapsed() {
        for elapsed in [WEB_SEARCH_THROTTLE_INTERVAL, Duration::from_secs(6)] {
            assert_eq!(
                throttle_delay(
                    Some(elapsed),
                    WEB_SEARCH_THROTTLE_INTERVAL,
                    Duration::from_millis(500)
                ),
                Duration::ZERO,
                "a call spaced at least one interval apart is not delayed"
            );
        }
    }

    #[test]
    fn throttle_delay_covers_the_interval_remainder_plus_jitter() {
        assert_eq!(
            throttle_delay(
                Some(Duration::from_secs(1)),
                WEB_SEARCH_THROTTLE_INTERVAL,
                Duration::ZERO
            ),
            Duration::from_secs(4),
            "only the remainder of the interval is awaited"
        );
        assert_eq!(
            throttle_delay(
                Some(Duration::from_secs(1)),
                WEB_SEARCH_THROTTLE_INTERVAL,
                Duration::from_millis(500)
            ),
            Duration::from_millis(4500),
            "jitter is added on top of the remainder"
        );
    }

    #[test]
    fn throttle_jitter_band_is_0_to_10_percent_of_the_interval() {
        let max_jitter = Duration::from_secs_f64(
            WEB_SEARCH_THROTTLE_INTERVAL.as_secs_f64() * WEB_SEARCH_THROTTLE_JITTER_FRACTION,
        );
        assert_eq!(max_jitter, Duration::from_millis(500));
        assert_eq!(
            throttle_delay(
                Some(Duration::ZERO),
                WEB_SEARCH_THROTTLE_INTERVAL,
                max_jitter
            ),
            WEB_SEARCH_THROTTLE_INTERVAL + max_jitter,
            "the slowest possible wait is interval + 10%"
        );
        for _ in 0..100 {
            let jitter = throttle_jitter(WEB_SEARCH_THROTTLE_INTERVAL);
            assert!(
                jitter < max_jitter,
                "sampled jitter must stay inside the band: {jitter:?}"
            );
        }
    }

    #[test]
    fn first_request_is_not_delayed() {
        crate::test_support::run_locked_paused(async {
            let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
            let started = Instant::now();
            execute(&tool, serde_json::json!({ "terms": "one" }))
                .await
                .expect("first search ok");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "the first request must start immediately: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn consecutive_requests_wait_out_the_interval_plus_jitter() {
        crate::test_support::run_locked_paused(async {
            let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
            execute(&tool, serde_json::json!({ "terms": "one" }))
                .await
                .expect("first search ok");

            let started = Instant::now();
            execute(&tool, serde_json::json!({ "terms": "two" }))
                .await
                .expect("second search ok");
            let elapsed = started.elapsed();

            assert!(
                elapsed >= WEB_SEARCH_THROTTLE_INTERVAL,
                "a back-to-back request waits out the interval: {elapsed:?}"
            );
            assert_throttle_wait_within_timer_precision(elapsed);
        });
    }

    #[test]
    fn requests_already_spaced_apart_are_not_delayed() {
        crate::test_support::run_locked_paused(async {
            let (tool, _backend) = tool_with(Arc::new(StubBackend::returning(Vec::new())));
            execute(&tool, serde_json::json!({ "terms": "one" }))
                .await
                .expect("first search ok");
            tokio::time::advance(WEB_SEARCH_THROTTLE_INTERVAL + Duration::from_secs(1)).await;

            let started = Instant::now();
            execute(&tool, serde_json::json!({ "terms": "two" }))
                .await
                .expect("second search ok");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "an already-spaced request must not be delayed: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_failed_request_still_throttles_the_next_one() {
        crate::test_support::run_locked_paused(async {
            let (tool, _backend) = tool_with(Arc::new(StubBackend::failing("HTTP 403")));
            execute(&tool, serde_json::json!({ "terms": "one" }))
                .await
                .expect_err("the first search fails fast");

            let started = Instant::now();
            execute(&tool, serde_json::json!({ "terms": "two" }))
                .await
                .expect_err("the second search fails fast");
            let elapsed = started.elapsed();

            assert!(
                elapsed >= WEB_SEARCH_THROTTLE_INTERVAL,
                "the failure timestamp must still throttle the next start: {elapsed:?}"
            );
            assert_throttle_wait_within_timer_precision(elapsed);
        });
    }

    fn assert_throttle_wait_within_timer_precision(elapsed: Duration) {
        let maximum_jitter = Duration::from_secs_f64(
            WEB_SEARCH_THROTTLE_INTERVAL.as_secs_f64() * WEB_SEARCH_THROTTLE_JITTER_FRACTION,
        );
        // Tokio rounds sleep deadlines to its millisecond timer granularity,
        // including with a paused clock. Pure jitter tests enforce the band.
        let timer_precision = Duration::from_millis(1);
        assert!(
            elapsed <= WEB_SEARCH_THROTTLE_INTERVAL + maximum_jitter + timer_precision,
            "throttled wait exceeds jitter plus timer precision: {elapsed:?}"
        );
    }
}
