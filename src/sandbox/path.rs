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

//! Path expansion, canonicalization, device checks and path approval.

use super::rules::ResolvedRule;
use super::{ApprovalCheck, NamedSandboxRule, SandboxContext};
use std::path::{Path, PathBuf};

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
/// [`super::Sandbox`]'s configured `workspace_root`: relative paths in tool
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

/// Expand the path tokens a model may quote verbatim in a path-like tool
/// argument: `$NAME`/`${NAME}` environment variables and a leading `~`.
///
/// This is plain string substitution performed in Rust — never a shell — and
/// it runs **before** [`to_relative_path`] and before any sandbox approval
/// check, so the sandbox sees only the expanded, canonicalized path. It never
/// bypasses approval: an expanded path outside the workspace still hits the
/// `outside_workspace` approval floor.
///
/// Rules:
///
/// - `$NAME` and `${NAME}` tokens (`NAME` = `[A-Za-z_][A-Za-z0-9_]*`) are
///   replaced by `lookup(NAME)` when it is defined, and left literal when it
///   is not — a literal `$` in a file name survives.
/// - Substitution runs once: substituted values are never rescanned for
///   further tokens.
/// - After environment substitution, a leading `~` or `~/` expands through
///   `lookup("HOME")`. `~user/…` is deliberately unsupported and stays
///   untouched.
/// - The `lookup` parameter keeps this helper free of process-environment
///   access, so unit tests never mutate the process environment.
pub fn expand_path_tokens(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    expand_leading_tilde(&substitute_environment_variables(value, lookup), lookup)
}

/// Replace every well-formed `$NAME`/`${NAME}` token in `value` exactly once.
///
/// The output is built only from untouched input characters and resolved
/// values, so a replacement value containing `$OTHER` is emitted verbatim.
fn substitute_environment_variables(
    value: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> String {
    let bytes = value.as_bytes();
    let mut expanded = String::with_capacity(value.len());
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] != b'$' {
            let character = value[position..]
                .chars()
                .next()
                .expect("every position starts a character");
            expanded.push(character);
            position += character.len_utf8();
            continue;
        }
        match read_variable_reference(bytes, position) {
            Some((name, length)) => {
                match lookup(name) {
                    Some(replacement) => expanded.push_str(&replacement),
                    None => expanded.push_str(&value[position..position + length]),
                }
                position += length;
            }
            None => {
                expanded.push('$');
                position += 1;
            }
        }
    }
    expanded
}

/// Read a `$NAME` or `${NAME}` reference starting at `position` (the index
/// of the `$`), returning the variable name and the number of bytes the
/// reference occupies from `position`.
///
/// Returns `None` when no valid reference starts there — an empty name, a
/// name that does not start with `[A-Za-z_]`, an unterminated `${…`, or a
/// bare `$` — so the caller leaves that `$` literal.
fn read_variable_reference(bytes: &[u8], position: usize) -> Option<(&str, usize)> {
    let (name_start, name_end, consumed_end) = if bytes.get(position + 1) == Some(&b'{') {
        let name_start = position + 2;
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start
            || !is_variable_name_start(bytes[name_start])
            || bytes.get(name_end) != Some(&b'}')
        {
            return None;
        }
        (name_start, name_end, name_end + 1)
    } else {
        let name_start = position + 1;
        if name_start >= bytes.len() || !is_variable_name_start(bytes[name_start]) {
            return None;
        }
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        (name_start, name_end, name_end)
    };
    let name = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;
    Some((name, consumed_end - position))
}

/// First byte of a variable name: `[A-Za-z_]`.
fn is_variable_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Any byte of a variable name: `[A-Za-z0-9_]`.
fn is_variable_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Expand a leading `~` or `~/` prefix through `lookup("HOME")`.
///
/// `~user/…` is deliberately unsupported: expanding it would require parsing
/// the operating system's user database for no benefit, so the value stays
/// untouched.
fn expand_leading_tilde(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(rest) = value.strip_prefix('~') else {
        return value.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return value.to_string();
    }
    match lookup("HOME") {
        Some(home) => format!("{home}{rest}"),
        None => value.to_string(),
    }
}

