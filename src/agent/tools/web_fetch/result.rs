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

//! Text/media rendering, token-budget fitting and metadata of `web_fetch` results.

use base64::Engine as _;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::backends::web_fetch::{WebFetchBody, WebFetchResponse};
use super::super::read::{
    safe_source_filename, SUPPORTED_IMAGE_MEDIA_TYPES, SUPPORTED_MEDIA_TYPES,
};
use super::super::{
    utf8_text_preview, ContentSection, Section, ToolAttachment, ToolCallResult, STANDARD_BASE64,
};

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
    /// Text type autodetected from the body (`text/html` or `text/plain`);
    /// only present when the server sent no Content-Type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_content_type: Option<String>,
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
    /// Text type autodetected when no Content-Type was sent.
    detected_content_type: Option<String>,
    /// Model-facing content: converted/verbatim text, or standard padded
    /// Base64 of a complete media body.
    text: String,
    /// UTF-8 text projection for user-facing previews when `text` carries
    /// Base64 media bytes.
    text_preview: Option<String>,
    /// Recognized media type when the body is complete supported media.
    media_type: Option<String>,
    /// `true` when `text` was cut; reported as `truncated: true` metadata
    /// and the `Partial output` content-section label.
    truncated: bool,
}

impl WebFetchResult {
    /// Project a backend response into the model-facing result: text bodies
    /// pass through (already capped by the backend), media becomes Base64
    /// plus attachment metadata (complete bodies only — the backend rejects
    /// oversize media with [`WebFetchError::TooLarge`] instead).
    pub(super) fn new(url: String, response: WebFetchResponse) -> Self {
        let WebFetchResponse {
            final_url,
            status,
            content_type,
            detected_content_type,
            body,
            truncated,
        } = response;
        match body {
            WebFetchBody::Text { text } => WebFetchResult {
                url,
                final_url,
                status,
                content_type,
                detected_content_type,
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
                detected_content_type,
                text: STANDARD_BASE64.encode(&bytes),
                text_preview: utf8_text_preview(&bytes),
                media_type: Some(media_type),
                truncated: false,
            },
        }
    }

    /// Fit the model-facing text to the per-request context allowance. Text is
    /// cut and flagged as truncated; a complete media body cannot be cut
    /// without corrupting the attachment, so it is rejected instead.
    pub(super) fn fit_to_token_budget(&mut self, max_output_tokens: usize) -> Result<(), String> {
        let estimator = crate::llm::TokenEstimator::Tiktoken;
        if self.media_type.is_some() {
            if estimator.estimate_tokens(&self.text) > max_output_tokens {
                return Err(
                    "media result exceeds the available context token allowance; fetch fewer \
                     resources per batch or a text-oriented resource"
                        .to_string(),
                );
            }
            return Ok(());
        }
        let (text, truncated) = estimator.truncate(&self.text, max_output_tokens);
        if truncated {
            self.text = text;
            self.truncated = true;
        }
        Ok(())
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
        if let Some(detected) = &self.detected_content_type {
            facts.push(format!("detected_content_type={detected} (autodetected)"));
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
            detected_content_type: self.detected_content_type.clone(),
            media_type: self.media_type.clone(),
            truncated: self.truncated.then_some(true),
        })
        .ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        let mut schema = serde_json::to_value(schema_for!(WebFetchResultMetadata)).ok()?;
        let properties = schema.get_mut("properties")?.as_object_mut()?;
        if self.detected_content_type.is_none() {
            properties.remove("detected_content_type");
        }
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
            let label = Some(
                if self.truncated {
                    "Partial output"
                } else {
                    "Output"
                }
                .to_string(),
            );
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
