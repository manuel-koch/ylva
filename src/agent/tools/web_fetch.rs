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
use std::time::Duration;

use base64::Engine as _;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::backends::web_fetch::{
    network_target, WebFetchBackend, WebFetchBackendKind, WebFetchBody, WebFetchError,
    WebFetchResponse, WEB_FETCH_MAX_OUTPUT_BYTES, WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS,
};
use super::{
    read::{
        safe_source_filename, READ_MEDIA_MAX_BYTES, SUPPORTED_IMAGE_MEDIA_TYPES,
        SUPPORTED_MEDIA_TYPES,
    },
    title_case_tool_name, utf8_text_preview, ContentSection, Section, Tool, ToolAttachment,
    ToolCall, ToolCallResult, ToolError, STANDARD_BASE64,
};
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

// ── Result ────────────────────────────────────────────────────────────────────

/// Structured metadata of a `web_fetch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebFetchResultMetadata {
    /// The requested URL, before any redirect was followed.
    pub url: String,
    /// The URL the request ended up at, after redirects were followed.
    pub final_url: String,
    /// HTTP status of the final response.
    pub status: u16,
    /// The response's Content-Type header value as sent (may carry
    /// parameters like `charset`), `null` when the server sent none.
    pub content_type: Option<String>,
    /// Magic-number-sniffed media type of the returned Base64 attachment
    /// (PNG/JPEG/WebP/GIF/PDF); absent for text bodies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// `true` when the returned text was cut (raw-read limit or output cap);
    /// never present for media — a partial image or PDF is unusable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// Successful output from a `web_fetch` invocation.
#[derive(Debug)]
pub struct WebFetchResult {
    /// The requested URL, before any redirect was followed.
    url: String,
    /// The URL the request ended up at, after redirects were followed.
    final_url: String,
    /// HTTP status of the final response.
    status: u16,
    /// The response's Content-Type header value as sent, when present.
    content_type: Option<String>,
    /// Model-facing content: converted/verbatim text, or standard padded
    /// Base64 of a complete media body.
    text: String,
    /// UTF-8 text projection for user-facing previews when `text` carries
    /// Base64 media bytes.
    text_preview: Option<String>,
    /// Recognized media type when the body is complete supported media.
    media_type: Option<String>,
    /// `true` when `text` was cut; reported as `truncated: true` metadata
    /// and the `Partial result` content-section label.
    truncated: bool,
}

impl WebFetchResult {
    /// Project a backend response into the model-facing result: text bodies
    /// pass through (already capped by the backend), media becomes Base64
    /// plus attachment metadata (complete bodies only — the backend rejects
    /// oversize media with [`WebFetchError::TooLarge`] instead).
    fn new(url: String, response: WebFetchResponse) -> Self {
        let WebFetchResponse {
            final_url,
            status,
            content_type,
            body,
            truncated,
        } = response;
        match body {
            WebFetchBody::Text { text } => WebFetchResult {
                url,
                final_url,
                status,
                content_type,
                text,
                text_preview: None,
                media_type: None,
                truncated,
            },
            WebFetchBody::Media { bytes, media_type } => WebFetchResult {
                url,
                final_url,
                status,
                content_type,
                text: STANDARD_BASE64.encode(&bytes),
                text_preview: utf8_text_preview(&bytes),
                media_type: Some(media_type),
                truncated: false,
            },
        }
    }

    /// Response facts as `key=value` lines for the `Response` content-section.
    ///
    /// Equal duplicates are collapsed: `final_url` only appears when a
    /// redirect moved the request off `url`, and the sniffed `media_type`
    /// only appears when it disagrees with the sent `content_type` (or none
    /// was sent) — the model-facing `metadata()` keeps every field
    /// regardless.
    fn response_facts(&self) -> String {
        let mut facts = vec![format!("url={}", self.url)];
        if self.final_url != self.url {
            facts.push(format!("final_url={}", self.final_url));
        }
        facts.push(format!("status={}", self.status));
        if let Some(content_type) = &self.content_type {
            facts.push(format!("content_type={content_type}"));
        }
        if let Some(media_type) = &self.media_type {
            let sent_type_matches = self.content_type.as_deref().is_some_and(|sent| {
                sent.split(';')
                    .next()
                    .unwrap_or(sent)
                    .trim()
                    .eq_ignore_ascii_case(media_type)
            });
            if !sent_type_matches {
                facts.push(format!("media_type={media_type}"));
            }
        }
        facts.join("\n")
    }
}

