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

//! Web-fetch backend seam and content handling for the `web_fetch` tool.
//!
//! [`WebFetchBackend`] is the injectable seam the `web_fetch` tool depends
//! on: the tool receives an `Arc<dyn WebFetchBackend>` from its constructor,
//! so tests pass a stub and no unit/integration test touches the network
//! (see `docs/known-test-issues.md` — network side effects must be
//! caller-injected). Each backend kind lives in its own implementation
//! module ([`native`] today); [`backend_for`] selects one by config name.
//!
//! Content handling mirrors `read`'s autodetect philosophy
//! ([`convert_body_to_result`]): magic-number-recognized media
//! (PNG/JPEG/WebP/GIF/PDF) is returned as typed Base64 attachments with
//! `read`'s whole-file size ceiling ([`crate::agent::tools::read::READ_MEDIA_MAX_BYTES`]),
//! `text/html` is converted to markdown via `htmd` (script/style payloads
//! dropped, see [`convert_html_to_markdown`]), and text/json/xml bodies
//! pass through verbatim. Other binary or non-UTF-8 bodies are a clear
//! [`WebFetchError`] — never a retry and never garbage text.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::agent::tools::read::supported_media_type;

/// Implementation modules, one per [`WebFetchBackend`] backend kind.
pub mod native;

pub use native::{HttpFetchBackend, MAX_REDIRECTS};

/// Maximum number of bytes of model-facing text (HTML-derived markdown or
/// verbatim text) a `web_fetch` result carries. Longer output is truncated on
/// a UTF-8 character boundary and flagged `truncated` (metadata +
/// `Partial output` content-section label, no in-text marker) — the same
/// vocabulary capped `read`/`bash`/`search` output uses.
pub const WEB_FETCH_MAX_OUTPUT_BYTES: usize = 65536;

/// Seconds the `web_fetch` tool adds to the configured request timeout to
/// form its outer execution timeout (mirrors
/// `web_search`'s grace): the outer bound must exceed the inner HTTP timeout
/// so a slow server surfaces as a request timeout with a proper error, not
/// as an outer-tool timeout.
pub const WEB_FETCH_OUTER_TIMEOUT_GRACE_SECS: u64 = 10;

// ── Backend selection ─────────────────────────────────────────────────────────

/// A fetch backend selectable through `[web_fetch] backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchBackendKind {
    /// Plain local HTTP `GET` via `reqwest` (no browser, no external service).
    Native,
}

impl WebFetchBackendKind {
    /// Config/wire name of the native backend — the single place this name is
    /// spelled out.
    pub const NATIVE_NAME: &'static str = "native";

    /// User-facing label of the native backend.
    pub const NATIVE_DISPLAY_NAME: &'static str = "Native";

    /// Every backend name accepted as `[web_fetch] backend`, in display
    /// order. The error text for an unknown value is built from this list, so
    /// config validation cannot drift from what is actually implemented.
    pub const SUPPORTED_NAMES: &'static [&'static str] = &[Self::NATIVE_NAME];

    /// The wire/config name of this backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => Self::NATIVE_NAME,
        }
    }

    /// Human-readable backend name for user-facing labels (e.g. the
    /// `Web Fetch (…)` tool title).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Native => Self::NATIVE_DISPLAY_NAME,
        }
    }

    /// Look up a backend by its config name; `None` for an unsupported name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            Self::NATIVE_NAME => Some(Self::Native),
            _ => None,
        }
    }

    /// The human-readable list of supported names for error messages.
    pub fn supported_names_display() -> String {
        Self::SUPPORTED_NAMES.join(", ")
    }
}

// ── Seam ──────────────────────────────────────────────────────────────────────

