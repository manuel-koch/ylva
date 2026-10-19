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

//! Invocation object and execution of a single `web_fetch` call.

use std::time::Duration;

use serde_json::Value;
use tracing::Instrument;

use super::super::backends::web_fetch::{network_target, HopPolicy, WebFetchError};
use super::super::{Tool, ToolCall, ToolCallResult, ToolError};
use super::result::WebFetchResult;
use super::{WebFetchInputArgs, WebFetchTool};
use crate::sandbox::ApprovalCheck;

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
        WebFetchError::InvalidUrl { .. }
        | WebFetchError::Status { .. }
        | WebFetchError::Refused { .. } => error.to_string(),
    };
    ToolError::failed(WebFetchTool::NAME, message)
}

/// Invocation object for a single `web_fetch` tool call.
///
/// Carries a clone of the creating tool so schema-derived argument ranking
/// and the tool's settings stay in sync without re-deriving them here.
pub struct WebFetchCall {
    pub(super) id: uuid::Uuid,
    pub(super) raw_arguments: Value,
    pub(super) tool: WebFetchTool,
    pub(super) runtime_output_token_limit: Option<usize>,
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
                let WebFetchCall {
                    raw_arguments,
                    tool,
                    runtime_output_token_limit,
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
                // Redirect hops are never prompted for; only an explicit
                // `deny` rule refuses a hop, and private addresses need a
                // `network_private_address` rule.
                let deny = |target: &str| match tool.sandbox_context.check_network_request(target) {
                    Some(ApprovalCheck::AutoDenied { reason }) => Some(reason),
                    _ => None,
                };
                let allow_private_address =
                    |target: &str| tool.sandbox_context.check_private_address(target);
                let policy = HopPolicy {
                    deny: &deny,
                    allow_private_address: &allow_private_address,
                };
                let response =
                    tokio::time::timeout(outer_timeout, tool.backend.fetch(&args.url, policy))
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
                let mut result = WebFetchResult::new(args.url, response);
                if let Some(max_output_tokens) = runtime_output_token_limit {
                    result
                        .fit_to_token_budget(max_output_tokens)
                        .map_err(|message| ToolError::failed(WebFetchTool::NAME, message))?;
                }
                Ok(Box::new(result) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
}
