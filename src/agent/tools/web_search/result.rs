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

//! Numbered-list rendering, token-budget fitting and metadata of `web_search` results.

use schemars::schema_for;
use serde_json::Value;

use super::super::backends::web_search::{WebSearchHit, WEB_SEARCH_SNIPPET_MAX_CHARS};
use super::super::{ContentSection, Section, ToolCallResult};

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
    pub(super) fn new(mut hits: Vec<WebSearchHit>) -> Self {
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
    pub(super) fn fit_to_token_budget(&mut self, max_output_tokens: usize) {
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
            label: Some("Output".to_string()),
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
