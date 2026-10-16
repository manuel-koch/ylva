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

//! Sandbox: path-based tool approval policy.
//!
//! [`SandboxContext::check_path`] returns `None` (abstain) or an explicit
//! `Some(`[`ApprovalCheck`]`)` verdict. Tools apply their own default when
//! the sandbox abstains.
//!
//! Per-tool rule lists are configurable via a `[tools.<name>]` TOML table
//! (see [`SandboxRuleConfig`] and `docs/configuration.md`'s "Sandbox"
//! section) that is parsed by `crate::config::profile::ProfileConfig` and
//! wired into a tool's [`SandboxContext`] by
//! [`crate::agent::tools::default_registry`] via
//! [`SandboxContext::with_configured_rules`]. When a tool has no such table
//! configured at any resolved layer, [`SandboxContext`] falls back to the
//! unconditional built-in floor: `outside_workspace` → `UserApprovalRequired`
//! and `dot_path` → `UserApprovalRequired`.

mod bash_command_extractor;
pub(crate) mod network_target_pattern;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

mod command;
mod network;
mod path;
mod rules;

pub use path::{expand_path_tokens, to_relative_path};
use rules::ResolvedRule;
pub use rules::{
    ApprovalCheck, ApprovalVerdictConfig, NamedSandboxRule, SandboxRule, SandboxRuleConfig,
};

/// Shared sandbox. Holds the launch-directory anchor used to resolve relative
/// paths and discover the workspace boundary.
pub struct Sandbox {
    /// Absolute path of the sandbox root (cwd at startup).
    pub sandbox_root: PathBuf,
}

impl Sandbox {
    /// Create a new sandbox rooted at the current working directory.
    ///
    /// The sandbox root is canonicalized (symlinks resolved) so that
    /// `check_path` comparisons are consistent on platforms where temp
    /// directories live behind symlinks (e.g. macOS `/private/var/…`).
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Sandbox {
            sandbox_root: std::fs::canonicalize(&cwd).unwrap_or(cwd),
        }
    }

    /// Create a sandbox rooted at an explicit path.
    ///
    /// The path is canonicalized for the same reason as in [`Sandbox::new`].
    /// Falls back to the raw path when canonicalization fails (e.g. the
    /// directory does not yet exist).
    pub fn with_root(root: PathBuf) -> Self {
        Sandbox {
            sandbox_root: std::fs::canonicalize(&root).unwrap_or(root),
        }
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

/// Per-tool sandbox context. Carries an `Arc<RwLock<Sandbox>>` so it always
/// sees the latest sandbox state, plus a tool-specific ordered rule list.
///
/// Evaluation: normalize path (sandbox_root.join(path), collapse ".."
/// without hitting the filesystem) → walk rules in order → last match wins.
/// Path does not need to exist on disk; existing ancestors are canonicalized
/// before evaluating the workspace and hidden-path rules.
///
/// Returns `None` when no rule fires (sandbox abstains); the calling tool
/// applies its own default.
#[derive(Clone)]
pub struct SandboxContext {
    sandbox: Arc<RwLock<Sandbox>>,
    /// Ordered rules; last match wins. Always evaluated last, after either
    /// `configured_rules` or the built-in default floor — this is what
    /// `--auto-approve-write`/`--auto-approve-edit`'s catch-all rule uses so
    /// it wins regardless of any `[tools.<name>]` config.
    rules: Vec<SandboxRule>,
    /// When `Some`, fully replaces the built-in default floor (the two
    /// hardcoded `outside_workspace`/`dot_path` checks) for this tool: only
    /// this list's entries are evaluated in list order (last match wins)
    /// before `rules`. `None` means "no `[tools.<name>]` config for this
    /// tool" — the default floor applies instead. Populated from a
    /// profile's `[tools.<name>]` table via
    /// [`SandboxContext::with_configured_rules`].
    configured_rules: Option<Vec<ResolvedRule>>,
    skill_sources: Arc<Vec<PathBuf>>,
}

impl std::fmt::Debug for SandboxContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxContext")
            .field("rules_count", &self.rules.len())
            .field(
                "configured_rules_count",
                &self.configured_rules.as_ref().map(Vec::len),
            )
            .finish()
    }
}

impl SandboxContext {
    /// Callable source identities supplied only to the read tool's context.
    pub fn with_skill_sources(mut self, sources: Arc<Vec<PathBuf>>) -> Self {
        self.skill_sources = sources;
        self
    }

    /// Tool-aware defense for callers that bypass profile validation.
    pub fn with_tool_rules(self, tool_name: &str, rules: Vec<SandboxRuleConfig>) -> Self {
        if tool_name != "read" && rules.iter().any(SandboxRuleConfig::is_read_tool_only) {
            tracing::error!(
                tool = tool_name,
                "skill_sidecar rule rejected for non-read tool"
            );
            return self.with_rule(SandboxRule {
                verdict: ApprovalCheck::AutoDenied {
                    reason: "skill_sidecar is valid only for read".into(),
                },
                pattern: glob::Pattern::new("**").expect("valid glob"),
            });
        }
        self.with_configured_rules(rules)
    }

    /// Build a context with an empty rule list and no `[tools.<name>]`
    /// config, so the built-in default floor
    /// (`outside_workspace`/`dot_path` → `UserApprovalRequired`) applies.
    /// Named glob rules (if any) are appended by tool constructors.
    pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self {
        SandboxContext {
            sandbox,
            rules: vec![],
            configured_rules: None,
            skill_sources: Arc::default(),
        }
    }

    /// Append a rule to the end of this context's rule list. Last match wins,
    /// so appending a catch-all rule (e.g. `**` → `AutoApproved`) effectively
    /// overrides any earlier verdict for every path. Always evaluated after
    /// `configured_rules`/the default floor, so this continues to win
    /// regardless of `[tools.<name>]` config.
    pub fn with_rule(mut self, rule: SandboxRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Replace this tool's built-in default floor with the rule list parsed
    /// from a `[tools.<name>]` table's `rules` key. This *fully replaces* the
    /// floor for this tool (it does not merge with it): a caller that still
    /// wants `outside_workspace`/`dot_path` protection must include those as
    /// explicit [`SandboxRuleConfig::Named`] entries in `rules`.
    ///
    /// Invalid rules (a malformed glob pattern, or a `command` rule with a
    /// misplaced `*`/`**` wildcard) are skipped (with a `tracing::warn!`)
    /// rather than causing a panic or a hard config-load failure, since
    /// sandbox rules are safety-critical and a single malformed rule should
    /// not silently disable outside_workspace/dot_path protection.
    pub fn with_configured_rules(mut self, rules: Vec<SandboxRuleConfig>) -> Self {
        let resolved = rules
            .into_iter()
            .filter_map(|rule| match ResolvedRule::from_config(rule) {
                Ok(resolved) => Some(resolved),
                Err(error) => {
                    tracing::warn!(%error, "skipping invalid sandbox rule");
                    None
                }
            })
            .collect();
        self.configured_rules = Some(resolved);
        self
    }
}

#[cfg(test)]
mod tests;
