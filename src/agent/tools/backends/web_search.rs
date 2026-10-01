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
//! module ([`duckduckgo`], [`searxng`]); [`backend_for`] builds the one named
//! by the config, or returns an error — it never substitutes a different
//! backend for the configured one.
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
pub mod searxng;

pub use duckduckgo::DuckDuckGoBackend;
pub use searxng::SearXngBackend;

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
    /// A SearXNG instance (`[web_search.backends.searxng] base_url`).
    SearXng,
}

impl WebSearchBackendKind {
    /// Config/wire name of the DuckDuckGo backend — the single place this
    /// name is spelled out.
    pub const DUCKDUCKGO_NAME: &'static str = "duckduckgo";
    /// Config/wire name of the SearXNG backend.
    pub const SEARXNG_NAME: &'static str = "searxng";

    /// User-facing label of the DuckDuckGo backend.
    pub const DUCKDUCKGO_DISPLAY_NAME: &'static str = "DuckDuckGo";
    /// User-facing label of the SearXNG backend.
    pub const SEARXNG_DISPLAY_NAME: &'static str = "SearXNG";

    /// Every backend name accepted as `[web_search] backend`, in display
    /// order. The error text for an unknown value is built from this list, so
    /// config validation cannot drift from what is actually implemented.
    pub const SUPPORTED_NAMES: &'static [&'static str] =
        &[Self::DUCKDUCKGO_NAME, Self::SEARXNG_NAME];

    /// The wire/config name of this backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => Self::DUCKDUCKGO_NAME,
            Self::SearXng => Self::SEARXNG_NAME,
        }
    }

    /// Human-readable backend name for user-facing labels (e.g. the
    /// `Web Search (…)` tool title).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::DuckDuckGo => Self::DUCKDUCKGO_DISPLAY_NAME,
            Self::SearXng => Self::SEARXNG_DISPLAY_NAME,
        }
    }

    /// Look up a backend by its config name; `None` for an unsupported name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            Self::DUCKDUCKGO_NAME => Some(Self::DuckDuckGo),
            Self::SEARXNG_NAME => Some(Self::SearXng),
            _ => None,
        }
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
    /// The backend answered with a non-success HTTP status that carries a
    /// diagnosis worth passing on — currently SearXNG's `403` for instances
    /// that disabled JSON output.
    #[error("web search backend responded with HTTP {status}: {hint}")]
    StatusWithHint { status: u16, hint: String },
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

/// Why [`backend_for`] could not build the configured backend.
///
/// Construction only ever yields the configured backend or one of these —
/// there is no fallback to a different backend.
#[derive(Debug, thiserror::Error)]
pub enum WebSearchBackendError {
    /// `[web_search] backend` names a backend this build does not implement.
    /// Config validation rejects the value first; this is the defensive path.
    #[error("unsupported web search backend '{backend_name}'; supported: {supported}")]
    UnsupportedBackend {
        backend_name: String,
        supported: String,
    },
    /// `[web_search] backend = "searxng"` without the configuration key its
    /// instance URL lives in.
    #[error("web search backend 'searxng' requires {missing_key} to name the SearXNG instance")]
    MissingBackendSetting { missing_key: String },
}

/// Build the backend named by `backend_name`.
///
/// `[web_search] backend` is validated at config load, so an error here is a
/// defensive-only path: registration must never fail over it to a different
/// backend. Returns an `Arc` so the caller can hand the instance straight to
/// the tool.
pub fn backend_for(
    backend_name: &str,
    base_url: Option<&str>,
    http_timeout: Duration,
) -> Result<Arc<dyn WebSearchBackend>, WebSearchBackendError> {
    match WebSearchBackendKind::from_name(backend_name) {
        Some(WebSearchBackendKind::DuckDuckGo) => {
            Ok(Arc::new(DuckDuckGoBackend::new(http_timeout)))
        }
        Some(WebSearchBackendKind::SearXng) => {
            let Some(base_url) = base_url.filter(|value| !value.trim().is_empty()) else {
                return Err(WebSearchBackendError::MissingBackendSetting {
                    missing_key: "[web_search.backends.searxng].base_url".to_string(),
                });
            };
            Ok(Arc::new(SearXngBackend::with_base_url(
                base_url.trim(),
                http_timeout,
            )))
        }
        None => Err(WebSearchBackendError::UnsupportedBackend {
            backend_name: backend_name.to_string(),
            supported: WebSearchBackendKind::supported_names_display(),
        }),
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
        assert_eq!(
            WebSearchBackendKind::from_name("searxng"),
            Some(WebSearchBackendKind::SearXng)
        );
        assert_eq!(WebSearchBackendKind::from_name("startpage"), None);
        assert_eq!(WebSearchBackendKind::DuckDuckGo.as_str(), "duckduckgo");
        assert_eq!(WebSearchBackendKind::SearXng.as_str(), "searxng");
        assert_eq!(
            WebSearchBackendKind::DuckDuckGo.display_name(),
            "DuckDuckGo"
        );
        assert_eq!(WebSearchBackendKind::SearXng.display_name(), "SearXNG");
        assert_eq!(
            WebSearchBackendKind::supported_names_display(),
            "duckduckgo, searxng"
        );
    }

    #[test]
    fn every_supported_name_is_selectable_and_roundtrips() {
        for supported in WebSearchBackendKind::SUPPORTED_NAMES {
            let kind = WebSearchBackendKind::from_name(supported)
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
        let duckduckgo = backend_for("duckduckgo", None, Duration::from_secs(1))
            .expect("the default backend always builds");
        assert!(format!("{duckduckgo:?}").contains("DuckDuckGoBackend"));

        let searxng = backend_for(
            "searxng",
            Some("https://searx.example.org"),
            Duration::from_secs(1),
        )
        .expect("searxng with an instance URL builds");
        assert!(format!("{searxng:?}").contains("SearXngBackend"));

        let unsupported = backend_for("something-else", None, Duration::from_secs(1))
            .expect_err("an unsupported name must not build a substituted backend");
        assert!(
            unsupported.to_string().contains("something-else")
                && unsupported.to_string().contains("duckduckgo, searxng"),
            "{unsupported}"
        );

        let missing_url = backend_for("searxng", None, Duration::from_secs(1))
            .expect_err("searxng without base_url must not build a substituted backend");
        assert!(
            missing_url.to_string().contains("base_url"),
            "the error names the missing key: {missing_url}"
        );
    }
}
