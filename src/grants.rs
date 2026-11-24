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

//! Reusable tool-approval grants.
//!
//! A [`Grant`] is a user-owned, reusable approval for *similar* future tool
//! calls. Grants only ever satisfy an ordinary `UserApprovalRequired`
//! verdict; they never override an effective denial, a schema error, a
//! disabled tool or a tool that demands human interaction. See
//! `docs/architecture-design-records/0005-keep-tool-approval-grants-separate-from-sandbox-rules.md`.
//!
//! Tools describe a call as [`GrantSubjects`]; a grant's matcher decides
//! whether it covers a [`GrantSubject`]. [`GrantState`] holds the shared
//! temporary and persistent grants and evaluates subjects against them.

mod bash_rule;
mod origin_rule;
mod path_rule;
mod proposal;
mod state;
mod store;
mod subject;

use std::path::Path;

use sha2::{Digest, Sha256};

pub use bash_rule::{
    non_grantable_reason, propose_command_rule, CommandRule, CommandRuleScope,
    MAXIMUM_PROPOSED_TOKENS,
};
pub use origin_rule::OriginRule;
pub use path_rule::PathRule;
pub use proposal::{describe_grant, grant_warnings, propose_grants};
pub use state::{GrantDecision, GrantState, SharedGrantState};
pub use store::{GrantStore, GrantStoreError, SaveOutcome, GRANTS_FILE_NAME};
pub use subject::{CommandStatement, CommandSubject, CommandToken, GrantSubject, GrantSubjects};

/// Why a stored or edited grant is not acceptable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GrantError(pub String);

impl GrantError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        GrantError(message.into())
    }
}

/// Tools a grant can be created for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GrantTool {
    Read,
    Write,
    Edit,
    Search,
    Glob,
    LoadSkill,
    Bash,
    WebFetch,
    WebSearch,
    SpawnSubagent,
}

impl GrantTool {
    pub const ALL: [GrantTool; 10] = [
        GrantTool::Read,
        GrantTool::Write,
        GrantTool::Edit,
        GrantTool::Search,
        GrantTool::Glob,
        GrantTool::LoadSkill,
        GrantTool::Bash,
        GrantTool::WebFetch,
        GrantTool::WebSearch,
        GrantTool::SpawnSubagent,
    ];

    /// The tool's canonical name, as stored in `grants.toml`.
    pub fn name(self) -> &'static str {
        match self {
            GrantTool::Read => "read",
            GrantTool::Write => "write",
            GrantTool::Edit => "edit",
            GrantTool::Search => "search",
            GrantTool::Glob => "glob",
            GrantTool::LoadSkill => "load_skill",
            GrantTool::Bash => "bash",
            GrantTool::WebFetch => "web_fetch",
            GrantTool::WebSearch => "web_search",
            GrantTool::SpawnSubagent => "spawn_subagent",
        }
    }

    pub fn from_name(name: &str) -> Option<GrantTool> {
        GrantTool::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// Whether this tool's grants judge a resolved file-system path.
    pub fn uses_path_rule(self) -> bool {
        matches!(
            self,
            GrantTool::Read
                | GrantTool::Write
                | GrantTool::Edit
                | GrantTool::Search
                | GrantTool::Glob
        )
    }

    /// Whether a grant for this tool is stored without any matcher.
    pub fn is_unconditional(self) -> bool {
        matches!(self, GrantTool::WebSearch | GrantTool::SpawnSubagent)
    }

    /// Whether a grant for this tool permits any later content change at the
    /// matched path.
    pub fn mutates_files(self) -> bool {
        matches!(self, GrantTool::Write | GrantTool::Edit)
    }
}

/// How long a grant lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantLifetime {
    /// In memory, global to the running root conversation and its subagents.
    Temporary,
    /// Stored in the workspace profile's `grants.toml`.
    Persistent,
}

impl GrantLifetime {
    pub fn label(self) -> &'static str {
        match self {
            GrantLifetime::Temporary => "temporary",
            GrantLifetime::Persistent => "persistent",
        }
    }
}

/// The matching part of a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantMatcher {
    /// `read`, `write`, `edit`, `search`, `glob`: the resolved path.
    Path(PathRule),
    /// `load_skill`: skill name and resolved instruction-source path.
    Skill { name: String, source: PathRule },
    /// `bash`: one simple command.
    Command(CommandRule),
    /// `web_fetch`: scheme, host and effective port of the initial URL.
    Origin(OriginRule),
    /// `web_search`, `spawn_subagent`: any call of the tool.
    Any,
}