/// The fetched response body, already classified for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebFetchBody {
    /// Model-facing text: HTML-derived markdown or verbatim
    /// text/json/xml. May be cut at [`WEB_FETCH_MAX_OUTPUT_BYTES`].
    Text { text: String },
    /// Complete recognized media (PNG/JPEG/WebP/GIF/PDF) as raw bytes, to be
    /// returned as a typed Base64 attachment like `read` does for local
    /// files. `media_type` is the magic-number-sniffed type, never the
    /// Content-Type header (headers can lie).
    Media { bytes: Vec<u8>, media_type: String },
}

/// Outcome of a successful fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFetchResponse {
    /// URL the request ended up at after redirects were followed.
    pub final_url: String,
    /// HTTP status of the final response (always a success status; the
    /// backend maps non-success to [`WebFetchError::Status`]).
    pub status: u16,
    /// The response's Content-Type header value as sent (may carry
    /// parameters like `charset`), when present.
    pub content_type: Option<String>,
    /// Text type autodetected from the body (`text/html` or `text/plain`)
    /// when the server sent no Content-Type; `None` otherwise.
    pub detected_content_type: Option<String>,
    /// The classified body.
    pub body: WebFetchBody,
    /// `true` when [`WebFetchBody::Text`] was cut (raw-read limit or output
    /// cap). Only ever `true` for text bodies — media is never truncated.
    pub truncated: bool,
}

/// A web-fetch failure. Mapped by the `web_fetch` tool to a
/// [`ToolError`](crate::agent::tools::ToolError) with this message; never
/// retried.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WebFetchError {
    /// The URL is not a parsable absolute `http`/`https` URL. Rejected
    /// before any request is made.
    #[error("web fetch requires an absolute http:// or https:// URL, got '{url}'")]
    InvalidUrl { url: String },
    /// The HTTP request could not be sent or its body could not be read.
    #[error("web fetch request failed: {detail}")]
    Request { detail: String },
    /// The server answered with a non-success HTTP status (e.g. `403`
    /// blocks, `404` missing pages, `429` rate limits, `5xx` errors).
    #[error("web fetch of '{url}' responded with HTTP {status}")]
    Status { url: String, status: u16 },
    /// A complete media body larger than
    /// [`READ_MEDIA_MAX_BYTES`](crate::agent::tools::read::READ_MEDIA_MAX_BYTES).
    /// Truncation is not an option: a partial image or PDF is unusable, so
    /// the fetch fails with the size and the limit (mirrors `read`'s
    /// whole-file media error).
    #[error(
        "web fetch response is {size} bytes, over the {limit}-byte limit for complete \
         responses (a partial file cannot be attached)"
    )]
    TooLarge { size: usize, limit: usize },
    /// The body is neither recognized supported media nor UTF-8 model text
    /// (unsupported binary content type, or a text content type whose body is
    /// not valid UTF-8).
    #[error("web fetch response content is not supported: {detail}")]
    UnsupportedContentType { detail: String },
    /// A redirect hop or request target was refused by the hop policy: too
    /// many redirects, a non-`http(s)` scheme, a private or reserved address,
    /// or an explicit `deny` rule.
    #[error("web fetch of '{url}' was refused: {reason}")]
    Refused { url: String, reason: String },
    /// The body is HTML but `htmd` could not convert it to markdown.
    #[error("web fetch response could not be converted to markdown: {detail}")]
    Conversion { detail: String },
}

/// Sandbox decisions the backend consults while following a fetch.
///
/// Both callbacks receive a hop's `scheme://host[:port]` match key. Neither
/// ever prompts.
#[derive(Clone, Copy)]
pub struct HopPolicy<'a> {
    /// Explicit `deny` rules for a redirect hop to a different target:
    /// returns the denial reason, or `None` when the hop may proceed.
    pub deny: &'a (dyn Fn(&str) -> Option<String> + Send + Sync),
    /// Whether a connection to a private or reserved address is permitted for
    /// the target (the `network_private_address` rule).
    pub allow_private_address: &'a (dyn Fn(&str) -> bool + Send + Sync),
}

