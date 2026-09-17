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
//! Per-tool rule lists are configurable via a `[tool.<name>]` TOML table
//! (see [`SandboxRuleConfig`] and `docs/configuration.md`'s "Sandbox"
//! section) that is parsed by `crate::config::profile::ProfileConfig` and
//! wired into a tool's [`SandboxContext`] by
//! [`crate::agent::tools::default_registry`] via
//! [`SandboxContext::with_configured_rules`]. When a tool has no such table
//! configured at any resolved layer, [`SandboxContext`] falls back to the
//! unconditional built-in floor: `outside_workspace` → `AutoDenied` and
//! `dot_path` → `AutoDenied`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glob::Pattern;
use serde::Deserialize;

/// The verdict from a sandbox approval check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheck {
    /// The tool call may proceed without asking the user.
    AutoApproved,
    /// The tool call must be rejected automatically; show `reason` to the user/model.
    AutoDenied { reason: String },
    /// The tool call requires explicit user approval.
    UserApprovalRequired,
}

/// A single rule in the sandbox rule list.
pub struct SandboxRule {
    /// The verdict this rule produces when matched.
    pub verdict: ApprovalCheck,
    /// Glob pattern matched against the canonicalized absolute path.
    pub pattern: Pattern,
}

/// Default reason shown when the built-in (or a configured, reason-less)
/// `outside_workspace` rule denies a path.
const OUTSIDE_WORKSPACE_REASON: &str = "Access denied by security policy: path is outside the \
                                         workspace root; only paths within the workspace are \
                                         accessible";

/// Default reason shown when the built-in (or a configured, reason-less)
/// `dot_path` rule denies a path.
const DOT_PATH_REASON: &str =
    "Access denied by security policy: dot-files and dot-directories are not accessible";

/// One of the two structural sandbox checks that ship as Rust code
/// (`SandboxContext`'s hardcoded default floor) but may also be referenced
/// by name from a `[tool.<name>]` config table's `rules` list, so a user can
/// reorder or change their verdict without reimplementing the underlying
/// path-comparison logic in glob syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedSandboxRule {
    /// Matches any path that normalizes outside the workspace root.
    OutsideWorkspace,
    /// Matches any content-tool path whose relative-to-root form has a
    /// component starting with `.` (unless `allow_dot_paths` bypasses it).
    DotPath,
}

/// The serde-friendly counterpart of [`ApprovalCheck`]'s variants, without
/// the `AutoDenied` variant's mandatory `reason` field (carried as a
/// sibling `reason` field on [`SandboxRuleConfig`] instead, so it can
/// default per-rule via [`SandboxRuleConfig::to_approval_check`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalVerdictConfig {
    AutoApproved,
    AutoDenied,
    UserApprovalRequired,
}

impl ApprovalVerdictConfig {
    /// Convert into an [`ApprovalCheck`], falling back to `default_reason`
    /// when `reason` is `None` for an `AutoDenied` verdict.
    fn to_approval_check(&self, reason: Option<&str>, default_reason: &str) -> ApprovalCheck {
        match self {
            ApprovalVerdictConfig::AutoApproved => ApprovalCheck::AutoApproved,
            ApprovalVerdictConfig::AutoDenied => ApprovalCheck::AutoDenied {
                reason: reason.unwrap_or(default_reason).to_string(),
            },
            ApprovalVerdictConfig::UserApprovalRequired => ApprovalCheck::UserApprovalRequired,
        }
    }
}

/// One entry of a `[tool.<name>]` table's `rules` list — either a reference
/// to one of the two built-in [`NamedSandboxRule`] checks (with a
/// configurable verdict) or a custom glob-pattern rule (the config-parseable
/// form of [`SandboxRule`]). Distinguished on deserialization by the
/// presence of `rule` (named) vs. `pattern` (glob). `reason` is optional
/// for `verdict = "auto_denied"` and defaults to the same message the
/// built-in floor uses for that rule (or a generic message for a custom
/// glob rule) when omitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SandboxRuleConfig {
    /// `{ rule = "outside_workspace" | "dot_path", verdict = "...", reason = "..." }`.
    Named {
        rule: NamedSandboxRule,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
    },
    /// `{ pattern = "...", verdict = "...", reason = "..." }`.
    Glob {
        pattern: String,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
    },
}