/// A reusable approval for similar calls of one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub tool: GrantTool,
    pub matcher: GrantMatcher,
}

impl Grant {
    /// Build a grant from its stored attributes. Missing or surplus
    /// attributes are errors: an invalid grant must never broaden scope.
    pub fn from_stored(
        tool: GrantTool,
        name: Option<&str>,
        rule: Option<&str>,
    ) -> Result<Grant, GrantError> {
        let describe = |attribute: &str| {
            GrantError::new(format!("a '{}' grant requires '{attribute}'", tool.name()))
        };
        let surplus = |attribute: &str| {
            GrantError::new(format!(
                "a '{}' grant must not have '{attribute}'",
                tool.name()
            ))
        };
        let matcher = if tool.uses_path_rule() {
            if name.is_some() {
                return Err(surplus("name"));
            }
            GrantMatcher::Path(PathRule::parse(rule.ok_or_else(|| describe("rule"))?)?)
        } else if tool == GrantTool::LoadSkill {
            let name = name.ok_or_else(|| describe("name"))?;
            if name.trim().is_empty() || name.contains('\n') {
                return Err(GrantError::new(
                    "the skill name must be a single non-empty line",
                ));
            }
            GrantMatcher::Skill {
                name: name.to_string(),
                source: PathRule::parse(rule.ok_or_else(|| describe("rule"))?)?,
            }
        } else if tool == GrantTool::Bash {
            if name.is_some() {
                return Err(surplus("name"));
            }
            GrantMatcher::Command(CommandRule::parse(rule.ok_or_else(|| describe("rule"))?)?)
        } else if tool == GrantTool::WebFetch {
            if name.is_some() {
                return Err(surplus("name"));
            }
            GrantMatcher::Origin(OriginRule::parse(rule.ok_or_else(|| describe("rule"))?)?)
        } else {
            if name.is_some() {
                return Err(surplus("name"));
            }
            if rule.is_some() {
                return Err(surplus("rule"));
            }
            GrantMatcher::Any
        };
        Ok(Grant { tool, matcher })
    }

    /// The stored `name` attribute, if the tool has one.
    pub fn stored_name(&self) -> Option<&str> {
        match &self.matcher {
            GrantMatcher::Skill { name, .. } => Some(name),
            _ => None,
        }
    }

    /// The stored `rule` attribute, if the tool has one.
    pub fn stored_rule(&self) -> Option<String> {
        match &self.matcher {
            GrantMatcher::Path(rule) => Some(rule.text().to_string()),
            GrantMatcher::Skill { source, .. } => Some(source.text().to_string()),
            GrantMatcher::Command(rule) => Some(rule.text().to_string()),
            GrantMatcher::Origin(rule) => Some(rule.text().to_string()),
            GrantMatcher::Any => None,
        }
    }

    /// Whether this grant covers `subject`. `workspace_root` anchors
    /// workspace-relative path rules.
    pub fn covers(&self, subject: &GrantSubject, workspace_root: &Path) -> bool {
        match (&self.matcher, subject) {
            (GrantMatcher::Path(rule), GrantSubject::Path(resolved)) => {
                rule.matches(resolved, workspace_root)
            }
            (
                GrantMatcher::Skill { name, source },
                GrantSubject::Skill {
                    name: subject_name,
                    source: subject_source,
                },
            ) => name == subject_name && source.matches(subject_source, workspace_root),
            (GrantMatcher::Command(rule), GrantSubject::Command(command)) => rule.matches(command),
            (GrantMatcher::Origin(rule), GrantSubject::Origin(origin)) => rule.matches(origin),
            (GrantMatcher::Any, GrantSubject::Unconditional) => true,
            _ => false,
        }
    }

    /// A non-sensitive identifier for logs: lifetime, tool and a short digest
    /// of the stored attributes. Never contains the rule text itself.
    pub fn reference(&self, lifetime: GrantLifetime) -> GrantReference {
        let mut hasher = Sha256::new();
        hasher.update(self.tool.name().as_bytes());
        hasher.update([0]);
        hasher.update(self.stored_name().unwrap_or_default().as_bytes());
        hasher.update([0]);
        hasher.update(self.stored_rule().unwrap_or_default().as_bytes());
        let digest = crate::hexadecimal::hex_digest(&hasher.finalize());
        GrantReference(format!(
            "{}:{}:{}",
            lifetime.label(),
            self.tool.name(),
            &digest[..8]
        ))
    }
}

/// Non-sensitive grant identifier for structured logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantReference(String);

impl GrantReference {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GrantReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
