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

//! `agent_finish`: the tool a subagent (`Role::Subagent`) calls to report its
//! outcome back to the parent. Injected only into `Role::Subagent` tool
//! registries — `AgentFactory::create` never registers it for `Role::Root`.
//!
//! Calling `agent_finish` sends a [`FinishResult`] over the subagent's `finish_tx`
//! immediately (not deferred to a later `ToolDone`/re-prompt cycle); the
//! surrounding `AgentLoop::run` loop recognises this tool by name and exits
//! the subagent's ReAct loop unconditionally afterwards — the only tool call
//! exempt from `DEFAULT_MAX_TOOL_USE_ITERATIONS`.

use futures::FutureExt;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{
    ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::ApprovalCheck;
use crate::subagent_manager::{FinishResult, FinishResultOrigin};

/// The status a subagent reports for its own run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentFinishStatus {
    Success,
    Failure,
}

impl AgentFinishStatus {
    fn as_str(self) -> &'static str {
        match self {
            AgentFinishStatus::Success => "success",
            AgentFinishStatus::Failure => "failure",
        }
    }
}

/// `agent_finish` arguments: the subagent's final report to its parent.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct AgentFinishArgs {
    /// Whether the task succeeded or failed. Required — always specify this,
    /// and emit it as the first field in your call. If omitted (weaker
    /// models sometimes drop it), it is inferred as `"failure"` when `error`
    /// is set and `"success"` otherwise, so the call still completes rather
    /// than being rejected.
    #[serde(default)]
    pub status: Option<AgentFinishStatus>,
    /// Final answer for the parent when `status` is `"success"`. Must be
    /// self-contained — the parent never sees prior turns, tool output, or
    /// reasoning; inline the actual findings/content here, never "see above".
    /// Keep it concise and non-redundant, proportional to the task. Include
    /// only the deliverable itself — no meta-commentary, self-review, or
    /// narration about your process (e.g. "Self-Correction/Review:", "I will
    /// now use agent_finish"). Keep such reasoning in your own turns, never
    /// in this field.
    #[serde(default)]
    pub result: Option<String>,
    /// What went wrong, when `status` is `"failure"`.
    #[serde(default)]
    pub error: Option<String>,
    /// Paths, URLs, or other identifiers consulted while producing `result`.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
}

impl AgentFinishArgs {
    pub(crate) fn resolved_status(&self) -> AgentFinishStatus {
        self.status.unwrap_or_else(|| {
            if self.error.is_some() {
                AgentFinishStatus::Failure
            } else {
                AgentFinishStatus::Success
            }
        })
    }
}

/// Configuration a specific subagent run's `agent_finish` tool needs to construct and
/// send its [`FinishResult`].
#[derive(Debug, Clone)]
pub struct AgentFinishToolConfig {
    pub finish_tx: mpsc::Sender<FinishResult>,
    pub run_id: Uuid,
    /// The parent's `spawn_subagent` tool-call id this run is tied to.
    pub call_id: Uuid,
}

pub struct AgentFinishTool {
    config: AgentFinishToolConfig,
}

impl AgentFinishTool {
    pub const NAME: &'static str = "agent_finish";

    pub fn new(config: AgentFinishToolConfig) -> Self {
        AgentFinishTool { config }
    }
}

pub struct AgentFinishCall {
    id: Uuid,
    raw_arguments: Value,
    config: AgentFinishToolConfig,
}

#[derive(Debug)]
struct AgentFinishAck(String);

impl ToolCallResult for AgentFinishAck {
    fn text(&self) -> String {
        self.0.clone()
    }
}

