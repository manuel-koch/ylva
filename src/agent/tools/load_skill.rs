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

//! On-demand delivery of a previously discovered, agent-specific skill snapshot.

use std::sync::Arc;

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError};
use crate::prompt::{format_prompt_block, PromptBlockKind};
use crate::sandbox::{ApprovalCheck, SandboxContext};
use crate::skills::policy::CallableSkills;
use crate::skills::Skill;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadSkillArgs {
    /// Exact name from this agent's advertised skill catalog, not a path.
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LoadSkillMetadata {
    /// Canonical skill name.
    pub name: String,
    /// Absolute canonical instruction source, captured at discovery.
    pub source: String,
}

#[derive(Debug)]
pub struct LoadSkillResult {
    skill: Arc<Skill>,
}

impl ToolCallResult for LoadSkillResult {
    fn text(&self) -> String {
        let source = format!(
            "Instruction file: {}\nResolve relative references against its parent directory: {}.\nSidecars require ordinary read approval; loading grants no execution or write permissions.",
            serde_json::to_string(&self.skill.source.canonical_path.to_string_lossy())
                .expect("path serializes"),
            serde_json::to_string(&self.skill.source.canonical_path.parent().expect("source has parent").to_string_lossy())
                .expect("path serializes"),
        );
        format!(
            "{}\n\n{}",
            self.skill.body,
            format_prompt_block(PromptBlockKind::SkillSource, &source)
        )
    }

    fn requires_complete_delivery(&self) -> bool {
        true
    }

    fn metadata(&self) -> Option<Value> {
        Some(
            serde_json::to_value(LoadSkillMetadata {
                name: self.skill.name.clone(),
                source: self
                    .skill
                    .source
                    .canonical_path
                    .to_string_lossy()
                    .into_owned(),
            })
            .expect("skill metadata serializes"),
        )
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schema_for!(LoadSkillMetadata))
    }

    fn display_summary(&self) -> Vec<(String, String)> {
        vec![]
    }

    fn content_sections(&self) -> Vec<Section> {
        vec![
            Section::Content(ContentSection {
                label: Some("Source".into()),
                content: self
                    .skill
                    .source
                    .canonical_path
                    .to_string_lossy()
                    .into_owned(),
                file_extension: None,
                line_number_offset: None,
            }),
            Section::Content(ContentSection {
                label: Some("Skill instructions".into()),
                content: self.text(),
                file_extension: Some("md".into()),
                line_number_offset: None,
            }),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct LoadSkillTool {
    catalog: Arc<CallableSkills>,
    context: SandboxContext,
}

impl LoadSkillTool {
    pub const NAME: &'static str = "load_skill";

    pub fn new(catalog: Arc<CallableSkills>, context: SandboxContext) -> Self {
        Self { catalog, context }
    }

    fn selected_skill(&self, arguments: &Value) -> Result<Arc<Skill>, String> {
        let arguments: LoadSkillArgs =
            serde_json::from_value(arguments.clone()).map_err(|error| error.to_string())?;
        self.catalog
            .skills
            .get(&arguments.name)
            .map(|entry| entry.skill.clone())
            .ok_or_else(|| {
                format!(
                    "skill '{}' is not in this agent's callable catalog",
                    arguments.name
                )
            })
    }
}

impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self) -> &str {
        "Load complete instructions for an exact advertised skill name. Relative references resolve against the returned source directory. This does not load sidecars, execute scripts, or grant permissions."
    }
    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(LoadSkillArgs)
    }
    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        match self.selected_skill(arguments) {
            Ok(skill) => self
                .context
                .check_snapshot_path(&skill.source.canonical_path)
                .unwrap_or(ApprovalCheck::AutoApproved),
            Err(reason) => ApprovalCheck::AutoDenied { reason },
        }
    }
    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        arguments
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
    fn timeout_secs(&self) -> u64 {
        super::DEFAULT_TOOL_TIMEOUT_SECS
    }
    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(LoadSkillCall {
            id,
            arguments,
            tool: self.clone(),
        })
    }
}

struct LoadSkillCall {
    id: uuid::Uuid,
    arguments: Value,
    tool: LoadSkillTool,
}

impl ToolCall for LoadSkillCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        LoadSkillTool::NAME
    }
    fn arguments(&self) -> &Value {
        &self.arguments
    }
    fn timeout_secs(&self) -> u64 {
        self.tool.timeout_secs()
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, Value)> {
        self.tool.non_default_redacted_arguments(&self.arguments)
    }
    fn argument_order(&self, key: &str) -> usize {
        self.tool.argument_order(key)
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        Box::pin(async move {
            let skill = self
                .tool
                .selected_skill(&self.arguments)
                .map_err(|error| ToolError::failed(self.name(), error))?;
            if let ApprovalCheck::AutoDenied { reason } = self.tool.needs_approval(&self.arguments)
            {
                return Err(ToolError::failed(self.name(), reason));
            }
            Ok(Box::new(LoadSkillResult { skill }) as Box<dyn ToolCallResult>)
        })
    }
}
