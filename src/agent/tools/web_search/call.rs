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

//! Invocation object and execution of a single `web_search` call.

use serde_json::Value;
use tracing::Instrument;

use super::super::backends::web_search::WEB_SEARCH_MAX_RESULT_LIMIT;
use super::super::{Tool, ToolCall, ToolCallResult, ToolError};
use super::result::WebSearchResult;
use super::throttle::wait_before_request;
use super::{WebSearchInputArgs, WebSearchTool};

/// Invocation object for a single `web_search` tool call.
///
/// Carries a clone of the creating tool so schema-derived argument ranking
/// and the tool's settings stay in sync without re-deriving them here.
pub struct WebSearchCall {
    pub(super) id: uuid::Uuid,
    pub(super) raw_arguments: Value,
    pub(super) tool: WebSearchTool,
    pub(super) runtime_output_token_limit: Option<usize>,
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