impl HopPolicy<'static> {
    /// No denials and private addresses allowed; for tests and callers
    /// without a sandbox.
    pub const fn permissive() -> Self {
        HopPolicy {
            deny: &|_| None,
            allow_private_address: &|_| true,
        }
    }
}

/// A fetch backend the `web_fetch` tool can call.
///
/// Boxed-future return keeps the trait `dyn`-compatible (the same pattern as
/// [`crate::web_search::WebSearchBackend`]).
pub trait WebFetchBackend: Send + Sync + std::fmt::Debug {
    /// Issue an HTTP `GET` for `url` and return the classified response.
    ///
    /// The requested URL is approved by the caller. Redirects are followed by
    /// the backend under `policy`: at most [`MAX_REDIRECTS`] hops,
    /// `http`/`https` only, no private or reserved addresses unless
    /// `policy.allow_private_address` permits the hop's target, and
    /// `policy.deny` is consulted for every hop to a different target.
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        policy: HopPolicy<'a>,
    ) -> BoxFuture<'a, Result<WebFetchResponse, WebFetchError>>;
}

// ── URL validation and approval matching ──────────────────────────────────────

/// Validate `url` as fetchable and return its network-sandbox match key.
///
/// Only absolute `http`/`https` URLs are fetchable; anything else is
/// [`WebFetchError::InvalidUrl`] before any request is made. The key is
/// `scheme://host` (explicit port included when present in the URL) — the
/// string `network_request` rule `target` patterns are matched against; its
/// derivation lives in [`crate::http::network_target`], shared with the
/// `web_search` backends.
pub fn network_target(url: &str) -> Result<String, WebFetchError> {
    crate::http::network_target(url).ok_or_else(|| WebFetchError::InvalidUrl {
        url: url.to_string(),
    })
}

// ── Body classification ───────────────────────────────────────────────────────

/// The model-facing text kinds a response body can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextBodyKind {
    /// `text/html` — converted to markdown.
    Html,
    /// `text/*`, JSON or XML — passed through verbatim.
    Plain,
}

/// Convert an HTML document to markdown for model-facing text.
///
/// The web-fetch reader behaves like a no-JS browser: `<script>`/`<style>`
/// payloads (including `<script type="application/json">` config blobs) are
/// not page content and are dropped entirely, and `<noscript>` fallback
/// markup is parsed as normal DOM — kept as visible content — instead of
/// leaking into the output as raw text.
fn convert_html_to_markdown(text: &str) -> Result<String, WebFetchError> {
    htmd::HtmlToMarkdown::builder()
        .scripting_enabled(false)
        .skip_tags(vec!["script", "style"])
        .build()
        .convert(text)
        .map_err(|error| WebFetchError::Conversion {
            detail: error.to_string(),
        })
}

/// Classify a response body for the model (pure; no I/O).
///
/// Precedence mirrors `read`'s autodetect: magic-number-recognized supported
/// media (PNG/JPEG/WebP/GIF/PDF) becomes a [`WebFetchBody::Media`] attachment
/// payload regardless of the Content-Type header — headers can lie. Only
/// those five types are promoted; `infer` also sniffs text kinds (e.g.
/// `text/html`), which fall through to the content-type-driven text path.
///
/// Text handling: `text/html` is converted to markdown via
/// [`convert_html_to_markdown`]; `text/*`, `application/json`,
/// `application/xml`, and `+json`/`+xml` suffixed types pass through verbatim
/// as UTF-8. A missing content type is autodetected from the body (see
/// [`sniff_text_body_kind`]). Anything else — other content types or
/// binary/non-UTF-8 bodies — is [`WebFetchError::UnsupportedContentType`] naming what
/// stood in the way.
pub fn convert_body_to_result(
    content_type: Option<&str>,
    raw_body: &[u8],
) -> Result<WebFetchBody, WebFetchError> {
    if let Some(media_type) = supported_media_type(raw_body) {
        return Ok(WebFetchBody::Media {
            bytes: raw_body.to_vec(),
            media_type,
        });
    }
    let mime_type = normalized_mime_type(content_type);
    let body_kind = match mime_type.as_deref() {
        None => sniff_text_body_kind(raw_body),
        known => text_body_kind(known),
    };
    match body_kind {
        Some(TextBodyKind::Html) => {
            let text = utf8_text_or_unsupported(mime_type.as_deref(), raw_body)?;
            let markdown = convert_html_to_markdown(text)?;
            Ok(WebFetchBody::Text { text: markdown })
        }
        Some(TextBodyKind::Plain) => {
            let text = utf8_text_or_unsupported(mime_type.as_deref(), raw_body)?;
            Ok(WebFetchBody::Text {
                text: text.to_string(),
            })
        }
        None => Err(WebFetchError::UnsupportedContentType {
            detail: unsupported_content_detail(mime_type.as_deref()),
        }),
    }
}

