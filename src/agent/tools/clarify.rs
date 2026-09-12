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

use futures::FutureExt;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;

use super::{
    validate_args_with_schema, ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError,
};
use crate::sandbox::ApprovalCheck;

#[derive(JsonSchema, Deserialize)]
#[expect(dead_code)]
struct ClarifyArgs {
    #[schemars(
        description = "Question shown to the user. Do not embed choices here — use `choices`. Markdown rendered; use a real newline, never `\\n`."
    )]
    question: String,
    #[serde(default)]
    #[schemars(
        description = "Optional Markdown context shown with the question. Real newline, never `\\n`."
    )]
    context: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Up to 5 choices for the user to pick from (max 5). Free-text answers are still accepted.",
        length(max = 5)
    )]
    choices: Option<Vec<String>>,
}

pub struct ClarifyTool;

impl ClarifyTool {
    pub const NAME: &'static str = "clarify";

    /// Returns the display sections for a completed clarify call — the context
    /// (if any) and the choices list (if any). These are the tool's own
    /// decisions about what to show in the chat bubble after the user answers.
    pub fn answer_display_sections(arguments: &Value) -> Vec<Section> {
        let mut sections = vec![];

        if let Some(ctx) = arguments.get("context").and_then(|v| v.as_str()) {
            if !ctx.is_empty() {
                sections.push(Section::Content(ContentSection {
                    label: Some("Context".to_string()),
                    content: ctx.to_string(),
                    file_extension: None,
                    line_number_offset: None,
                }));
            }
        }

        if let Some(choices) = arguments
            .get("choices")
            .and_then(|v| v.as_array())
            .filter(|list| !list.is_empty())
        {
            let choices_text = choices
                .iter()
                .filter_map(|v| v.as_str())
                .enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(Section::Content(ContentSection {
                label: Some("Choices".to_string()),
                content: choices_text,
                file_extension: None,
                line_number_offset: None,
            }));
        }

        sections
    }
}

pub struct ClarifyCall {
    id: uuid::Uuid,
    raw_arguments: Value,
}

impl Tool for ClarifyTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question. Pass options via `choices` — never embed them in \
        `question`; free text is always accepted regardless. `question`/`context` render as \
        Markdown; use real newlines, never `\\n`."
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(ClarifyArgs)
    }

    fn validate_args(&self, arguments: &Value) -> Result<(), String> {
        validate_args_with_schema(&self.input_schema(), arguments)?;
        if let Some(choices) = arguments.get("choices").and_then(|value| value.as_array()) {
            if choices.len() > 5 {
                return Err("`choices` must contain at most 5 items".to_string());
            }
        }
        Ok(())
    }

    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        ApprovalCheck::UserApprovalRequired
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    /// The interactive wait is unbounded; `u64::MAX` signals no timeout.
    fn timeout_secs(&self) -> u64 {
        u64::MAX
    }

    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(ClarifyCall {
            id,
            raw_arguments: arguments,
        })
    }
}

impl ToolCall for ClarifyCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn name(&self) -> &str {
        ClarifyTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn timeout_secs(&self) -> u64 {
        u64::MAX
    }

    fn is_body_argument(&self, key: &str) -> bool {
        matches!(key, "context" | "choices")
    }

    fn argument_order(&self, key: &str) -> usize {
        let tool = ClarifyTool;
        tool.argument_order(key)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        let tool = ClarifyTool;
        tool.non_default_redacted_arguments(&self.raw_arguments)
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        async move {
            Err(ToolError::failed(
                "clarify",
                "clarify is not available in headless mode",
            ))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_count_within_limit_is_accepted() {
        let tool = ClarifyTool;
        let args = serde_json::json!({
            "question": "Pick one",
            "choices": ["a", "b", "c", "d", "e"]
        });
        let result = tool.validate_args(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn choice_count_exceeding_limit_is_rejected() {
        let tool = ClarifyTool;
        let args = serde_json::json!({
            "question": "Pick one",
            "choices": ["a", "b", "c", "d", "e", "f"]
        });
        let result = tool.validate_args(&args);
        assert!(result.is_err());
    }

    #[test]
    fn title_arguments_only_include_the_question() {
        let call = ClarifyTool.create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "question": "Pick one",
                "context": "Supporting details",
                "choices": ["a", "b"],
            }),
        );

        assert_eq!(
            call.title_arguments(),
            vec![(
                "question".to_string(),
                serde_json::Value::String("Pick one".to_string())
            )]
        );
    }

    #[test]
    fn input_schema_fields_have_descriptions() {
        let schema = ClarifyTool.input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "ClarifyArgs");
    }
}
