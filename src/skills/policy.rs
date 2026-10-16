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

//! Per-agent callable catalogs, resolved from the immutable discovery snapshot.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::config::ProfileConfig;
use crate::sandbox::{ApprovalCheck, Sandbox, SandboxContext};

use super::{Skill, SkillCatalog};

#[derive(Debug, Clone)]
pub struct CallableSkill {
    pub skill: Arc<Skill>,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CallableSkills {
    pub skills: BTreeMap<String, CallableSkill>,
}

impl CallableSkills {
    pub fn resolve(
        catalog: &SkillCatalog,
        profile: &ProfileConfig,
        agent_type: &str,
        allowed_tools: &[String],
        interactive: bool,
        sandbox: Arc<RwLock<Sandbox>>,
    ) -> anyhow::Result<Self> {
        let agent = profile.agents.get(agent_type);
        let explicit_names = agent.and_then(|agent| agent.allowed_skills.as_ref());
        if let Some(names) = explicit_names {
            for name in names {
                if !catalog.skills.contains_key(name) {
                    let rejections = catalog
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.name.as_ref() == Some(name))
                        .map(|diagnostic| {
                            format!("{}: {}", diagnostic.path.display(), diagnostic.reason)
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    anyhow::bail!(
                        "agent type '{agent_type}' references unknown skill '{name}'{}",
                        if rejections.is_empty() {
                            String::new()
                        } else {
                            format!(" ({rejections})")
                        }
                    );
                }
            }
        }
        let loader_enabled = agent
            .and_then(|agent| agent.allowed_tools.as_deref())
            .unwrap_or(allowed_tools)
            .iter()
            .any(|name| name == "load_skill");
        anyhow::ensure!(
            loader_enabled || explicit_names.is_none_or(Vec::is_empty),
            "agent type '{agent_type}' explicitly selects skills but allowed_tools disables load_skill"
        );
        if !loader_enabled {
            return Ok(Self::default());
        }
        let context = loader_context(sandbox, profile, agent_type);
        let mut callable = Self::default();
        for (name, skill) in &catalog.skills {
            if explicit_names.is_some_and(|names| !names.contains(name)) {
                continue;
            }
            let approval = context
                .check_snapshot_path(&skill.source.canonical_path)
                .unwrap_or(ApprovalCheck::AutoApproved);
            let unavailable = match &approval {
                ApprovalCheck::AutoDenied { reason } => Some(reason.as_str()),
                ApprovalCheck::UserApprovalRequired if !interactive => {
                    Some("requires interactive approval in headless mode")
                }
                _ => None,
            };
            if let Some(reason) = unavailable {
                anyhow::ensure!(
                    explicit_names.is_none(),
                    "agent type '{agent_type}' cannot load explicitly selected skill '{name}': {reason}"
                );
                continue;
            }
            callable.skills.insert(
                name.clone(),
                CallableSkill {
                    skill: skill.clone(),
                    requires_approval: approval == ApprovalCheck::UserApprovalRequired,
                },
            );
        }
        Ok(callable)
    }

    pub fn advertisement(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut prompt = String::from(
            "Available skills (name and description only). Load relevant instructions with load_skill before following them. Loading does not grant sidecar or execution permissions.\n",
        );
        for (name, entry) in &self.skills {
            // JSON quoting makes metadata boundaries and embedded newlines explicit.
            prompt.push_str(&format!(
                "\n- {}: {}{}",
                serde_json::to_string(name).expect("string serializes"),
                serde_json::to_string(&super::terminal_safe(&entry.skill.description))
                    .expect("string serializes"),
                if entry.requires_approval {
                    " [requires user approval]"
                } else {
                    ""
                },
            ));
        }
        Some(prompt)
    }
}

pub fn loader_context(
    sandbox: Arc<RwLock<Sandbox>>,
    profile: &ProfileConfig,
    agent_type: &str,
) -> SandboxContext {
    let rules = profile
        .agents
        .get(agent_type)
        .and_then(|agent| agent.tools.get("load_skill"))
        .or_else(|| profile.tools.get("load_skill"))
        .map(|config| config.rules.clone())
        .unwrap_or_default();
    SandboxContext::new(sandbox).with_tool_rules("load_skill", rules)
}
