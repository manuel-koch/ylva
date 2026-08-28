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
//! the sandbox abstains. TOML per-tool config loading is not yet implemented.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glob::Pattern;

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

/// Shared workspace sandbox. Holds the workspace root used to evaluate
/// path-based rules. Per-tool TOML config is not yet implemented.
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
    /// Ordered rules; last match wins.
    rules: Vec<SandboxRule>,
}

impl std::fmt::Debug for SandboxContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxContext")
            .field("rules_count", &self.rules.len())
            .finish()
    }
}

impl SandboxContext {
    /// Build a context with an empty rule list.
    /// Named glob rules (if any) are appended by tool constructors.
    pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self {
        SandboxContext {
            sandbox,
            rules: vec![],
        }
    }

    /// Append a rule to the end of this context's rule list. Last match wins,
    /// so appending a catch-all rule (e.g. `**` → `AutoApproved`) effectively
    /// overrides any earlier verdict for every path.
    pub fn with_rule(mut self, rule: SandboxRule) -> Self {
        self.rules.push(rule);
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
    fn canonicalize_best_effort(path: &Path) -> PathBuf {
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
    /// 1. Outside-workspace → `Some(AutoDenied)`.
    /// 2. Any component of the path relative to the workspace root starts with `.` → `Some(AutoDenied)`.
    /// 3. Named glob rules (from `self.rules`) are evaluated last; last match wins.
    /// 4. If nothing fires → `None` (sandbox abstains).
    pub fn check_path(&self, path: &Path) -> Option<ApprovalCheck> {
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

        // No implicit zeroth verdict — absence of a match returns None.
        let mut verdict: Option<ApprovalCheck> = None;

        // Rule: outside workspace → AutoDenied.
        // TODO: make this configurable via named rule `path_outside_workspace`.
        if !effective.starts_with(&workspace_root) {
            verdict = Some(ApprovalCheck::AutoDenied {
                reason: "Access denied by security policy: path is outside the workspace root; \
                         only paths within the workspace are accessible"
                    .to_string(),
            });
        } else {
            // Rule: any component of the relative portion starts with `.` → AutoDenied.
            // Strip the workspace prefix so only the tool-navigated components are
            // examined, not system directories embedded in the workspace path itself.
            // TODO: make this configurable via named rule `dot_path`.
            if let Ok(relative) = effective.strip_prefix(&workspace_root) {
                if Self::is_dot_path(relative) {
                    verdict = Some(ApprovalCheck::AutoDenied {
                        reason: "Access denied by security policy: \
                                 dot-files and dot-directories are not accessible"
                            .to_string(),
                    });
                }
            }
        }

        // Named glob rules (appended by tool constructors).
        for rule in &self.rules {
            let path_str = effective.to_string_lossy();
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
}
