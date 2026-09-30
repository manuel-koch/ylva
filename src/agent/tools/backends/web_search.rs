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

//! Web-search backend seam for the `web_search` tool.
//!
//! [`WebSearchBackend`] is the injectable seam the `web_search` tool depends
//! on: the tool receives an `Arc<dyn WebSearchBackend>` from its constructor,
//! so tests pass a stub and no unit/integration test touches the network
//! (see `docs/known-test-issues.md` — network side effects must be
//! caller-injected). Each backend kind lives in its own implementation
//! module ([`duckduckgo`] today); [`backend_for`] selects one by config name.
//!
//! Every backend fails fast: a transport error, a non-success HTTP status
//! (including bot-detection/captcha responses), or an unparseable body is a
//! [`WebSearchError`] — never a retry and never a silently-empty result set.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Implementation modules, one per [`WebSearchBackend`] backend kind.
pub mod duckduckgo;

pub use duckduckgo::DuckDuckGoBackend;

/// Result count used when a `web_search` call omits `limit`.
pub const WEB_SEARCH_DEFAULT_RESULT_LIMIT: usize = 15;

/// Schema-enforced maximum number of results a single `web_search` call may
/// request (`limit`).
pub const WEB_SEARCH_MAX_RESULT_LIMIT: usize = 20;

/// Maximum number of characters of a result snippet carried in the tool's
/// structured metadata.
pub const WEB_SEARCH_SNIPPET_MAX_CHARS: usize = 300;

// ── Backend selection ─────────────────────────────────────────────────────────

/// A search backend selectable through `[web_search] backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchBackendKind {
    /// DuckDuckGo's HTML endpoint (`html.duckduckgo.com`).
    DuckDuckGo,
}

impl WebSearchBackendKind {
    /// Every backend name accepted as `[web_search] backend`, in display
    /// order. The error text for an unknown value is built from this list, so
    /// config validation cannot drift from what is actually implemented.
    pub const SUPPORTED_NAMES: &'static [&'static str] = &["duckduckgo"];

    /// The wire/config name of this backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
        }
    }

    /// Human-readable backend name for user-facing labels (e.g. the
    /// `Web Search (…)` tool title).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
        }
    }

    /// Look up a backend by its config name; `None` for an unsupported name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::SUPPORTED_NAMES
            .iter()
            .find(|supported| **supported == name)
            .map(|_| match name {
                "duckduckgo" => Self::DuckDuckGo,
                _ => unreachable!("SUPPORTED_NAMES only lists known backends"),
            })
    }

    /// The human-readable list of supported names for error messages.
    pub fn supported_names_display() -> String {
        Self::SUPPORTED_NAMES.join(", ")
    }
}

// ── Seam ──────────────────────────────────────────────────────────────────────

/// One ranked web-search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebSearchHit {
    /// Title of the result page.
    pub title: String,
    /// Absolute URL of the result page.
    pub url: String,
    /// Excerpt from the result page; whitespace-normalized, may be empty when
    /// the backend provides none.
    pub snippet: String,
}

/// A web-search failure. Mapped by the `web_search` tool to a
/// [`ToolError`](crate::agent::tools::ToolError) with this message; never
/// retried.
#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    /// The HTTP request could not be sent or its body could not be read.
    #[error("web search request failed: {0}")]
    Request(String),
    /// The backend answered with a non-success HTTP status (e.g. `403`
    /// captcha pages, `429` rate limits, `5xx` errors).
    #[error("web search backend responded with HTTP {status}")]
    Status { status: u16 },
    /// The response body was not the expected search-results markup — e.g.
    /// DuckDuckGo's bot-detection challenge page, which answers with HTTP
    /// `202`.
    #[error("web search response could not be parsed: {0}")]
    Parse(String),
}

/// A search backend the `web_search` tool can call.
///
/// Boxed-future return keeps the trait `dyn`-compatible (the same pattern as
/// [`crate::llm::Provider::chat`]).
pub trait WebSearchBackend: Send + Sync + std::fmt::Debug {
    /// Search for `terms` and return at most `limit` ranked results.
    fn search<'a>(
        &'a self,
        terms: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<WebSearchHit>, WebSearchError>>;

    /// The request's `scheme://host` network-sandbox match key — the
    /// backend endpoint's authority (e.g. `https://html.duckduckgo.com`) —
    /// matched against `network_request` rule `target` patterns.
    fn network_target(&self) -> String;
}

/// Build the backend named by `backend_name`.
///
/// `[web_search] backend` is validated at config load, so an unknown name is
/// a defensive-only path: registration must never fail over it. Returns an
/// `Arc` so the caller can hand the instance straight to the tool.
pub fn backend_for(backend_name: &str, http_timeout: Duration) -> Arc<dyn WebSearchBackend> {
    match WebSearchBackendKind::from_name(backend_name) {
        Some(WebSearchBackendKind::DuckDuckGo) => Arc::new(DuckDuckGoBackend::new(http_timeout)),
        None => {
            tracing::error!(
                backend = backend_name,
                supported = WebSearchBackendKind::SUPPORTED_NAMES.join(", "),
                "unsupported web search backend; falling back to duckduckgo"
            );
            Arc::new(DuckDuckGoBackend::new(http_timeout))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct StubBackend {
        hits: Vec<WebSearchHit>,
        error: Option<String>,
    }

    impl WebSearchBackend for StubBackend {
        fn network_target(&self) -> String {
            "https://stub.example".to_string()
        }

        fn search<'a>(
            &'a self,
            _terms: &'a str,
            _limit: usize,
        ) -> BoxFuture<'a, Result<Vec<WebSearchHit>, WebSearchError>> {
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

    #[test]
    fn stub_backend_returns_hits_unchanged_through_the_seam() {
        let expected = vec![hit("Title", "https://example.com", "snippet")];
        let backend: Arc<dyn WebSearchBackend> = Arc::new(StubBackend {
            hits: expected.clone(),
            error: None,
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let actual = runtime.block_on(backend.search("anything", 5)).expect("ok");
        assert_eq!(actual, expected, "the seam must not alter backend hits");
    }

    #[test]
    fn stub_backend_error_surfaces_through_the_seam() {
        let backend: Arc<dyn WebSearchBackend> = Arc::new(StubBackend {
            hits: Vec::new(),
            error: Some("boom".to_string()),
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(backend.search("anything", 5))
            .expect_err("the stub error must pass through");
        assert_eq!(error.to_string(), "web search request failed: boom");
    }

    #[test]
    fn backend_kind_names_roundtrip() {
        assert_eq!(
            WebSearchBackendKind::from_name("duckduckgo"),
            Some(WebSearchBackendKind::DuckDuckGo)
        );
        assert_eq!(WebSearchBackendKind::from_name("searxng"), None);
        assert_eq!(WebSearchBackendKind::DuckDuckGo.as_str(), "duckduckgo");
        assert_eq!(
            WebSearchBackendKind::DuckDuckGo.display_name(),
            "DuckDuckGo"
        );
        assert_eq!(
            WebSearchBackendKind::supported_names_display(),
            "duckduckgo"
        );
    }

    #[test]
    fn backend_for_builds_the_configured_backend() {
        let backend = backend_for("duckduckgo", Duration::from_secs(1));
        assert!(format!("{backend:?}").contains("DuckDuckGoBackend"));
        // Defensive fallback for an unsupported name (config load rejects it).
        let fallback = backend_for("something-else", Duration::from_secs(1));
        assert!(format!("{fallback:?}").contains("DuckDuckGoBackend"));
    }
}
