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

//! `spawn_subagent`: delegate a task to a one-level-deep child agent.
//!
//! Structurally absent from `Role::Child` registries — a child cannot itself
//! spawn grandchildren in Phase 6. Only registered for `Role::Root` agents
//! whose `AgentConfig::can_spawn` is `true` (`AgentFactory::create` decides
//! this, not this module).
//!
//! Unlike most tools, [`SpawnSubagentCall::run`] is never actually invoked:
//! `AgentLoop::run` recognises this tool by name once its call is approved
//! and resolves it directly against the agent's `ChildAgentManager` — the
//! manager lives on the outer `AgentLoop`, which a `ToolCall` object has no
//! access to. The `run` stub mirrors `clarify`'s headless-mode error for the
//! same reason: any codepath that reaches it is a bug in the agent loop's
//! special-casing, not a normal execution path.

use futures::FutureExt;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::{
    ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::ApprovalCheck;

/// The default agent-type name used when a `spawn_subagent` call omits
/// `agent_type`.
pub const DEFAULT_CHILD_AGENT_TYPE: &str = "default";

fn default_agent_type() -> String {
    DEFAULT_CHILD_AGENT_TYPE.to_string()
}

/// `spawn_subagent` arguments.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct SpawnSubagentArgs {
    /// 3-6 word label for what the child is doing, e.g. "Refactor auth token validation".
    /// Shown in the (possibly collapsed) tool-call bubble title and included in
    /// the later completion event so the parent can correlate results without
    /// relying on completion order.
    pub label: String,
    /// Self-contained instruction and expected deliverable for the child — it
    /// has no knowledge of this conversation beyond `briefing`.
    pub task: String,
    /// Optional context/background injected ahead of `task` in the child's own
    /// conversation, so it need not be repeated in `task`.
    #[serde(default)]
    pub briefing: Option<String>,
    /// Agent-type configuration name the child runs as (default: `"default"`).
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
}

pub struct SpawnSubagentTool {
    auto_approve: bool,
}

impl SpawnSubagentTool {
    pub const NAME: &'static str = "spawn_subagent";

    pub fn new(auto_approve: bool) -> Self {
        SpawnSubagentTool { auto_approve }
    }
}

pub struct SpawnSubagentCall {
    id: Uuid,
    raw_arguments: Value,
}

impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Delegate a self-contained task to a background child agent. The immediate tool result \
         is admission only, not the child's answer: it has the JSON shape \
         {\"event\":\"subagent_accepted\",\"status\":\"running\"|\"queued\",\"run_id\":\"...\", \
         \"outstanding_count\":N,\"all_done\":false}. Never invent a tool call merely to wait \
         (no sleep, status-only command, or polling). A terminal completion is delivered later \
         as a hidden harness notice, not as a second tool result; it contains the child's \
         run_id, tool_call_id, label, status, result/error/sources, outstanding_count, and \
         all_done. Completion order is not a reliable ordering signal. Do not acknowledge an \
         intermediate completion or tell the user to wait. Use a result only when it helps with \
         genuinely useful work; when all_done is true, combine the terminal results and answer \
         the user. The harness handles the continuation, so never claim a child completed from \
         the admission result alone."
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(SpawnSubagentArgs)
    }

    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        if self.auto_approve {
            ApprovalCheck::AutoApproved
        } else {
            ApprovalCheck::UserApprovalRequired
        }
    }

    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        key == "agent_type" && value.as_str() == Some(DEFAULT_CHILD_AGENT_TYPE)
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
        Box::new(SpawnSubagentCall {
            id,
            raw_arguments: arguments,
        })
    }
}

impl ToolCall for SpawnSubagentCall {
    fn id(&self) -> Uuid {
        self.id
    }

    fn name(&self) -> &str {
        SpawnSubagentTool::NAME
    }

    fn arguments(&self) -> &Value {
        &self.raw_arguments
    }

    fn timeout_secs(&self) -> u64 {
        DEFAULT_TOOL_TIMEOUT_SECS
    }

    fn is_body_argument(&self, key: &str) -> bool {
        key == "task" || key == "briefing"
    }

    fn pending_content_sections(&self) -> Vec<Section> {
        let mut sections = vec![];
        let task = self
            .raw_arguments
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !task.is_empty() {
            sections.push(Section::Content(ContentSection {
                label: Some("Task".to_string()),
                content: task.to_string(),
                file_extension: Some("md".to_string()),
                line_number_offset: None,
            }));
        }
        let briefing = self
            .raw_arguments
            .get("briefing")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !briefing.is_empty() {
            sections.push(Section::Content(ContentSection {
                label: Some("Briefing".to_string()),
                content: briefing.to_string(),
                file_extension: Some("md".to_string()),
                line_number_offset: None,
            }));
        }
        sections
    }