/// An entry in [`SandboxContext`]'s resolved per-tool rule list: either a
/// named structural check (re-evaluated dynamically against the current
/// `workspace_root`/path) or a fixed glob pattern. Built from
/// [`SandboxRuleConfig`] by [`SandboxContext::with_configured_rules`]; kept
/// distinct from the public [`SandboxRule`] type (which stays glob-only) so
/// that `SandboxRule`'s existing field shape — relied on by pre-existing
/// tests and by the `--auto-approve-write`/`--auto-approve-edit` catch-all
/// rules — is unaffected by this addition.
enum ResolvedRule {
    Named {
        rule: NamedSandboxRule,
        verdict: ApprovalCheck,
    },
    Glob {
        verdict: ApprovalCheck,
        pattern: Pattern,
    },
}

impl ResolvedRule {
    fn from_config(config: SandboxRuleConfig) -> Result<Self, glob::PatternError> {
        Ok(match config {
            SandboxRuleConfig::Named {
                rule,
                verdict,
                reason,
            } => {
                let default_reason = match rule {
                    NamedSandboxRule::OutsideWorkspace => OUTSIDE_WORKSPACE_REASON,
                    NamedSandboxRule::DotPath => DOT_PATH_REASON,
                };
                ResolvedRule::Named {
                    rule,
                    verdict: verdict.to_approval_check(reason.as_deref(), default_reason),
                }
            }
            SandboxRuleConfig::Glob {
                pattern,
                verdict,
                reason,
            } => {
                let default_reason = "Access denied by security policy: path matched a \
                                       configured sandbox rule";
                ResolvedRule::Glob {
                    verdict: verdict.to_approval_check(reason.as_deref(), default_reason),
                    pattern: Pattern::new(&pattern)?,
                }
            }
        })
    }
}

/// Shared workspace sandbox. Holds the workspace root used to evaluate
/// path-based rules.
pub struct Sandbox {
    /// Absolute path of the workspace root (cwd at startup).
    pub workspace_root: PathBuf,
}

impl Sandbox {
    /// Create a new sandbox rooted at the current working directory.
    ///
    /// The workspace root is canonicalized (symlinks resolved) so that
    /// `check_path` comparisons are consistent on platforms where temp
    /// directories live behind symlinks (e.g. macOS `/private/var/…`).
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Sandbox {
            workspace_root: std::fs::canonicalize(&cwd).unwrap_or(cwd),
        }
    }

    /// Create a sandbox rooted at an explicit path.
    ///
    /// The path is canonicalized for the same reason as in [`Sandbox::new`].
    /// Falls back to the raw path when canonicalization fails (e.g. the
    /// directory does not yet exist).
    pub fn with_root(root: PathBuf) -> Self {
        Sandbox {
            workspace_root: std::fs::canonicalize(&root).unwrap_or(root),
        }
    }
}

/// Collapse `path` to a form relative to `base` when `path` is absolute and
/// falls inside `base`'s directory tree; returns `None` otherwise (`path` is
/// already relative, or lies outside `base`).
///
/// Used by [`crate::agent::tools::ToolRegistry::create_call`] to rewrite
/// absolute tool-call `path`/`pattern` arguments — some LLMs emit these even
/// where a relative path would do just as well — to their cwd-relative form
/// before dispatch, so both the executed argument and the on-screen
/// tool-call rendering are short and workspace-portable. `base` is expected
/// to be the process's actual current working directory, not necessarily a
/// [`Sandbox`]'s configured `workspace_root`: relative paths in tool
/// execution (e.g. `std::fs::read`) resolve against cwd, and the two need
/// not coincide (e.g. in tests that point a sandbox at a tempdir without
/// changing the process's cwd).
///
/// Symlinked prefixes (e.g. macOS `/var` → `/private/var`) are resolved the
/// same way as [`SandboxContext::check_path`] before comparing against
/// `base`. Works for glob patterns too: wildcard segments (`**`, `*.rs`) are
/// treated as opaque path components and passed through unchanged in the
/// returned tail.
pub fn to_relative_path(path: &str, base: &Path) -> Option<String> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return None;
    }
    let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let normalized = SandboxContext::normalize(&base, candidate);
    let effective = SandboxContext::canonicalize_best_effort(&normalized);
    let relative = effective.strip_prefix(&base).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(relative.to_string_lossy().replace('\\', "/"))
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