impl ToolCallResult for WebFetchResult {
    fn text(&self) -> String {
        self.text.clone()
    }

    fn display_text(&self) -> Option<String> {
        // Base64 is transport for the attachment, never user-facing text
        // (same as `read`'s media results).
        match self.media_type {
            Some(_) => None,
            None => Some(self.text()),
        }
    }

    fn metadata(&self) -> Option<Value> {
        serde_json::to_value(WebFetchResultMetadata {
            url: self.url.clone(),
            final_url: self.final_url.clone(),
            status: self.status,
            content_type: self.content_type.clone(),
            media_type: self.media_type.clone(),
            truncated: self.truncated.then_some(true),
        })
        .ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        let mut schema = serde_json::to_value(schema_for!(WebFetchResultMetadata)).ok()?;
        let properties = schema.get_mut("properties")?.as_object_mut()?;
        if self.media_type.is_none() {
            properties.remove("media_type");
        }
        if !self.truncated {
            properties.remove("truncated");
        }
        serde_json::from_value(schema).ok()
    }

    fn attachments(&self) -> Vec<ToolAttachment> {
        // Mirrors `read`'s media attachments: images are typed images, PDFs
        // are typed files carrying a filename.
        let Some(media_type) = self.media_type.clone() else {
            return vec![];
        };
        if SUPPORTED_IMAGE_MEDIA_TYPES.contains(&media_type.as_str()) {
            vec![ToolAttachment::Image { media_type }]
        } else if SUPPORTED_MEDIA_TYPES.contains(&media_type.as_str()) {
            vec![ToolAttachment::File {
                media_type,
                filename: safe_source_filename(&self.url, "attachment.pdf"),
            }]
        } else {
            vec![]
        }
    }

    /// Response facts (`url`, `final_url`, `status`, `content_type`,
    /// `media_type`) render as the wrapped `Response` content-section in the
    /// tool bubble — the one-line title summary truncates them into
    /// unreadability and duplicates the call's `url` argument. Only the short
    /// `truncated` flag is worth that line (collapsed bubbles show no body).
    fn display_summary(&self) -> Vec<(String, String)> {
        if self.truncated {
            vec![("truncated".to_string(), "true".to_string())]
        } else {
            vec![]
        }
    }

    fn content_sections(&self) -> Vec<Section> {
        let mut sections = vec![Section::Content(ContentSection {
            label: Some("Response".to_string()),
            content: self.response_facts(),
            file_extension: None,
            line_number_offset: None,
        })];
        let preview = match self.media_type {
            Some(_) => self.text_preview.as_deref(),
            None => Some(self.text.as_str()),
        };
        if let Some(preview) = preview.filter(|content| !content.is_empty()) {
            // Truncated content is an incomplete display of the result — same
            // vocabulary as bash's and read's partial-output sections.
            let label = self.truncated.then(|| "Partial result".to_string());
            sections.push(Section::Content(ContentSection {
                label,
                content: preview.to_string(),
                file_extension: None,
                line_number_offset: None,
            }));
        }
        sections
    }
}