    fn argument_order(&self, key: &str) -> usize {
        SpawnSubagentTool::new(false).argument_order(key)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        let tool = SpawnSubagentTool::new(false);
        tool.non_default_redacted_arguments(&self.raw_arguments)
    }

    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        async move {
            Err(ToolError::failed(
                SpawnSubagentTool::NAME,
                "spawn_subagent must be resolved by the agent loop against its \
                 ChildAgentManager; it cannot be executed as a standalone tool call",
            ))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_approval_defaults_to_user_approval_required() {
        let tool = SpawnSubagentTool::new(false);
        assert_eq!(
            tool.needs_approval(&serde_json::json!({"task": "do it"})),
            ApprovalCheck::UserApprovalRequired
        );
    }

    #[test]
    fn needs_approval_is_auto_approved_when_configured() {
        let tool = SpawnSubagentTool::new(true);
        assert_eq!(
            tool.needs_approval(&serde_json::json!({"task": "do it"})),
            ApprovalCheck::AutoApproved
        );
    }

    #[test]
    fn agent_type_defaults_when_omitted() {
        let args: SpawnSubagentArgs =
            serde_json::from_value(serde_json::json!({"label": "Do it", "task": "do it"})).unwrap();
        assert_eq!(args.agent_type, DEFAULT_CHILD_AGENT_TYPE);
    }

    #[test]
    fn is_body_argument_true_for_task_and_briefing_only() {
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({"label": "Do it", "task": "do it", "briefing": "context", "agent_type": "default"}),
        );
        assert!(call.is_body_argument("task"));
        assert!(call.is_body_argument("briefing"));
        assert!(!call.is_body_argument("label"));
        assert!(!call.is_body_argument("agent_type"));
    }

    #[test]
    fn label_is_the_primary_title_argument_ranked_ahead_of_agent_type() {
        // `label` gives the LLM a way to summarise the child's purpose so
        // even a collapsed bubble hints at what it's doing. It must rank
        // before every other non-body argument so the bubble renderer picks
        // it as the bare/bold primary title text instead of `agent_type`.
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({
                "label": "Refactor auth token validation",
                "task": "do it",
                "briefing": "context",
                "agent_type": "research",
            }),
        );
        let title_arguments = call.title_arguments();
        assert_eq!(
            title_arguments,
            vec![
                (
                    "label".to_string(),
                    serde_json::json!("Refactor auth token validation")
                ),
                ("agent_type".to_string(), serde_json::json!("research")),
            ],
            "label must come first so it becomes the bubble's bare primary title text"
        );
    }

    #[test]
    fn pending_content_sections_includes_task_and_briefing_as_markdown() {
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(
            Uuid::new_v4(),
            serde_json::json!({"task": "# Do the thing", "briefing": "Some *context*."}),
        );
        let sections = call.pending_content_sections();
        assert_eq!(sections.len(), 2);
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Task"));
                assert_eq!(section.content, "# Do the thing");
                assert_eq!(section.file_extension.as_deref(), Some("md"));
            }
            _ => panic!("expected a Content section for task"),
        }
        match &sections[1] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Briefing"));
                assert_eq!(section.content, "Some *context*.");
                assert_eq!(section.file_extension.as_deref(), Some("md"));
            }
            _ => panic!("expected a Content section for briefing"),
        }
    }

    #[test]
    fn pending_content_sections_omits_briefing_when_absent() {
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(Uuid::new_v4(), serde_json::json!({"task": "do it"}));
        let sections = call.pending_content_sections();
        assert_eq!(sections.len(), 1);
        match &sections[0] {
            Section::Content(section) => assert_eq!(section.label.as_deref(), Some("Task")),
            _ => panic!("expected a Content section for task"),
        }
    }

    #[test]
    fn pending_content_sections_empty_when_task_is_empty() {
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(Uuid::new_v4(), serde_json::json!({"task": ""}));
        assert!(call.pending_content_sections().is_empty());
    }

    #[test]
    fn input_schema_fields_have_descriptions() {
        let schema = SpawnSubagentTool::new(false).input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "SpawnSubagentArgs");
    }

    #[tokio::test]
    async fn run_returns_error_stub() {
        let tool = SpawnSubagentTool::new(false);
        let call = tool.create_call(Uuid::new_v4(), serde_json::json!({"task": "do it"}));
        let result = call.run().await;
        assert!(result.is_err());
    }
}