/// Per-tool sandbox context. Carries an `Arc<RwLock<Sandbox>>` so it always
/// sees the latest sandbox state, plus a tool-specific ordered rule list.
///
/// Evaluation: normalize path (workspace_root.join(path), collapse ".."
/// without hitting the filesystem) → walk rules in order → last match wins.
/// Path does not need to exist on disk; normalization alone determines
/// workspace membership.
///
/// Returns `None` when no rule fires (sandbox abstains); the calling tool
/// applies its own default.
pub struct SandboxContext {
    sandbox: Arc<RwLock<Sandbox>>,
    /// Ordered rules; last match wins. Always evaluated last, after either
    /// `configured_rules` or the built-in default floor — this is what
    /// `--auto-approve-write`/`--auto-approve-edit`'s catch-all rule uses so
    /// it wins regardless of any `[tool.<name>]` config.
    rules: Vec<SandboxRule>,
    /// When `Some`, fully replaces the built-in default floor (the two
    /// hardcoded `outside_workspace`/`dot_path` checks) for this tool: only
    /// this list's entries are evaluated in list order (last match wins)
    /// before `rules`. `None` means "no `[tool.<name>]` config for this
    /// tool" — the default floor applies instead. Populated from a
    /// profile's `[tool.<name>]` table via
    /// [`SandboxContext::with_configured_rules`].
    configured_rules: Option<Vec<ResolvedRule>>,
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
    /// Build a context with an empty rule list and no `[tool.<name>]`
    /// config, so the built-in default floor
    /// (`outside_workspace`/`dot_path` → `AutoDenied`) applies. Named glob
    /// rules (if any) are appended by tool constructors.
    pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self {
        SandboxContext {
            sandbox,
            rules: vec![],
            configured_rules: None,
        }
    }

    /// Append a rule to the end of this context's rule list. Last match wins,
    /// so appending a catch-all rule (e.g. `**` → `AutoApproved`) effectively
    /// overrides any earlier verdict for every path. Always evaluated after
    /// `configured_rules`/the default floor, so this continues to win
    /// regardless of `[tool.<name>]` config.
    pub fn with_rule(mut self, rule: SandboxRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Replace this tool's built-in default floor with the rule list parsed
    /// from a `[tool.<name>]` table's `rules` key. This *fully replaces* the
    /// floor for this tool (it does not merge with it): a caller that still
    /// wants `outside_workspace`/`dot_path` protection must include those as
    /// explicit [`SandboxRuleConfig::Named`] entries in `rules`.
    ///
    /// Invalid glob patterns are skipped (with a `tracing::warn!`) rather
    /// than causing a panic or a hard config-load failure, since sandbox
    /// rules are safety-critical and a single malformed pattern should not
    /// silently disable outside_workspace/dot_path protection.
    pub fn with_configured_rules(mut self, rules: Vec<SandboxRuleConfig>) -> Self {
        let resolved = rules
            .into_iter()
            .filter_map(|rule| match ResolvedRule::from_config(rule) {
                Ok(resolved) => Some(resolved),
                Err(error) => {
                    tracing::warn!(%error, "skipping invalid sandbox rule glob pattern");
                    None
                }
            })
            .collect();
        self.configured_rules = Some(resolved);
        self
    }

    /// Collapse `path` to a clean absolute path using only lexical rules —
    /// no filesystem access, no symlink resolution.
    ///
    /// If `path` is relative it is joined to `base`. `..` and `.` components
    /// are then folded away. The result is suitable as a fallback for paths
    /// that do not yet exist; callers that need the real target must follow up
    /// with `std::fs::canonicalize`.
    fn normalize(base: &Path, path: &Path) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        // Collapse `..` and `.` components without `canonicalize` (which
        // requires the path to exist).
        let mut components: Vec<std::path::Component> = Vec::new();
        for component in absolute.components() {
            match component {
                std::path::Component::ParentDir => {
                    // Pop the last non-root component (never pop the root).
                    let last = components.last();
                    if matches!(last, Some(std::path::Component::Normal(_))) {
                        components.pop();
                    } else {
                        components.push(component);
                    }
                }
                std::path::Component::CurDir => {}
                other => components.push(other),
            }
        }
        components.iter().collect()
    }