impl Tool for AgentFinishTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Report your final outcome to the parent agent that spawned you. Call exactly once, \
         only when the task is complete (success or failure); ends your run immediately — \
         call no other tool afterwards. Only `result`/`error` reach the parent, never prior \
         messages or reasoning — `result` must fully restate every finding, not merely refer \
         back to earlier turns. Keep the handoff concise and non-redundant, proportional to the \
         task. Do not paraphrase your reasoning process or add self-review/self-correction \
         commentary before calling this tool or inside its arguments — only the finished \
         deliverable belongs in `result`/`error`."
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(AgentFinishArgs)
    }

    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        ApprovalCheck::AutoApproved
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        DEFAULT_TOOL_TIMEOUT_SECS
    }

    fn create_call(&self, id: Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(AgentFinishCall {
            id,
            raw_arguments: arguments,
            config: self.config.clone(),
        })
    }
}

impl ToolCall for AgentFinishCall {
    fn id(&self) -> Uuid {
        self.id
    }

    fn name(&self) -> &str {
        AgentFinishTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn timeout_secs(&self) -> u64 {
        DEFAULT_TOOL_TIMEOUT_SECS
    }

    fn is_body_argument(&self, key: &str) -> bool {
        matches!(key, "result" | "error" | "sources")
    }

    fn pending_content_sections(&self) -> Vec<Section> {
        let text_argument = |key: &str| {
            self.raw_arguments
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let sources = self
            .raw_arguments
            .get("sources")
            .and_then(Value::as_array)
            .map(|sources| {
                sources
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty());
        [
            ("Error", text_argument("error"), Some("md")),
            ("Sources", sources, None),
            ("Result", text_argument("result"), Some("md")),
        ]
        .into_iter()
        .filter_map(|(label, content, file_extension)| {
            content.map(|content| {
                Section::Content(ContentSection {
                    label: Some(label.to_string()),
                    content,
                    file_extension: file_extension.map(str::to_string),
                    line_number_offset: None,
                })
            })
        })
        .collect()
    }

    fn argument_order(&self, key: &str) -> usize {
        match key {
            "status" => 0,
            "result" => 1,
            "error" => 2,
            "sources" => 3,
            _ => usize::MAX,
        }
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        async move {
            let args: AgentFinishArgs = serde_json::from_value(self.raw_arguments.clone())
                .map_err(|error| ToolError::failed(AgentFinishTool::NAME, error.to_string()))?;
            // `status` is optional on the wire: some models omit it. Infer it
            // from `error`/`result` so the call still completes instead of
            // being rejected — see `AgentFinishArgs::status` doc comment.
            let status = args.resolved_status();
            let finish_result = FinishResult {
                run_id: self.config.run_id,
                call_id: self.config.call_id,
                origin: FinishResultOrigin::Agent,
                status: status.as_str().to_string(),
                result: args.result,
                error: args.error,
                sources: args.sources.unwrap_or_default(),
            };
            // The receiving end (the parent's `AgentLoop`) always outlives
            // this send; a closed channel only happens if the parent has
            // already torn down, in which case there is nothing left to
            // notify and the send failure is safely ignored.
            let _ = self.config.finish_tx.send(finish_result).await;
            let output: Box<dyn ToolCallResult> = Box::new(AgentFinishAck(format!(
                "Reported {} to the parent agent.",
                status.as_str()
            )));
            Ok(output)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AgentFinishToolConfig {
        let (finish_tx, _finish_rx) = mpsc::channel(1);
        AgentFinishToolConfig {
            finish_tx,
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn result_error_and_sources_move_from_the_title_into_sections() {
        let tool = AgentFinishTool::new(test_config());
        let arguments = serde_json::json!({
            "status": "success",
            "result": "## Findings\n- one",
            "sources": ["a.md", "b.md"],
        });
        let call = tool.create_call(Uuid::new_v4(), arguments);

        let title_keys: Vec<String> = call.title_arguments().into_iter().map(|(k, _)| k).collect();
        assert_eq!(title_keys, ["status"]);

        let sections: Vec<(String, String)> = call
            .pending_content_sections()
            .into_iter()
            .map(|section| match section {
                Section::Content(content) => (content.label.unwrap(), content.content),
                other => panic!("unexpected section {other:?}"),
            })
            .collect();
        assert_eq!(
            sections,
            [
                ("Sources".to_string(), "a.md\nb.md".to_string()),
                ("Result".to_string(), "## Findings\n- one".to_string()),
            ]
        );
    }

    #[test]
    fn needs_approval_is_auto_approved() {
        let tool = AgentFinishTool::new(test_config());
        let approval = tool.needs_approval(&serde_json::json!({"status": "success"}));
        assert_eq!(approval, ApprovalCheck::AutoApproved);
    }

    #[test]
    fn input_schema_fields_have_descriptions() {
        let schema = AgentFinishTool::new(test_config()).input_schema();
        crate::test_support::assert_all_properties_have_descriptions(&schema, "AgentFinishArgs");
    }

    #[test]
    fn description_requests_a_concise_task_proportional_handoff() {
        let tool = AgentFinishTool::new(test_config());
        let description = tool.description();

        assert!(description.contains("concise and non-redundant, proportional to the task"));
    }

    #[tokio::test]
    async fn run_sends_finish_result_and_returns_ack() {
        let (finish_tx, mut finish_rx) = mpsc::channel(1);
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let config = AgentFinishToolConfig {
            finish_tx,
            run_id,
            call_id,
        };
        let tool = AgentFinishTool::new(config);
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({
                "status": "success",
                "result": "all done",
                "sources": ["a.rs"],
            }),
        );
        let output = call.run().await.expect("finish call succeeds");
        assert!(output.text().contains("success"));

        let finish_result = finish_rx.recv().await.expect("finish result sent");
        assert_eq!(finish_result.run_id, run_id);
        assert_eq!(finish_result.call_id, call_id);
        assert_eq!(finish_result.origin, FinishResultOrigin::Agent);
        assert_eq!(finish_result.status, "success");
        assert_eq!(finish_result.result.as_deref(), Some("all done"));
        assert_eq!(finish_result.sources, vec!["a.rs".to_string()]);
    }

    #[tokio::test]
    async fn run_infers_success_status_when_status_is_omitted_but_result_is_present() {
        let (finish_tx, mut finish_rx) = mpsc::channel(1);
        let tool = AgentFinishTool::new(AgentFinishToolConfig {
            finish_tx,
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
        });
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({"result": "all done, no status given"}),
        );
        let output = call.run().await.expect("finish call succeeds");
        assert!(output.text().contains("success"));
        let finish_result = finish_rx.recv().await.expect("finish result sent");
        assert_eq!(finish_result.status, "success");
    }

    #[tokio::test]
    async fn run_infers_failure_status_when_status_is_omitted_but_error_is_present() {
        let (finish_tx, mut finish_rx) = mpsc::channel(1);
        let tool = AgentFinishTool::new(AgentFinishToolConfig {
            finish_tx,
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
        });
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({"error": "could not complete the task, no status given"}),
        );
        let output = call.run().await.expect("finish call succeeds");
        assert!(output.text().contains("failure"));
        let finish_result = finish_rx.recv().await.expect("finish result sent");
        assert_eq!(finish_result.status, "failure");
    }

    #[test]
    fn input_schema_does_not_require_status() {
        let schema = AgentFinishTool::new(test_config()).input_schema();
        assert!(
            !schema
                .schema
                .object
                .as_ref()
                .expect("object schema")
                .required
                .contains("status"),
            "status must stay optional in the schema so omitting it is not auto-denied"
        );
    }

    #[tokio::test]
    async fn run_reports_failure_status_and_error() {
        let (finish_tx, mut finish_rx) = mpsc::channel(1);
        let config = AgentFinishToolConfig {
            finish_tx,
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
        };
        let tool = AgentFinishTool::new(config);
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({
                "status": "failure",
                "error": "could not complete the task",
            }),
        );
        let _ = call.run().await.expect("finish call succeeds");
        let finish_result = finish_rx.recv().await.expect("finish result sent");
        assert_eq!(finish_result.origin, FinishResultOrigin::Agent);
        assert_eq!(finish_result.status, "failure");
        assert_eq!(
            finish_result.error.as_deref(),
            Some("could not complete the task")
        );
        assert!(finish_result.result.is_none());
    }
}
