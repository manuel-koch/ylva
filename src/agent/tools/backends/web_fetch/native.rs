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

//! The native [`WebFetchBackend`]: a plain local HTTP `GET` via `reqwest`.

use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::StreamExt;

use super::{
    convert_body_to_result, network_target, truncate_text_output, WebFetchBackend, WebFetchBody,
    WebFetchError, WebFetchResponse,
};
use crate::agent::tools::read::{supported_media_type, READ_MEDIA_MAX_BYTES};
use crate::http::OUTBOUND_USER_AGENT;

const SNIFF_PREFIX_BYTES: usize = 32;

/// The native backend: a plain local HTTP `GET` via `reqwest` with the Ylva
/// user-agent. Redirects are followed normally (up to `reqwest`'s default
/// hop limit); known caveat: a host-scoped `network_request` `target` rule
/// evaluates the requested URL only and is not re-checked per redirect hop.
#[derive(Debug)]
pub struct HttpFetchBackend {
    client: reqwest::Client,
    dump_source: crate::http::RequestDumpSource,
}

impl HttpFetchBackend {
    /// Build a backend. `http_timeout` bounds a single request (the tool's
    /// outer timeout must exceed it).
    pub fn new(http_timeout: Duration) -> Self {
        Self::with_dump_source(http_timeout, crate::http::RequestDumpSource::Environment)
    }

    /// Build a backend with an explicit dump-path template.
    ///
    /// This keeps tests on the actual request/dump path without mutating the
    /// process environment; production callers normally use [`Self::new`],
    /// which resolves `YLVA_REQUEST_DUMP_PATH` when each request starts.
    pub fn with_dump_path(http_timeout: Duration, dump_path: &Path) -> Self {
        Self::with_dump_source(
            http_timeout,
            crate::http::RequestDumpSource::from_template(dump_path),
        )
    }

    fn with_dump_source(
        http_timeout: Duration,
        dump_source: crate::http::RequestDumpSource,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(OUTBOUND_USER_AGENT)
            .timeout(http_timeout)
            .build()
            .expect("shared HTTP client construction must succeed");
        HttpFetchBackend {
            client,
            dump_source,
        }
    }
}

impl WebFetchBackend for HttpFetchBackend {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<WebFetchResponse, WebFetchError>> {
        Box::pin(async move {
            // Scheme allowlist before any socket work.
            network_target(url)?;

            let request = self
                .client
                .get(url)
                .build()
                .map_err(|error| WebFetchError::Request {
                    detail: error.to_string(),
                })?;
            let request_url = request.url().to_string();
            let dump_context = crate::http::RequestDumpContext::new(&self.dump_source);
            if let Some(path) = dump_context.request_path() {
                crate::http::dump_request(
                    &request_url,
                    &crate::http::redacted_headers_json(request.headers()),
                    &serde_json::Value::Null,
                    &path,
                );
            }
            // No retry: every failure is terminal (see the module docs).
            let response = self.client.execute(request).await.map_err(|error| {
                let response_dump_path = dump_context.response_path_for_content_type(None, "jsonl");
                crate::http::dump_response_transport_error(&response_dump_path, &error);
                WebFetchError::Request {
                    detail: error.to_string(),
                }
            })?;
            let status = response.status();
            let final_url = response.url().to_string();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let content_length = response.content_length();
            let response_dump_path =
                dump_context.response_path_for_content_type(content_type.as_deref(), "bin");
            if !status.is_success() {
                return Err(WebFetchError::Status {
                    url: final_url,
                    status: status.as_u16(),
                });
            }

            let limit = READ_MEDIA_MAX_BYTES;
            let content_length_over_limit =
                content_length.is_some_and(|length| length > limit as u64);
            let mut raw_body = Vec::new();
            let mut stream = response.bytes_stream();

            // Decide the body kind from a small prefix (magic numbers first)
            // so the size policy can differ by kind: a known-oversize media
            // body fails before it is downloaded, text is read up to `limit`.
            let mut sniffed_media = false;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    crate::http::dump_response_transport_error(&response_dump_path, &error);
                    WebFetchError::Request {
                        detail: error.to_string(),
                    }
                })?;
                raw_body.extend_from_slice(&chunk);
                if supported_media_type(&raw_body).is_some() {
                    sniffed_media = true;
                    break;
                }
                if raw_body.len() >= SNIFF_PREFIX_BYTES {
                    break;
                }
            }
            if sniffed_media && content_length_over_limit {
                return Err(WebFetchError::TooLarge {
                    size: content_length.unwrap_or_default() as usize,
                    limit,
                });
            }

            let mut truncated_raw = content_length_over_limit;
            if sniffed_media {
                // Media must arrive complete: a partial image or PDF is
                // unusable (mirrors `read`'s whole-file media policy).
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|error| {
                        crate::http::dump_response_transport_error(&response_dump_path, &error);
                        WebFetchError::Request {
                            detail: error.to_string(),
                        }
                    })?;
                    raw_body.extend_from_slice(&chunk);
                    if raw_body.len() > limit {
                        return Err(WebFetchError::TooLarge {
                            size: content_length.unwrap_or(raw_body.len() as u64) as usize,
                            limit,
                        });
                    }
                }
            } else {
                // Text is read to at most `limit` bytes; over the limit the
                // partial body is classified and converted, flagged truncated.
                while raw_body.len() < limit {
                    match stream.next().await {
                        None => break,
                        Some(Err(error)) => {
                            crate::http::dump_response_transport_error(&response_dump_path, &error);
                            return Err(WebFetchError::Request {
                                detail: error.to_string(),
                            });
                        }
                        Some(Ok(chunk)) => {
                            let remaining = limit - raw_body.len();
                            if chunk.len() > remaining {
                                raw_body.extend_from_slice(&chunk[..remaining]);
                                truncated_raw = true;
                                break;
                            }
                            raw_body.extend_from_slice(&chunk);
                        }
                    }
                }
                if !truncated_raw && raw_body.len() == limit && stream.next().await.is_some() {
                    // The stream carries more than `limit` bytes.
                    truncated_raw = true;
                }
                if truncated_raw {
                    // Cut on a UTF-8 character boundary so the partial text
                    // body stays convertible.
                    if let Err(utf8_error) = std::str::from_utf8(&raw_body) {
                        if utf8_error.error_len().is_none() {
                            raw_body.truncate(utf8_error.valid_up_to());
                        }
                    }
                }
            }
            crate::http::dump_buffered_response(&raw_body, response_dump_path.as_deref());

            let body = convert_body_to_result(content_type.as_deref(), &raw_body)?;
            let (body, truncated) = match body {
                WebFetchBody::Media { .. } if truncated_raw => {
                    return Err(WebFetchError::TooLarge {
                        size: content_length.unwrap_or(raw_body.len() as u64) as usize,
                        limit,
                    });
                }
                media @ WebFetchBody::Media { .. } => (media, false),
                WebFetchBody::Text { text } => {
                    let (text, output_truncated) = truncate_text_output(&text);
                    (
                        WebFetchBody::Text { text },
                        truncated_raw || output_truncated,
                    )
                }
            };
            Ok(WebFetchResponse {
                final_url,
                status: status.as_u16(),
                content_type,
                body,
                truncated,
            })
        })
    }
}