/// Cut `text` at [`WEB_FETCH_MAX_OUTPUT_BYTES`] bytes on a UTF-8 character
/// boundary and report whether anything was cut. Media is never truncated —
/// a partial image or PDF is unusable — so this only applies to text bodies.
pub fn truncate_text_output(text: &str) -> (String, bool) {
    if text.len() <= WEB_FETCH_MAX_OUTPUT_BYTES {
        return (text.to_string(), false);
    }
    let mut end = WEB_FETCH_MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// The Content-Type header's mime type: the part before any `;` parameter,
/// lowercased and trimmed. `None` when the header is missing or empty.
fn normalized_mime_type(content_type: Option<&str>) -> Option<String> {
    let mime_type = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    (!mime_type.is_empty()).then_some(mime_type)
}

/// The text type autodetected for a text body served without a Content-Type
/// header; `None` when a header was sent, the body is media, or it is not text.
pub fn detect_missing_content_type(content_type: Option<&str>, raw_body: &[u8]) -> Option<String> {
    if normalized_mime_type(content_type).is_some() || supported_media_type(raw_body).is_some() {
        return None;
    }
    let detected = match sniff_text_body_kind(raw_body)? {
        TextBodyKind::Html => "text/html",
        TextBodyKind::Plain => "text/plain",
    };
    Some(detected.to_string())
}

/// Autodetect the text kind of a body served without a content type: valid
/// UTF-8 without NUL bytes is text, and an HTML doctype or `<html` root tag
/// makes it HTML. Anything else stays unsupported.
fn sniff_text_body_kind(raw_body: &[u8]) -> Option<TextBodyKind> {
    if raw_body.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(raw_body).ok()?;
    let head: String = text
        .trim_start()
        .chars()
        .take(256)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.starts_with("<!doctype html") || head.starts_with("<html") {
        Some(TextBodyKind::Html)
    } else {
        Some(TextBodyKind::Plain)
    }
}

/// The text kind for a normalized mime type, or `None` for content that is
/// not model-returnable text (recognized media is handled before this via
/// magic numbers).
fn text_body_kind(mime_type: Option<&str>) -> Option<TextBodyKind> {
    let mime_type = mime_type?;
    if mime_type == "text/html" {
        return Some(TextBodyKind::Html);
    }
    let plain = mime_type.starts_with("text/")
        || mime_type == "application/json"
        || mime_type == "application/xml"
        || mime_type.ends_with("+json")
        || mime_type.ends_with("+xml");
    plain.then_some(TextBodyKind::Plain)
}

/// Decode `raw_body` as UTF-8 text or fail with a clear
/// [`WebFetchError::UnsupportedContentType`] naming the content type.
fn utf8_text_or_unsupported<'a>(
    mime_type: Option<&str>,
    raw_body: &'a [u8],
) -> Result<&'a str, WebFetchError> {
    std::str::from_utf8(raw_body).map_err(|_| WebFetchError::UnsupportedContentType {
        detail: match mime_type {
            Some(mime_type) => {
                format!("body of content type '{mime_type}' is not valid UTF-8 text")
            }
            None => "body is not valid UTF-8 text".to_string(),
        },
    })
}