/// Map a [`WebFetchError`] to the tool's [`ToolError`]. `TooLarge` gets
/// `read`'s explicit-size whole-file wording (naming the URL, the size, and
/// the limit); every other variant carries a self-describing message.
fn web_fetch_tool_error(url: &str, error: WebFetchError) -> ToolError {
    let message = match &error {
        WebFetchError::TooLarge { size, limit } => format!(
            "failed to fetch '{url}': complete response is {size} bytes, over the \
             {limit}-byte limit for whole-file fetches (a partial file cannot be attached)"
        ),
        WebFetchError::Request { detail } => format!("failed to fetch '{url}': {detail}"),
        WebFetchError::UnsupportedContentType { detail } => {
            format!("failed to fetch '{url}': {detail}")
        }
        WebFetchError::Conversion { detail } => {
            format!("failed to fetch '{url}': could not convert HTML to markdown: {detail}")
        }
        WebFetchError::InvalidUrl { .. } | WebFetchError::Status { .. } => error.to_string(),
    };
    ToolError::failed(WebFetchTool::NAME, message)
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `web_fetch` tool call.
///
/// Carries a clone of the creating tool so schema-derived argument ranking
/// and the tool's settings stay in sync without re-deriving them here.
pub struct WebFetchCall {
    id: uuid::Uuid,
    raw_arguments: Value,
    tool: WebFetchTool,
}

impl ToolCall for WebFetchCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        WebFetchTool::NAME
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
                let WebFetchCall {
                    raw_arguments,
                    tool,
                    ..
                } = *self;
                let args = serde_json::from_value::<WebFetchInputArgs>(raw_arguments).map_err(
                    |error| {
                        ToolError::failed(WebFetchTool::NAME, format!("invalid arguments: {error}"))
                    },
                )?;
                // Scheme allowlist before any request (also the approval
                // match-key source in `needs_approval`).
                network_target(&args.url)
                    .map_err(|error| web_fetch_tool_error(&args.url, error))?;
                let outer_timeout = Duration::from_secs(tool.timeout_secs);
                let response = tokio::time::timeout(outer_timeout, tool.backend.fetch(&args.url))
                    .await
                    .map_err(|_| {
                        ToolError::failed(
                            WebFetchTool::NAME,
                            format!(
                                "web fetch of '{}' timed out after {} seconds",
                                args.url, tool.timeout_secs
                            ),
                        )
                    })?
                    .map_err(|error| web_fetch_tool_error(&args.url, error))?;
                Ok(Box::new(WebFetchResult::new(args.url, response)) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
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
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures::future::BoxFuture;

    use super::*;
    use crate::agent::tools::backends::web_fetch::{
        WebFetchBody, WEB_FETCH_MAX_OUTPUT_BYTES as OUTPUT_CAP,
    };
    use crate::sandbox::{NamedSandboxRule, SandboxRule, SandboxRuleConfig};

    #[derive(Debug)]
    struct StubBackend {
        outcome: Mutex<Option<Result<WebFetchResponse, WebFetchError>>>,
    }

    impl StubBackend {
        fn returning(response: WebFetchResponse) -> Self {
            StubBackend {
                outcome: Mutex::new(Some(Ok(response))),
            }
        }

        fn failing(error: WebFetchError) -> Self {
            StubBackend {
                outcome: Mutex::new(Some(Err(error))),
            }
        }
    }

    impl WebFetchBackend for StubBackend {
        fn fetch<'a>(
            &'a self,
            _url: &'a str,
        ) -> BoxFuture<'a, Result<WebFetchResponse, WebFetchError>> {
            let outcome = self
                .outcome
                .lock()
                .expect("stub lock")
                .take()
                .expect("stub outcome consumed once");
            Box::pin(async move { outcome })
        }
    }

    // `Mutex` without the `std::sync` prefix clashes with the tool-module
    // imports; alias it locally.
    use std::sync::Mutex;

    fn text_response(text: &str, truncated: bool) -> WebFetchResponse {
        WebFetchResponse {
            final_url: "https://docs.rs/page".to_string(),
            status: 200,
            content_type: Some("text/html".to_string()),
            body: WebFetchBody::Text {
                text: text.to_string(),
            },
            truncated,
        }
    }

    fn png_bytes() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        bytes
    }

    fn media_response() -> WebFetchResponse {
        WebFetchResponse {
            final_url: "https://example.com/image.png".to_string(),
            status: 200,
            content_type: Some("image/png".to_string()),
            body: WebFetchBody::Media {
                bytes: png_bytes(),
                media_type: "image/png".to_string(),
            },
            truncated: false,
        }
    }

    fn default_sandbox_context() -> SandboxContext {
        SandboxContext::new(Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        )))
    }

    fn auto_approve_catch_all_context() -> SandboxContext {
        default_sandbox_context().with_rule(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: glob::Pattern::new("**").expect("** is a valid glob"),
        })
    }

    fn tool_with(backend: StubBackend, sandbox_context: SandboxContext) -> WebFetchTool {
        WebFetchTool::new(
            Arc::new(backend),
            &WebFetchConfig::default(),
            sandbox_context,
        )
    }

    fn arguments(url: &str) -> Value {
        serde_json::json!({ "url": url })
    }

    async fn execute(
        tool: &WebFetchTool,
        arguments: Value,
    ) -> Result<Box<dyn ToolCallResult>, ToolError> {
        let call = tool.create_call(uuid::Uuid::new_v4(), arguments);
        call.run().await
    }

    /// Run one call under the shared tracing-subscriber lock (see
    /// `docs/known-test-issues.md`).
    fn run_call(
        tool: &WebFetchTool,
        arguments: Value,
    ) -> Result<Box<dyn ToolCallResult>, ToolError> {
        let mut outcome = None;
        crate::test_support::run_locked(async {
            outcome = Some(execute(tool, arguments).await);
        });
        outcome.expect("the async body must run")
    }

    #[test]
    fn every_schema_property_has_a_description() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        let schema = tool.input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "web_fetch");
    }

    #[test]
    fn title_hints_at_the_configured_backend() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        assert_eq!(tool.title(), "Web Fetch (Native)");
        assert_eq!(tool.name(), "web_fetch");
        assert!(tool.timeout_secs() >= 30, "outer timeout bounds the call");
    }

    #[test]
    fn text_success_carries_text_and_metadata() {
        let tool = tool_with(
            StubBackend::returning(text_response("# Title", false)),
            default_sandbox_context(),
        );
        let result = run_call(&tool, arguments("https://docs.rs/page")).expect("fetch succeeds");
        assert_eq!(result.text(), "# Title");
        assert_eq!(result.display_text().as_deref(), Some("# Title"));
        assert_eq!(
            result.metadata(),
            Some(serde_json::json!({
                "url": "https://docs.rs/page",
                "final_url": "https://docs.rs/page",
                "status": 200,
                "content_type": "text/html",
            }))
        );
        let sections = result.content_sections();
        assert_eq!(sections.len(), 2);
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Response"));
                assert_eq!(
                    section.content, "url=https://docs.rs/page\nstatus=200\ncontent_type=text/html",
                    "equal url/final_url collapses to one url line"
                );
            }
            other => panic!("expected a content section, got {other:?}"),
        }
        match &sections[1] {
            Section::Content(section) => {
                assert_eq!(section.label, None, "complete results carry no label");
                assert_eq!(section.content, "# Title");
            }
            other => panic!("expected a content section, got {other:?}"),
        }
        assert!(
            result.display_summary().is_empty(),
            "response facts live in the Response section, not the one-line summary"
        );
    }

    #[test]
    fn truncated_text_carries_metadata_and_the_partial_result_section() {
        let tool = tool_with(
            StubBackend::returning(text_response("partial", true)),
            default_sandbox_context(),
        );
        let result = run_call(&tool, arguments("https://docs.rs/page")).expect("fetch succeeds");
        let metadata = result.metadata().expect("metadata");
        assert_eq!(metadata["truncated"], serde_json::json!(true));
        let schema = serde_json::to_value(result.metadata_schema().expect("schema")).expect("json");
        assert!(
            schema["properties"].get("truncated").is_some(),
            "the schema keeps the truncated field when it is present"
        );
        assert_eq!(
            result.display_summary(),
            vec![("truncated".to_string(), "true".to_string())],
            "the short truncation flag stays on the one-line summary"
        );
        let sections = result.content_sections();
        assert_eq!(sections.len(), 2);
        match &sections[1] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Partial result"));
            }
            other => panic!("expected a content section, got {other:?}"),
        }
    }

    #[test]
    fn media_carries_base64_text_an_attachment_and_media_type_metadata() {
        let tool = tool_with(
            StubBackend::returning(media_response()),
            default_sandbox_context(),
        );
        let result = run_call(&tool, arguments("https://example.com/image.png")).expect("fetch");
        assert_eq!(result.text(), STANDARD_BASE64.encode(png_bytes()));
        assert_eq!(
            result.display_text(),
            None,
            "Base64 is not user-facing text"
        );
        let metadata = result.metadata().expect("metadata");
        assert_eq!(metadata["media_type"], serde_json::json!("image/png"));
        assert_eq!(metadata["truncated"], serde_json::Value::Null);
        assert_eq!(
            result.attachments(),
            vec![ToolAttachment::Image {
                media_type: "image/png".to_string()
            }]
        );
        let sections = result.content_sections();
        assert_eq!(
            sections.len(),
            1,
            "media without a text preview carries only the Response section"
        );
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Response"));
                assert_eq!(
                    section.content,
                    "url=https://example.com/image.png\nstatus=200\ncontent_type=image/png",
                    "media_type equal to content_type collapses to one type line"
                );
            }
            other => panic!("expected a content section, got {other:?}"),
        }
        assert!(result.display_summary().is_empty());
    }

    #[test]
    fn response_section_shows_final_url_only_when_redirected() {
        let mut response = text_response("x", false);
        response.final_url = "https://docs.rs/redirected".to_string();
        let tool = tool_with(StubBackend::returning(response), default_sandbox_context());
        let result = run_call(&tool, arguments("https://docs.rs/page")).expect("fetch succeeds");
        let sections = result.content_sections();
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(
                    section.content,
                    "url=https://docs.rs/page\n\
                     final_url=https://docs.rs/redirected\n\
                     status=200\n\
                     content_type=text/html",
                    "a followed redirect keeps both URLs visible"
                );
            }
            other => panic!("expected a content section, got {other:?}"),
        }
    }

    #[test]
    fn response_section_keeps_both_type_lines_when_they_differ() {
        let mut response = media_response();
        response.content_type = Some("application/octet-stream".to_string());
        let tool = tool_with(StubBackend::returning(response), default_sandbox_context());
        let result =
            run_call(&tool, arguments("https://example.com/image.png")).expect("fetch succeeds");
        let sections = result.content_sections();
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(
                    section.content,
                    "url=https://example.com/image.png\n\
                     status=200\n\
                     content_type=application/octet-stream\n\
                     media_type=image/png",
                    "a lying content type stays visible next to the sniffed media type"
                );
            }
            other => panic!("expected a content section, got {other:?}"),
        }
    }

    #[test]
    fn response_section_shows_media_type_when_content_type_is_absent() {
        let mut response = media_response();
        response.content_type = None;
        let tool = tool_with(StubBackend::returning(response), default_sandbox_context());
        let result =
            run_call(&tool, arguments("https://example.com/image.png")).expect("fetch succeeds");
        let sections = result.content_sections();
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(
                    section.content,
                    "url=https://example.com/image.png\nstatus=200\nmedia_type=image/png",
                    "the sniffed media type carries the type fact alone"
                );
            }
            other => panic!("expected a content section, got {other:?}"),
        }
    }

    #[test]
    fn pdf_media_carries_a_file_attachment_named_from_the_url() {
        let mut response = media_response();
        response.body = WebFetchBody::Media {
            bytes: b"%PDF-1.7 test".to_vec(),
            media_type: "application/pdf".to_string(),
        };
        response.final_url = "https://example.com/docs/paper.pdf".to_string();
        let tool = tool_with(StubBackend::returning(response), default_sandbox_context());
        let result =
            run_call(&tool, arguments("https://example.com/docs/paper.pdf")).expect("fetch");
        assert_eq!(
            result.attachments(),
            vec![ToolAttachment::File {
                media_type: "application/pdf".to_string(),
                filename: "paper.pdf".to_string()
            }]
        );
    }

    #[test]
    fn approval_requires_the_user_until_a_rule_or_flag_states_a_verdict() {
        let url = arguments("https://docs.rs/page");
        let backend = || StubBackend::returning(text_response("x", false));

        // No rules at all → the tool's own default.
        let tool = tool_with(backend(), default_sandbox_context());
        assert_eq!(
            tool.needs_approval(&url),
            ApprovalCheck::UserApprovalRequired
        );

        // A target-less network_request rule matches every call.
        let tool = tool_with(
            backend(),
            default_sandbox_context().with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            }]),
        );
        assert_eq!(tool.needs_approval(&url), ApprovalCheck::AutoApproved);

        // A matching `target` pattern applies to the requested URL's scheme://host.
        let tool = tool_with(
            backend(),
            default_sandbox_context().with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: Some("https://docs.rs".to_string()),
            }]),
        );
        assert_eq!(tool.needs_approval(&url), ApprovalCheck::AutoApproved);

        // A non-matching `target` abstains.
        let tool = tool_with(
            backend(),
            default_sandbox_context().with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: Some("https://example.com".to_string()),
            }]),
        );
        assert_eq!(
            tool.needs_approval(&url),
            ApprovalCheck::UserApprovalRequired
        );

        // The --auto-approve-web-fetch catch-all always wins.
        let tool = tool_with(backend(), auto_approve_catch_all_context());
        assert_eq!(tool.needs_approval(&url), ApprovalCheck::AutoApproved);
    }

    #[test]
    fn missing_or_empty_url_is_auto_denied() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({})),
            ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "url": "" })),
            ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "url": "ftp://example.com" })),
            ApprovalCheck::AutoDenied { reason } if reason.contains("http")
        ));
    }

    #[test]
    fn every_web_fetch_error_maps_to_a_tool_error() {
        let url = "https://example.com/blob";
        let cases: Vec<(WebFetchError, &str)> = vec![
            (
                WebFetchError::InvalidUrl {
                    url: "file:///etc/passwd".to_string(),
                },
                "http",
            ),
            (
                WebFetchError::Request {
                    detail: "connection refused".to_string(),
                },
                "connection refused",
            ),
            (
                WebFetchError::Status {
                    url: url.to_string(),
                    status: 404,
                },
                "HTTP 404",
            ),
            (
                WebFetchError::UnsupportedContentType {
                    detail: "content type 'application/zip' is not supported".to_string(),
                },
                "application/zip",
            ),
            (
                WebFetchError::Conversion {
                    detail: "bad markup".to_string(),
                },
                "bad markup",
            ),
        ];
        for (error, expected) in cases {
            let tool = tool_with(StubBackend::failing(error), default_sandbox_context());
            let error = run_call(&tool, arguments(url)).expect_err("the fetch must fail");
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn too_large_maps_to_read_style_explicit_size_wording() {
        let tool = tool_with(
            StubBackend::failing(WebFetchError::TooLarge {
                size: 20_000_000,
                limit: READ_MEDIA_MAX_BYTES,
            }),
            default_sandbox_context(),
        );
        let error = run_call(&tool, arguments("https://example.com/huge.png"))
            .expect_err("oversize media must fail");
        let message = error.to_string();
        assert!(
            message.contains("https://example.com/huge.png"),
            "{message}"
        );
        assert!(message.contains("20000000 bytes"), "{message}");
        assert!(
            message.contains(&format!("over the {}-byte limit", READ_MEDIA_MAX_BYTES)),
            "{message}"
        );
        assert!(
            message.contains("a partial file cannot be attached"),
            "{message}"
        );
    }

    #[test]
    fn missing_url_argument_fails_the_call() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        let error = run_call(&tool, serde_json::json!({})).expect_err("missing url");
        assert!(error.to_string().contains("invalid arguments"), "{error}");
    }

    #[test]
    fn invalid_url_fails_the_call_before_any_backend_work() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        let error = run_call(&tool, arguments("file:///etc/passwd")).expect_err("non-http scheme");
        assert!(error.to_string().contains("http"), "{error}");
    }

    #[test]
    fn metadata_schema_prunes_absent_fields() {
        let tool = tool_with(
            StubBackend::returning(text_response("x", false)),
            default_sandbox_context(),
        );
        let result = run_call(&tool, arguments("https://docs.rs/page")).expect("fetch");
        let schema = serde_json::to_value(result.metadata_schema().expect("schema")).expect("json");
        assert!(schema["properties"].get("media_type").is_none());
        assert!(schema["properties"].get("truncated").is_none());
        assert!(schema["properties"].get("final_url").is_some());
    }

    #[test]
    fn output_cap_constant_matches_the_documented_value() {
        assert_eq!(OUTPUT_CAP, 64 * 1024);
    }
}