    /// Return `true` when any path component starts with `.`.
    fn is_dot_path(path: &Path) -> bool {
        path.components().any(|component| {
            if let std::path::Component::Normal(os) = component {
                os.to_string_lossy().starts_with('.')
            } else {
                false
            }
        })
    }

    /// Canonicalize `path`, resolving symlinks and normalizing the form.
    ///
    /// When the full path does not exist (e.g. a write target), falls back to
    /// walking up the directory tree until a component that *does* exist is
    /// found, canonicalizing that anchor, and reattaching the non-existent
    /// tail. This ensures that a path like `/var/folders/ws/new_file.rs` is
    /// resolved to `/private/var/folders/ws/new_file.rs` on macOS even though
    /// `new_file.rs` has not been created yet — which is necessary for
    /// workspace-membership comparisons when `workspace_root` is canonical.
    /// On macOS, `/var` is a symlink to `/private/var`; canonicalizing the
    /// existing ancestor `/var/folders/ws` follows that symlink, producing
    /// the `/private/…` prefix that matches a canonical `workspace_root`.
    pub(crate) fn canonicalize_best_effort(path: &Path) -> PathBuf {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return canonical;
        }
        // Walk up to find the deepest existing ancestor.
        let mut current = path.to_path_buf();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if let Ok(canonical) = std::fs::canonicalize(&current) {
                let mut result = canonical;
                for component in tail.iter().rev() {
                    result.push(component);
                }
                return result;
            }
            match current.file_name() {
                Some(name) => {
                    tail.push(name.to_owned());
                    match current.parent() {
                        Some(parent) => current = parent.to_path_buf(),
                        None => break,
                    }
                }
                None => break,
            }
        }
        path.to_path_buf()
    }

    /// Check whether `path` is allowed by the sandbox rules.
    ///
    /// Returns `None` when no rule fires — the sandbox abstains and the calling
    /// tool applies its own default. Returns `Some(verdict)` when a rule has an
    /// explicit opinion that the tool must honour unconditionally.
    ///
    /// **Path resolution:** the input is first normalized lexically (no filesystem
    /// access), then resolved via [`canonicalize_best_effort`] which resolves
    /// symlinks for existing paths and walks up to the deepest existing ancestor
    /// for paths that do not yet exist (e.g. write targets). This ensures that
    /// workspace-membership comparisons are correct even when the input path
    /// uses a non-canonical prefix such as macOS
    /// `/var/folders` → `/private/var/folders`.
    ///
    /// Rule evaluation order:
    /// 1. When `configured_rules` is `Some` (this tool has a `[tool.<name>]`
    ///    config table), evaluate exactly that list, in order, last match
    ///    wins — this *replaces* the default floor rather than merging with
    ///    it. Otherwise evaluate the built-in default floor: outside-workspace
    ///    → `Some(AutoDenied)`, then any component of the path relative to the
    ///    workspace root starting with `.` → `Some(AutoDenied)`.
    /// 2. Named glob rules (from `self.rules`) are evaluated last; last match
    ///    wins. This is what `--auto-approve-write`/`--auto-approve-edit`'s
    ///    catch-all rule relies on to win regardless of step 1's outcome.
    /// 3. If nothing fires → `None` (sandbox abstains).
    pub fn check_path(&self, path: &Path) -> Option<ApprovalCheck> {
        self.check_path_with_dot_path_access(path, false)
    }

    /// Check whether an in-workspace enumeration may traverse `path`,
    /// allowing dot-prefixed components while retaining workspace and named
    /// rule enforcement.
    pub fn check_path_allowing_dot_paths(&self, path: &Path) -> Option<ApprovalCheck> {
        self.check_path_with_dot_path_access(path, true)
    }

    /// Returns `true` when `effective` normalizes outside `workspace_root`.
    /// Shared by the default floor and the `outside_workspace` named rule so
    /// the underlying comparison exists in exactly one place.
    fn matches_outside_workspace(workspace_root: &Path, effective: &Path) -> bool {
        !effective.starts_with(workspace_root)
    }

    /// Returns `true` when the portion of `effective` relative to
    /// `workspace_root` has a dot-prefixed component, unless `allow_dot_paths`
    /// bypasses the check (used by `glob` enumeration). A path outside the
    /// workspace (whose `strip_prefix` fails) never matches here — the
    /// `outside_workspace` rule is responsible for that case. Shared by the
    /// default floor and the `dot_path` named rule so the underlying
    /// comparison exists in exactly one place.
    fn matches_dot_path(workspace_root: &Path, effective: &Path, allow_dot_paths: bool) -> bool {
        if allow_dot_paths {
            return false;
        }
        match effective.strip_prefix(workspace_root) {
            Ok(relative) => Self::is_dot_path(relative),
            Err(_) => false,
        }
    }

    /// Evaluate one [`NamedSandboxRule`] against `workspace_root`/`effective`,
    /// returning whether it matches.
    fn evaluate_named_rule(
        rule: NamedSandboxRule,
        workspace_root: &Path,
        effective: &Path,
        allow_dot_paths: bool,
    ) -> bool {
        match rule {
            NamedSandboxRule::OutsideWorkspace => {
                Self::matches_outside_workspace(workspace_root, effective)
            }
            NamedSandboxRule::DotPath => {
                Self::matches_dot_path(workspace_root, effective, allow_dot_paths)
            }
        }
    }

    fn check_path_with_dot_path_access(
        &self,
        path: &Path,
        allow_dot_paths: bool,
    ) -> Option<ApprovalCheck> {
        let workspace_root = {
            let guard = self.sandbox.read().expect("sandbox lock poisoned");
            guard.workspace_root.clone()
        };
        // Ensure workspace_root is canonical even when the Sandbox was built via
        // direct struct construction rather than Sandbox::new() / Sandbox::with_root().
        // This guards against macOS /var/folders → /private/var/folders mismatches.
        let workspace_root = std::fs::canonicalize(&workspace_root).unwrap_or(workspace_root);
        let normalized = Self::normalize(&workspace_root, path);

        // Resolve symlinks and non-canonical prefixes (e.g. macOS /var → /private/var).
        let effective = Self::canonicalize_best_effort(&normalized);
        let path_str = effective.to_string_lossy();

        // No implicit zeroth verdict — absence of a match returns None.
        let mut verdict: Option<ApprovalCheck> = None;

        match &self.configured_rules {
            Some(configured) => {
                // `[tool.<name>]` config fully replaces the default floor:
                // evaluate exactly this list, in order, last match wins.
                for rule in configured {
                    match rule {
                        ResolvedRule::Named {
                            rule: named,
                            verdict: rule_verdict,
                        } => {
                            if Self::evaluate_named_rule(
                                *named,
                                &workspace_root,
                                &effective,
                                allow_dot_paths,
                            ) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        ResolvedRule::Glob {
                            verdict: rule_verdict,
                            pattern,
                        } => {
                            if pattern.matches(&path_str) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                    }
                }
            }
            None => {
                // Built-in default floor: outside_workspace, then dot_path.
                if Self::evaluate_named_rule(
                    NamedSandboxRule::OutsideWorkspace,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::AutoDenied {
                        reason: OUTSIDE_WORKSPACE_REASON.to_string(),
                    });
                }
                if Self::evaluate_named_rule(
                    NamedSandboxRule::DotPath,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::AutoDenied {
                        reason: DOT_PATH_REASON.to_string(),
                    });
                }
            }
        }

        // Named glob rules (appended by tool constructors, e.g.
        // --auto-approve-write/--auto-approve-edit's catch-all). Always
        // evaluated last so they win regardless of configured_rules/the
        // default floor.
        for rule in &self.rules {
            if rule.pattern.matches(&path_str) {
                verdict = Some(rule.verdict.clone());
            }
        }

        tracing::debug!(
            path = %effective.display(),
            sandbox_verdict = match &verdict {
                None => "abstain".to_string(),
                Some(ApprovalCheck::AutoApproved) => "auto-approved".to_string(),
                Some(ApprovalCheck::AutoDenied { reason }) => format!("auto-denied: {reason}"),
                Some(ApprovalCheck::UserApprovalRequired) => "user-approval-required".to_string(),
            },
            "sandbox check_path"
        );

        verdict
    }

    /// Check whether `command` is allowed by the sandbox.
    ///
    /// Returns `None` when no rule fires — the sandbox abstains and the calling
    /// tool applies its own default (typically `UserApprovalRequired` for shell
    /// commands). Returns `Some(verdict)` when a rule has an explicit opinion.
    ///
    /// Currently the sandbox has no command-specific rules and always abstains.
    /// Future configuration (e.g. per-tool TOML allow/deny command globs) will
    /// be evaluated here without changing call sites.
    pub fn check_command(&self, _command: &str) -> Option<ApprovalCheck> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sandbox(root: &Path) -> Arc<RwLock<Sandbox>> {
        Arc::new(RwLock::new(Sandbox::with_root(root.to_path_buf())))
    }

    #[test]
    fn check_path_outside_workspace_is_denied() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let outside = temporary_dir.path().join("other/file.rs");
        assert!(
            matches!(
                context.check_path(&outside),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "path outside workspace should be Some(AutoDenied)"
        );
    }

    #[test]
    fn check_path_dot_path_is_denied() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let dot_path = workspace.join(".env");
        assert!(
            matches!(
                context.check_path(&dot_path),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "dot-path relative to workspace should be Some(AutoDenied)"
        );
        let dot_dir_path = workspace.join(".git/config");
        assert!(
            matches!(
                context.check_path(&dot_dir_path),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "dot-directory relative to workspace should be Some(AutoDenied)"
        );
    }

    #[test]
    fn check_path_allowing_dot_paths_abstains_inside_workspace() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let dot_dir_path = workspace.join(".git/config");
        assert_eq!(
            context.check_path_allowing_dot_paths(&dot_dir_path),
            None,
            "dot-directory enumeration inside the workspace should abstain"
        );
    }

    #[test]
    fn check_path_normal_inside_workspace_returns_none() {
        // No rule fires for a plain in-workspace path: sandbox abstains.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let path = workspace.join("src/main.rs");
        assert_eq!(
            context.check_path(&path),
            None,
            "normal workspace path: sandbox abstains (None)"
        );
    }

    #[test]
    fn normalize_collapses_dot_dot_components() {
        let base = Path::new("/workspace");
        let path = Path::new("src/../README.md");
        let normalized = SandboxContext::normalize(base, path);
        assert_eq!(normalized, PathBuf::from("/workspace/README.md"));
    }

    #[test]
    fn normalize_collapses_current_dir_component() {
        let base = Path::new("/workspace");
        let path = Path::new("./src/./main.rs");
        let normalized = SandboxContext::normalize(base, path);
        assert_eq!(normalized, PathBuf::from("/workspace/src/main.rs"));
    }

    #[test]
    fn sandbox_context_debug_shows_rules_count() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let debug_str = format!("{:?}", context);
        assert!(debug_str.contains("rules_count"));
    }

    #[test]
    fn sandbox_default_uses_current_dir() {
        // Acquires SUBSCRIBER_LOCK to prevent concurrent execution with
        // `init_logging_relative_filename_uses_current_directory`, which
        // temporarily changes the process-global cwd while holding the same
        // lock.  Without this guard the two tests race: one reads cwd during
        // the other's cwd mutation, causing a spurious mismatch.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sandbox = Sandbox::default();
        let cwd = std::env::current_dir().expect("cwd");
        let canonical_cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        assert_eq!(sandbox.workspace_root, canonical_cwd);
    }

    #[test]
    fn normalize_does_not_pop_past_root() {
        // A `..` that cannot pop (root) is left in place.
        let base = Path::new("/");
        let path = Path::new("/../file.txt");
        let normalized = SandboxContext::normalize(base, path);
        // The `..` at root is a no-op; normalized stays at /../file.txt level.
        assert!(normalized.ends_with("file.txt"));
    }

    #[test]
    fn check_path_glob_rule_overrides_to_explicit_verdict() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        // Build the glob pattern from the canonical workspace_root stored in
        // the sandbox, not from the raw `workspace` variable, so that the
        // pattern matches the canonical `effective` path inside check_path.
        let canonical_workspace = sandbox.read().unwrap().workspace_root.clone();
        let allow_pattern = glob::Pattern::new(&format!(
            "{}/allowed/**",
            canonical_workspace.to_string_lossy()
        ))
        .unwrap();
        let mut context = SandboxContext::new(Arc::clone(&sandbox));
        context.rules.push(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: allow_pattern,
        });

        // Glob rule fires → Some(AutoApproved).
        let allowed_path = workspace.join("allowed/file.txt");
        assert_eq!(
            context.check_path(&allowed_path),
            Some(ApprovalCheck::AutoApproved),
            "glob rule should produce Some(AutoApproved)"
        );

        // No rule fires for this path → sandbox abstains.
        let normal_path = workspace.join("src/main.rs");
        assert_eq!(
            context.check_path(&normal_path),
            None,
            "unmatched path: sandbox abstains (None)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_workspace_is_denied() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = temporary_dir.path().join("secret.txt");
        std::fs::write(&outside, "sensitive").unwrap();

        // A workspace-relative symlink whose target lives outside the workspace.
        let link = workspace.join("innocent.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        assert!(
            matches!(
                context.check_path(&link),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "symlink escaping the workspace must be AutoDenied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_dot_file_inside_workspace_is_denied() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let real_dot_file = workspace.join(".env");
        std::fs::write(&real_dot_file, "SECRET=x").unwrap();

        // A non-dot symlink name pointing at a dot-file inside the workspace.
        let link = workspace.join("config.txt");
        std::os::unix::fs::symlink(&real_dot_file, &link).unwrap();

        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        assert!(
            matches!(
                context.check_path(&link),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "symlink whose real target is a dot-file must be AutoDenied"
        );
    }

    #[test]
    fn check_command_always_abstains() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(
            context.check_command("echo hello"),
            None,
            "sandbox abstains for ordinary commands"
        );
        assert_eq!(
            context.check_command("rm -rf /"),
            None,
            "sandbox abstains even for dangerous-looking commands (no rules configured)"
        );
        assert_eq!(
            context.check_command(""),
            None,
            "sandbox abstains for empty command"
        );
    }

    #[test]
    fn to_relative_path_rewrites_absolute_path_inside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        let absolute = base.join("src/main.rs");

        assert_eq!(
            to_relative_path(absolute.to_str().unwrap(), &base),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn to_relative_path_rewrites_absolute_glob_pattern_inside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        let pattern = format!("{}/src/**/*.rs", base.display());

        assert_eq!(
            to_relative_path(&pattern, &base),
            Some("src/**/*.rs".to_string())
        );
    }

    #[test]
    fn to_relative_path_returns_none_for_relative_path() {
        let temporary_dir = tempfile::tempdir().unwrap();
        assert_eq!(to_relative_path("src/main.rs", temporary_dir.path()), None);
    }

    #[test]
    fn to_relative_path_returns_none_for_path_outside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = temporary_dir.path().join("workspace");
        std::fs::create_dir_all(&base).unwrap();
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(to_relative_path(outside.to_str().unwrap(), &base), None);
    }

    #[test]
    fn to_relative_path_returns_none_for_base_itself() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        assert_eq!(to_relative_path(base.to_str().unwrap(), &base), None);
    }

    #[test]
    fn sandbox_rule_config_named_variant_parses_from_toml() {
        let parsed: SandboxRuleConfig = toml::from_str(
            r#"
            rule = "dot_path"
            verdict = "user_approval_required"
            "#,
        )
        .expect("named rule config parses");
        assert!(matches!(
            parsed,
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                ..
            }
        ));
    }

    #[test]
    fn sandbox_rule_config_glob_variant_parses_from_toml() {
        let parsed: SandboxRuleConfig = toml::from_str(
            r#"
            pattern = "**/*.secret"
            verdict = "auto_denied"
            reason = "no secrets"
            "#,
        )
        .expect("glob rule config parses");
        match parsed {
            SandboxRuleConfig::Glob {
                pattern,
                verdict,
                reason,
            } => {
                assert_eq!(pattern, "**/*.secret");
                assert_eq!(
                    verdict.to_approval_check(reason.as_deref(), "fallback"),
                    ApprovalCheck::AutoDenied {
                        reason: "no secrets".to_string()
                    }
                );
            }
            other => panic!("expected Glob variant, got {other:?}"),
        }
    }

    #[test]
    fn approval_verdict_config_auto_denied_defaults_reason_when_omitted() {
        let parsed = ApprovalVerdictConfig::AutoDenied;
        assert_eq!(
            parsed.to_approval_check(None, "default reason"),
            ApprovalCheck::AutoDenied {
                reason: "default reason".to_string()
            }
        );
    }

    #[test]
    fn configured_named_rule_overrides_default_dot_path_verdict() {
        // A `[tool.read]`-style config changing `dot_path`'s verdict to
        // `UserApprovalRequired` for this tool only; `outside_workspace`
        // protection is intentionally NOT included, confirming the
        // full-replace (not merge) semantics.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::UserApprovalRequired,
                reason: None,
            },
        ]);

        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::UserApprovalRequired),
            "configured dot_path rule should override the default AutoDenied verdict"
        );

        // outside_workspace is not part of this tool's configured list, so it
        // no longer fires (full replace, not merge) — sandbox abstains.
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(
            context.check_path(&outside),
            None,
            "outside_workspace should not fire when omitted from configured_rules"
        );
    }

    #[test]
    fn unconfigured_tool_falls_back_to_default_floor() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        let sandbox = make_sandbox(&workspace);
        // No with_configured_rules call: default floor applies to every tool.
        let context = SandboxContext::new(Arc::clone(&sandbox));

        let dot_path = workspace.join(".env");
        assert!(matches!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
        let outside = temporary_dir.path().join("other/file.rs");
        assert!(matches!(
            context.check_path(&outside),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
    }

    #[test]
    fn configured_rules_mix_named_and_glob_last_match_wins() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let canonical_workspace = sandbox.read().unwrap().workspace_root.clone();
        let allow_pattern = format!("{}/allowed/**", canonical_workspace.to_string_lossy());

        // List order: dot_path (AutoDenied) then a glob rule that re-approves
        // one specific dot-path — last match wins, so the glob rule (later in
        // the list) should override the earlier named rule for matching paths.
        let dot_allow_pattern =
            format!("{}/.allowed_dot/**", canonical_workspace.to_string_lossy());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
            },
            SandboxRuleConfig::Glob {
                pattern: allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
            SandboxRuleConfig::Glob {
                pattern: dot_allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        // Regular dot-path still denied (only the named rule matches it).
        let dot_path = workspace.join(".env");
        assert!(matches!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));

        // Dot-path under .allowed_dot: named dot_path rule matches (deny),
        // then the later glob rule also matches and overrides to approved —
        // proving last-match-wins across Named and Glob in one list.
        let allowed_dot_path = workspace.join(".allowed_dot/file.txt");
        assert_eq!(
            context.check_path(&allowed_dot_path),
            Some(ApprovalCheck::AutoApproved)
        );

        // Non-dot path under allowed/: only the glob rule matches.
        let allowed_path = workspace.join("allowed/file.txt");
        assert_eq!(
            context.check_path(&allowed_path),
            Some(ApprovalCheck::AutoApproved)
        );
    }

    #[test]
    fn auto_approve_catch_all_wins_over_configured_deny_rule() {
        // Mirrors --auto-approve-write/--auto-approve-edit: the catch-all
        // `with_rule` is appended after `with_configured_rules` and must win
        // even though the configured list would otherwise deny the path.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
            }])
            .with_rule(SandboxRule {
                verdict: ApprovalCheck::AutoApproved,
                pattern: glob::Pattern::new("**").unwrap(),
            });

        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoApproved),
            "catch-all auto-approve rule must win over a configured deny rule"
        );
    }
}