/// The error detail for a body that is not recognized supported media and not
/// a text kind: names the content type and what is accepted.
fn unsupported_content_detail(mime_type: Option<&str>) -> String {
    let accepted = "expected HTML, text, JSON, XML, or recognized media (PNG, JPEG, WebP, \
                    GIF, PDF)";
    match mime_type {
        Some(mime_type) => format!("content type '{mime_type}' is not supported; {accepted}"),
        None => format!("response has no content type; {accepted}"),
    }
}

/// Build the backend named by `backend_name`.
///
/// `[web_fetch] backend` is validated at config load, so an unknown name is
/// a defensive-only path: registration must never fail over it. Returns an
/// `Arc` so the caller can hand the instance straight to the tool.
pub fn backend_for(backend_name: &str, http_timeout: Duration) -> Arc<dyn WebFetchBackend> {
    match WebFetchBackendKind::from_name(backend_name) {
        Some(WebFetchBackendKind::Native) => Arc::new(HttpFetchBackend::new(http_timeout)),
        None => {
            tracing::error!(
                backend = backend_name,
                supported = WebFetchBackendKind::SUPPORTED_NAMES.join(", "),
                "unsupported web fetch backend; falling back to native"
            );
            Arc::new(HttpFetchBackend::new(http_timeout))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        bytes
    }

    fn pdf_bytes() -> Vec<u8> {
        b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec()
    }

    #[test]
    fn network_target_accepts_http_and_https_and_reports_scheme_and_host() {
        assert_eq!(
            network_target("https://docs.rs/htmd/0.5.5/htmd/").expect("https URL"),
            "https://docs.rs"
        );
        assert_eq!(
            network_target("http://example.com").expect("http URL"),
            "http://example.com"
        );
        assert_eq!(
            network_target("https://example.com:8443/path?q=1").expect("explicit port"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn network_target_rejects_other_schemes_and_unparsable_urls() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/file",
            "javascript:alert(1)",
            "example.com",
            "",
        ] {
            match network_target(url) {
                Err(WebFetchError::InvalidUrl { url: reported }) => assert_eq!(reported, url),
                other => panic!("expected InvalidUrl for {url}, got {other:?}"),
            }
        }
    }

    #[test]
    fn media_sniffing_wins_over_the_content_type_header() {
        let body = convert_body_to_result(Some("text/html"), &png_bytes()).expect("sniffed media");
        assert_eq!(
            body,
            WebFetchBody::Media {
                bytes: png_bytes(),
                media_type: "image/png".to_string(),
            }
        );
    }

    #[test]
    fn recognized_media_is_returned_verbatim_as_media() {
        for (raw, media_type) in [
            (png_bytes(), "image/png"),
            (pdf_bytes(), "application/pdf"),
            (b"GIF89a\x01\x00\x01\x00".to_vec(), "image/gif"),
            (b"\xff\xd8\xff\xe0\x00\x10JFIF".to_vec(), "image/jpeg"),
            (b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec(), "image/webp"),
        ] {
            let body = convert_body_to_result(None, &raw).expect("recognized media");
            assert_eq!(
                body,
                WebFetchBody::Media {
                    bytes: raw,
                    media_type: media_type.to_string(),
                },
                "magic numbers decide for {media_type}"
            );
        }
    }

    #[test]
    fn html_is_converted_to_markdown() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let body = convert_body_to_result(Some("text/html; charset=utf-8"), html.as_bytes())
            .expect("html converts");
        let WebFetchBody::Text { text } = body else {
            panic!("expected a text body");
        };
        assert!(text.contains("Title"), "keeps the heading text: {text}");
        assert!(text.contains("world"), "keeps the paragraph text: {text}");
        assert!(!text.contains("<h1>"), "strips markup: {text}");
    }

    #[test]
    fn html_script_and_style_payloads_are_dropped() {
        let html = concat!(
            "<html><body>",
            "<h1>Title</h1>",
            "<script>innerMap[\"mediaCategory\"] = \"Plenarsitzungen\";</script>",
            "<script type=\"application/json\">{ \"cmId\": 493054 }</script>",
            "<style>.bt-title { color: red; }</style>",
            "<p>Visible prose</p>",
            "</body></html>",
        );
        let body = convert_body_to_result(Some("text/html; charset=utf-8"), html.as_bytes())
            .expect("html converts");
        let WebFetchBody::Text { text } = body else {
            panic!("expected a text body");
        };
        assert!(text.contains("Title"), "keeps the heading text: {text}");
        assert!(text.contains("Visible prose"), "keeps the prose: {text}");
        assert!(
            !text.contains("mediaCategory"),
            "drops inline JavaScript: {text}"
        );
        assert!(!text.contains("cmId"), "drops JSON script payloads: {text}");
        assert!(!text.contains("color"), "drops CSS: {text}");
    }

    #[test]
    fn html_template_content_is_dropped() {
        let html = "<html><body><p>Visible</p><template><p>InertDraft</p></template></body></html>";
        let body = convert_body_to_result(Some("text/html; charset=utf-8"), html.as_bytes())
            .expect("html converts");
        let WebFetchBody::Text { text } = body else {
            panic!("expected a text body");
        };
        assert!(text.contains("Visible"), "keeps the prose: {text}");
        assert!(
            !text.contains("InertDraft"),
            "drops template content: {text}"
        );
    }

    #[test]
    fn html_noscript_fallback_is_kept_as_visible_content() {
        let html = "<html><body><noscript><p>StaticFallback</p></noscript></body></html>";
        let body = convert_body_to_result(Some("text/html; charset=utf-8"), html.as_bytes())
            .expect("html converts");
        let WebFetchBody::Text { text } = body else {
            panic!("expected a text body");
        };
        assert_eq!(
            text.trim(),
            "StaticFallback",
            "noscript fallback is markdown text, not raw markup: {text}"
        );
    }

    #[test]
    fn text_json_and_xml_pass_through_verbatim() {
        for (content_type, raw) in [
            ("text/plain; charset=utf-8", b"plain text".as_slice()),
            ("text/markdown", b"# heading".as_slice()),
            ("application/json", br#"{"key": "value"}"#.as_slice()),
            ("application/xml", b"<root/>".as_slice()),
            ("application/ld+json", br#"[]"#.as_slice()),
            ("application/atom+xml", b"<feed/>".as_slice()),
        ] {
            let body = convert_body_to_result(Some(content_type), raw).expect("text passes");
            assert_eq!(
                body,
                WebFetchBody::Text {
                    text: String::from_utf8(raw.to_vec()).expect("fixture is UTF-8")
                },
                "{content_type} passes through verbatim"
            );
        }
    }

    #[test]
    fn unrecognized_binary_and_non_utf8_bodies_fail_with_a_clear_error() {
        let cases: [(Option<&str>, &[u8]); 4] = [
            (Some("application/zip"), b"PK\x03\x04\x14\x00\x00\x00"),
            (Some("application/octet-stream"), b"\x00\x01\x02\x03"),
            (Some("text/plain"), &[0xff, 0xfe, 0xfd]),
            (None, &[0x00, 0x01, 0x02, 0x03]),
        ];
        for (content_type, raw) in cases {
            match convert_body_to_result(content_type, raw) {
                Err(WebFetchError::UnsupportedContentType { detail }) => {
                    assert!(!detail.is_empty(), "detail explains the failure");
                }
                other => {
                    panic!("expected UnsupportedContentType for {content_type:?}, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn missing_content_type_is_autodetected_from_the_body() {
        let plain = convert_body_to_result(None, b"Origin: Launchpad\nSuite: focal\n")
            .expect("plain text is detected");
        assert!(matches!(plain, WebFetchBody::Text { ref text } if text.contains("Suite: focal")));
        let html = convert_body_to_result(
            None,
            b"<!DOCTYPE html><html><body><h1>Hi</h1></body></html>",
        )
        .expect("html is detected");
        assert!(matches!(html, WebFetchBody::Text { ref text } if text.contains("# Hi")));
    }

    #[test]
    fn detected_content_type_is_reported_only_for_headerless_text() {
        let detect = detect_missing_content_type;
        assert_eq!(detect(None, b"Suite: focal"), Some("text/plain".into()));
        assert_eq!(detect(None, b"<html></html>"), Some("text/html".into()));
        assert_eq!(detect(Some("text/plain"), b"Suite: focal"), None);
        assert_eq!(detect(None, &png_bytes()), None);
        assert_eq!(detect(None, &[0x00, 0x01]), None);
    }

    #[test]
    fn unsupported_content_detail_names_the_content_type_and_the_accepted_kinds() {
        let detail = unsupported_content_detail(Some("application/zip"));
        assert!(detail.contains("application/zip"), "{detail}");
        assert!(detail.contains("PNG"), "{detail}");
        let missing = unsupported_content_detail(None);
        assert!(missing.contains("no content type"), "{missing}");
    }

    #[test]
    fn truncation_cuts_at_the_output_cap_on_a_char_boundary() {
        assert_eq!(truncate_text_output("short"), ("short".to_string(), false));
        let long = "é".repeat(WEB_FETCH_MAX_OUTPUT_BYTES); // 2 bytes per char
        let (cut, truncated) = truncate_text_output(&long);
        assert!(truncated);
        assert!(cut.len() <= WEB_FETCH_MAX_OUTPUT_BYTES);
        assert!(
            cut.chars().all(|character| character == 'é'),
            "no half chars"
        );
    }

    #[test]
    fn truncation_keeps_exactly_the_prefix_at_the_cap() {
        let exactly = "a".repeat(WEB_FETCH_MAX_OUTPUT_BYTES);
        assert_eq!(truncate_text_output(&exactly), (exactly.clone(), false));
        let over = format!("{exactly}b");
        assert_eq!(truncate_text_output(&over), (exactly, true));
    }

    #[test]
    fn backend_kind_names_roundtrip() {
        assert_eq!(WebFetchBackendKind::SUPPORTED_NAMES, &["native"]);
        assert_eq!(WebFetchBackendKind::Native.as_str(), "native");
        assert_eq!(WebFetchBackendKind::Native.display_name(), "Native");
        assert_eq!(
            WebFetchBackendKind::from_name("native"),
            Some(WebFetchBackendKind::Native)
        );
        assert_eq!(WebFetchBackendKind::from_name("camofox"), None);
        assert_eq!(
            WebFetchBackendKind::supported_names_display(),
            "native".to_string()
        );
    }

    #[test]
    fn every_supported_name_is_selectable_and_roundtrips() {
        for supported in WebFetchBackendKind::SUPPORTED_NAMES {
            let kind = WebFetchBackendKind::from_name(supported)
                .unwrap_or_else(|| panic!("SUPPORTED_NAMES entry '{supported}' must be usable"));
            assert_eq!(kind.as_str(), *supported);
            assert!(
                !kind.display_name().is_empty(),
                "'{supported}' must have a user-facing label"
            );
        }
    }

    #[test]
    fn backend_for_builds_the_configured_backend() {
        let backend = backend_for("native", Duration::from_secs(1));
        assert!(format!("{backend:?}").contains("HttpFetchBackend"));
        // Defensive fallback for an unsupported name (config load rejects it).
        let fallback = backend_for("something-else", Duration::from_secs(1));
        assert!(format!("{fallback:?}").contains("HttpFetchBackend"));
    }
}