/// Resolve the filesystem device ID of `path` by stat-ing it (following
/// symlinks), returning the kernel's opaque numeric `st_dev` value.
///
/// Returns `None` when the path's metadata is unavailable, and always `None`
/// on non-unix targets, so [`is_cross_device`] never reports a difference
/// there (the `CrossFilesystem` rule parses on every platform but never
/// matches off-unix). Device IDs are only ever compared for equality, never
/// interpreted.
fn resolve_device(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|metadata| metadata.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Pure comparison for the `cross_filesystem` rule: `true` only when both
/// device IDs are known and differ. Any `None` on either side — metadata
/// unavailable for the candidate anchor or the workspace root — means "no
/// positively established difference" and yields `false` (the rule does not
/// match; `outside_workspace`/`dot_path` remain the boundary guards).
fn is_cross_device(candidate: Option<u64>, workspace_root: Option<u64>) -> bool {
    matches!(
        (candidate, workspace_root),
        (Some(candidate), Some(workspace_root)) if candidate != workspace_root
    )
}

/// Canonicalized deepest existing ancestor of `path` — the device-comparison
/// anchor used by [`resolve_device`] when `path` itself does not exist yet
/// (e.g. a write target or a not-yet-created read path). Sibling of
/// [`SandboxContext::canonicalize_best_effort`]: that function reattaches the
/// missing tail to the canonicalized anchor, while this one returns only the
/// anchor. Falls back to `path` unchanged when no ancestor can be
/// canonicalized.
fn canonicalize_existing_anchor(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            return canonical;
        }
        match current.file_name() {
            Some(_) => match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => break,
            },
            None => break,
        }
    }
    path.to_path_buf()
}

impl SandboxContext {
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
    /// 1. When `configured_rules` is `Some` (this tool has a `[tools.<name>]`
    ///    config table), evaluate exactly that list, in order, last match
    ///    wins — this *replaces* the default floor rather than merging with
    ///    it. Otherwise evaluate the built-in default floor: outside-workspace
    ///    → `Some(UserApprovalRequired)`, then any component of the path
    ///    relative to the workspace root starting with `.` →
    ///    `Some(UserApprovalRequired)`.
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

    /// Evaluate one of the three path-based [`NamedSandboxRule`] variants
    /// against `workspace_root`/`effective`. The four `bash`-only variants
    /// are never valid under a path tool (rejected by config validation);
    /// defensively `false` here if one slips through.
    ///
    /// `CrossFilesystem` resolves both device IDs fresh on every evaluation
    /// (workspace root: the defensively re-canonicalized `workspace_root`;
    /// candidate: [`resolve_device`] of [`canonicalize_existing_anchor`] of
    /// `effective`) — never cached, so a mid-session remount cannot leave a
    /// stale ID behind.
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
            NamedSandboxRule::CrossFilesystem => is_cross_device(
                resolve_device(&canonicalize_existing_anchor(effective)),
                resolve_device(workspace_root),
            ),
            NamedSandboxRule::Backgrounded
            | NamedSandboxRule::VariableAssignmentPrefix
            | NamedSandboxRule::UnquotedGlob
            | NamedSandboxRule::UnquotedBraceExpansion
            | NamedSandboxRule::NetworkRequest => false,
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
                // `[tools.<name>]` config fully replaces the default floor:
                // evaluate exactly this list, in order, last match wins.
                for rule in configured {
                    match rule {
                        ResolvedRule::Named {
                            rule: named,
                            verdict: rule_verdict,
                            target: _,
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
                        ResolvedRule::PathGlob {
                            pattern,
                            verdict: rule_verdict,
                        } => {
                            if pattern.matches(&path_str) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        // A `bash`-only `CommandGlob` rule under a path tool
                        // is rejected by config validation; never matches a
                        // path defensively if it slips through.
                        ResolvedRule::CommandGlob { .. } => {}
                    }
                }
            }
            None => {
                // Built-in default floor: outside_workspace, then dot_path.
                // Both boundary checks ask the user instead of rejecting the
                // call outright, so a legitimate out-of-workspace read (or a
                // dot-file the model was pointed at) can be granted; the
                // `OUTSIDE_WORKSPACE_REASON`/`DOT_PATH_REASON` texts remain
                // the messages for a *configured* `auto_denied` named rule.
                if Self::evaluate_named_rule(
                    NamedSandboxRule::OutsideWorkspace,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::UserApprovalRequired);
                }
                if Self::evaluate_named_rule(
                    NamedSandboxRule::DotPath,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::UserApprovalRequired);
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
}

#[cfg(test)]
mod tests;
