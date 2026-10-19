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

//! Invocation object and execution of a single `search` call.

use serde_json::Value;
use tracing::Instrument;

use super::super::{Tool, ToolCall, ToolCallResult, ToolError};
use super::engine::search_files;
use super::{SearchInputArgs, SearchTool};

/// Invocation object for a single `search` tool call.
pub struct SearchCall {
    pub(super) id: uuid::Uuid,
    pub(super) raw_arguments: Value,
    pub(super) timeout_secs: u64,
    pub(super) runtime_output_token_limit: Option<usize>,
}

impl ToolCall for SearchCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        SearchTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn argument_order(&self, key: &str) -> usize {
        SearchTool::default().argument_order(key)
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
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
        let runtime_output_token_limit = self.runtime_output_token_limit;
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let args = serde_json::from_value::<SearchInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("search", format!("invalid arguments: {error}")),
                )?;
                let mut result = search_files(args).await?;
                if let Some(max_output_tokens) = runtime_output_token_limit {
                    result.truncate_to_token_budget(max_output_tokens);
                }
                Ok(Box::new(result) as Box<dyn ToolCallResult>)
            }
            .instrument(span),
        )
    }
}
