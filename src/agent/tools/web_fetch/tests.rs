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

use futures::future::BoxFuture;

use super::super::backends::web_fetch::{WebFetchError, WebFetchResponse};
use super::super::{Section, ToolAttachment, ToolCallResult, ToolError, STANDARD_BASE64};
use super::*;
use crate::agent::tools::backends::web_fetch::{
    WebFetchBody, WEB_FETCH_MAX_OUTPUT_BYTES as OUTPUT_CAP,
};
use crate::sandbox::{NamedSandboxRule, SandboxRule, SandboxRuleConfig};
use base64::Engine as _;

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
        _policy: crate::agent::tools::backends::web_fetch::HopPolicy<'a>,
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
        detected_content_type: None,
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
        detected_content_type: None,
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
fn run_call(tool: &WebFetchTool, arguments: Value) -> Result<Box<dyn ToolCallResult>, ToolError> {
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
    crate::test_support::assert_all_properties_have_descriptions(&schema, "web_fetch");
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
            assert_eq!(
                section.label.as_deref(),
                Some("Output"),
                "complete results label their output Result"
            );
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
            assert_eq!(section.label.as_deref(), Some("Partial output"));
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
    let result = run_call(&tool, arguments("https://example.com/docs/paper.pdf")).expect("fetch");
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
        default_sandbox_context()
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            }])
            .expect("valid sandbox fixture"),
    );
    assert_eq!(tool.needs_approval(&url), ApprovalCheck::AutoApproved);

    // A matching `target` pattern applies to the requested URL's scheme://host.
    let tool = tool_with(
        backend(),
        default_sandbox_context()
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: Some("https://docs.rs".to_string()),
            }])
            .expect("valid sandbox fixture"),
    );
    assert_eq!(tool.needs_approval(&url), ApprovalCheck::AutoApproved);

    // A non-matching `target` abstains.
    let tool = tool_with(
        backend(),
        default_sandbox_context()
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: Some("https://example.com".to_string()),
            }])
            .expect("valid sandbox fixture"),
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

#[test]
fn response_section_shows_detected_content_type_when_header_is_absent() {
    let mut response = text_response("Suite: focal", false);
    response.content_type = None;
    response.detected_content_type = Some("text/plain".to_string());
    let tool = tool_with(StubBackend::returning(response), default_sandbox_context());
    let result = run_call(&tool, arguments("https://example.com/page")).expect("fetch succeeds");
    match &result.content_sections()[0] {
        Section::Content(section) => assert!(
            section
                .content
                .contains("detected_content_type=text/plain (autodetected)"),
            "{}",
            section.content
        ),
        other => panic!("expected a content section, got {other:?}"),
    }
    assert_eq!(
        result.metadata().expect("metadata")["detected_content_type"],
        "text/plain"
    );
}
