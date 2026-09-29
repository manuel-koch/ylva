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

//! `glob` tool: enumerate filesystem paths matching one glob pattern.
//!
//! Pattern validation and traversal stay within this module; execution is
//! bounded upstream by [`crate::agent::DEFAULT_TOOL_TIMEOUT_SECS`]. The
//! `git_ignore=true` path walks with `ignore::WalkBuilder`, while
//! `git_ignore=false` uses the `glob` crate directly.
//!
//! **`timeout_secs` sentinel (Q6):** the tool carries a `timeout_secs: u64`
//! override where `0` selects [`DEFAULT_TOOL_TIMEOUT_SECS`]. A timeout is always
//! enforced — there is no "no-timeout" value; `0` merely selects the module
//! default.

use std::path::{Path, PathBuf};

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use super::{
    ContentSection, Section, Tool, ToolCall, ToolCallResult, ToolError, DEFAULT_TOOL_TIMEOUT_SECS,
};
use crate::sandbox::{ApprovalCheck, SandboxContext};

fn require_string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ApprovalCheck> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(ApprovalCheck::AutoDenied {
            reason: format!("Required argument '{key}' must not be empty"),
        }),
        None => Err(ApprovalCheck::AutoDenied {
            reason: format!("Required argument '{key}' is missing or not a string"),
        }),
    }
}

fn default_true() -> bool {
    true
}

/// `glob` arguments: a glob pattern resolved against the current directory.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct GlobInputArgs {
    /// Search for paths matching this single glob pattern beneath the supplied
    /// search root. The search root itself is never a match and is always
    /// excluded from returned paths and all counts, regardless of the pattern
    /// or option flags. Only descendants of the search root are eligible.
    ///
    /// `*` matches within one segment; `**` matches directory components; `?`
    /// matches one char; `[abc]` matches a class. Use `**/*` rather than bare
    /// `**` to match both files and directories recursively. Comma lists
    /// (`*.rs,*.toml`) and brace alternation (`*.{rs,toml}`) are **not**
    /// supported — treated as one literal pattern, matching nothing. Call
    /// once per pattern to match several. Patterns are resolved relative to
    /// cwd unless absolute; to search beneath a sub-directory, include that
    /// directory in the pattern (for example, `docs/**/*.md`). There is no
    /// separate `path` argument.
    pub pattern: String,
    /// Exclude paths matched by `.gitignore` rules (default `true`). This does
    /// not exclude the `.git` directory or other dot-prefixed paths; set
    /// `dot_ignore` to `true` to exclude them.
    #[serde(default = "default_true")]
    pub git_ignore: bool,
    /// Return no paths. The total remains available as `total` in the
    /// structured result metadata.
    #[serde(default)]
    pub only_count: bool,
    /// Restrict to regular files (excludes dirs/symlinks). Mutually exclusive
    /// with `only_dirs` — both `true` is an error. Omit both for all path types.
    #[serde(default)]
    pub only_files: bool,
    /// Restrict to directories (excludes files/symlinks). Mutually exclusive
    /// with `only_files` — both `true` is an error. Omit both for all path types.
    #[serde(default)]
    pub only_dirs: bool,
    /// Max paths to return. Omit this argument for no result-count limit; do
    /// not use an arbitrary large value to request unlimited results. When
    /// additional paths are omitted, result metadata contains `truncated: true`.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Exclude dot-prefixed paths (default `false` — hidden files included,
    /// matching `find . -type f` behaviour).
    #[serde(default)]
    pub dot_ignore: bool,
}

/// Counts matching descendants by path type.
///
/// `total` is always the sum of `files` and `directories`. The search root is
/// never included in any of these values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GlobMatchCounts {
    /// Number of matching file paths.
    pub files: usize,
    /// Number of matching directory paths.
    pub directories: usize,
    /// Sum of `files` and `directories`.
    pub total: usize,
}

impl GlobMatchCounts {
    fn add_path_kind(&mut self, path_kind: GlobPathKind) {
        match path_kind {
            GlobPathKind::File => self.files += 1,
            GlobPathKind::Directory => self.directories += 1,
        }
        self.total += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobPathKind {
    File,
    Directory,
}

#[derive(Debug)]
struct GlobCandidate {
    path: PathBuf,
    path_kind: GlobPathKind,
}

impl GlobCandidate {
    fn from_path(path: PathBuf) -> Self {
        let path_kind = if std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
        {
            GlobPathKind::Directory
        } else {
            GlobPathKind::File
        };
        Self { path, path_kind }
    }
}

/// Structured metadata for a successful `glob` tool invocation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GlobResultMetadata {
    /// Number of matching file paths returned.
    pub files: usize,
    /// Number of matching directory paths returned.
    pub directories: usize,
    /// Sum of `files` and `directories`.
    pub total: usize,
    /// `true` when `limit` omitted additional matching paths.
    pub truncated: bool,
}

/// Output of a successful `glob` tool invocation.
#[derive(Debug)]
pub struct GlobResult {
    /// Paths included in this response. Empty when `only_count=true`.
    pub matched_paths: Vec<String>,
    /// Matching descendants returned after type filters and `limit` are applied.
    pub matches: GlobMatchCounts,
    /// `true` when `limit` omitted additional matching paths.
    pub truncated: bool,
}

impl GlobResult {
    fn new(matched_paths: Vec<String>, matches: GlobMatchCounts, truncated: bool) -> Self {
        GlobResult {
            matched_paths,
            matches,
            truncated,
        }
    }
}

impl ToolCallResult for GlobResult {
    fn text(&self) -> String {
        if self.matched_paths.is_empty() && self.matches.total > 0 {
            // only_count mode: no paths in response but the LLM needs the number.
            return self.matches.total.to_string();
        }
        self.matched_paths.join("\n")
    }

    fn display_summary(&self) -> Vec<(String, String)> {
        let mut summary = Vec::new();
        if self.matches.files > 0 {
            summary.push(("files".to_string(), self.matches.files.to_string()));
        }
        if self.matches.directories > 0 {
            summary.push((
                "directories".to_string(),
                self.matches.directories.to_string(),
            ));
        }
        if self.matches.total == 0 || (self.matches.files > 0 && self.matches.directories > 0) {
            summary.push(("total".to_string(), self.matches.total.to_string()));
        }
        if self.truncated {
            summary.push(("truncated".to_string(), "true".to_string()));
        }
        summary
    }

    fn metadata(&self) -> Option<serde_json::Value> {
        let metadata = GlobResultMetadata {
            files: self.matches.files,
            directories: self.matches.directories,
            total: self.matches.total,
            truncated: self.truncated,
        };
        serde_json::to_value(metadata).ok()
    }

    fn metadata_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(GlobResultMetadata))
    }

    fn content_sections(&self) -> Vec<Section> {
        let text = self.matched_paths.join("\n");
        if text.is_empty() {
            return vec![];
        }
        vec![Section::Content(ContentSection {
            label: None,
            content: text,
            file_extension: None,
            line_number_offset: Some(0),
        })]
    }
}

// ── ToolCall ──────────────────────────────────────────────────────────────────

/// Invocation object for a single `glob` tool call.
pub struct GlobCall {
    id: uuid::Uuid,
    raw_arguments: serde_json::Value,
    timeout_secs: u64,
}

impl ToolCall for GlobCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "glob"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.raw_arguments
    }
    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    fn argument_order(&self, key: &str) -> usize {
        GlobTool::default().argument_order(key)
    }

    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        let tool = GlobTool::default();
        self.raw_arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(key, value)| !tool.is_default_argument(key.as_str(), value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        let serialized = serde_json::to_string(&self.raw_arguments).unwrap_or_default();
        let span = tracing::info_span!("tool_args", args = %serialized);
        Box::pin(
            async move {
                let args = serde_json::from_value::<GlobInputArgs>(self.raw_arguments).map_err(
                    |error| ToolError::failed("glob", format!("invalid arguments: {error}")),
                )?;
                glob_files(&args)
                    .await
                    .map(|o| -> Box<dyn ToolCallResult> { Box::new(o) })
            }
            .instrument(span),
        )
    }
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `glob` tool: enumerate pathnames matching a pattern.
#[derive(Debug)]
pub struct GlobTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl GlobTool {
    pub const NAME: &'static str = "glob";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        GlobTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        GlobTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        GlobTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Search for paths matching `pattern` beneath the supplied search root. The search root \
        itself is never a match and is always excluded from returned paths and all counts, \
        regardless of the pattern or option flags. Only descendants of the search root are \
        eligible. The pattern is resolved relative to cwd unless absolute; to search beneath a \
        sub-directory, include that directory in the pattern (for example, `docs/**/*.md` or `/tmp/*.txt`). \
        There is no separate `path` argument. Use one glob pattern per call \
        (`*`,`?`,`**`,`[...]`); comma lists and `{a,b}` alternation match nothing. \
        `dot_ignore` (default `false`) excludes dot-prefixed paths. `git_ignore` (default `true`) \
        skips paths matched by `.gitignore` rules; it does not exclude the `.git` directory or \
        other dot-prefixed paths. Set `dot_ignore=true` to exclude them. `only_files`/`only_dirs` \
        are mutually exclusive type filters; both `true` is an error. Omit the optional `limit` \
        argument for no result-count limit; do not use an arbitrary large value to request \
        unlimited results. Bare `**` matches directories; use `**/*` to match files and \
        directories recursively. Result metadata contains \
        flat `files`, `directories`, and `total` counts plus a boolean `truncated`, which is \
        `false` when all matching paths are returned and `true` when `limit` omits additional \
        paths. The `pattern` argument accepts `~` and `$ENVVAR`/`${ENVVAR}` tokens; they are \
        expanded before use."
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(GlobInputArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["pattern"]
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        let pattern_str = match require_string_arg(arguments, "pattern") {
            Ok(value) => value,
            Err(denied) => return denied,
        };
        let root = pattern_root(pattern_str);
        // A single candidate path is checked: the pattern's root directory.
        // `dot_path` is exempt (`check_path_allowing_dot_paths`) because glob
        // only enumerates, and a configured `cross_filesystem` rule compares
        // this root's device — a mount point nested below it is never
        // detected, since result paths are not re-checked.
        let sandbox_verdict = self
            .sandbox_context
            .check_path_allowing_dot_paths(std::path::Path::new(&root));
        let approval = match sandbox_verdict {
            None => ApprovalCheck::AutoApproved,
            Some(verdict) => verdict,
        };
        tracing::debug!(
            tool = "glob",
            pattern = pattern_str,
            root = root,
            sandbox = match &approval {
                ApprovalCheck::AutoApproved => "abstain→auto-approved",
                ApprovalCheck::AutoDenied { .. } => "auto-denied",
                ApprovalCheck::UserApprovalRequired => "user-approval-required",
            },
            "tool approval decision"
        );
        approval
    }

    fn is_default_argument(&self, key: &str, value: &Value) -> bool {
        match (key, value) {
            ("git_ignore", Value::Bool(git_ignore)) => *git_ignore,
            ("only_count", Value::Bool(false)) => true,
            ("only_files", Value::Bool(false)) => true,
            ("only_dirs", Value::Bool(false)) => true,
            ("dot_ignore", Value::Bool(false)) => true,
            ("limit", Value::Null) => true,
            _ => false,
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(GlobCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
        })
    }
}

fn pattern_root(pattern: &str) -> String {
    let first_glob_position = pattern
        .char_indices()
        .find(|(_, character)| matches!(character, '*' | '?' | '['))
        .map(|(index, _)| index);
    let Some(first_glob_position) = first_glob_position else {
        return ".".to_string();
    };
    let slash_position = pattern[..first_glob_position].rfind('/');
    match slash_position {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(position) => pattern[..position].to_string(),
    }
}

fn normalize_glob_match_path(path: &Path) -> String {
    let path_string = path.display().to_string();
    if path_string == "." {
        String::new()
    } else if let Some(stripped_prefix) = path_string.strip_prefix("./") {
        stripped_prefix.to_string()
    } else {
        path_string
    }
}

fn path_matches_type_filter(path: &Path, args: &GlobInputArgs) -> bool {
    let file_type = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        Err(_) => return false,
    };
    if args.only_files {
        return file_type.is_file();
    }
    if args.only_dirs {
        return file_type.is_dir();
    }
    true
}

fn is_search_root(path: &Path, root: &str) -> bool {
    normalize_glob_match_path(path) == normalize_glob_match_path(Path::new(root))
}

/// Reject glob patterns that contain constructs the `glob` crate silently
/// misinterprets, giving the caller a clear error instead of empty results.
///
/// Currently detected:
/// - `!` outside a `[...]` character class: the `glob` crate treats it as a
///   literal character, so patterns like `!*.rs` or `**/!foo` never match
///   anything. Negation is not supported; the caller must filter results
///   programmatically instead.
fn validate_glob_pattern(pattern: &str) -> Result<(), String> {
    let mut inside_bracket = false;
    for character in pattern.chars() {
        match character {
            '[' => inside_bracket = true,
            ']' => inside_bracket = false,
            '!' if !inside_bracket => {
                return Err(format!(
                    "invalid pattern '{pattern}': '!' outside '[...]' is not supported \
                     (the glob crate treats it as a literal character, so the pattern \
                     would never match). Negation is not supported; remove '!' and filter \
                     results after matching instead."
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

async fn glob_files(args: &GlobInputArgs) -> Result<GlobResult, ToolError> {
    if args.only_files && args.only_dirs {
        return Err(ToolError::failed(
            "glob",
            "arguments 'only_files' and 'only_dirs' cannot both be true",
        ));
    }
    if let Err(reason) = validate_glob_pattern(&args.pattern) {
        return Err(ToolError::failed("glob", reason));
    }
    tracing::debug!(pattern = %args.pattern, "glob searching");
    let root = pattern_root(&args.pattern);
    let candidate_paths: Vec<PathBuf> = if args.git_ignore {
        let compiled_pattern = glob::Pattern::new(&args.pattern).map_err(|error| {
            ToolError::failed(
                "glob",
                format!("invalid pattern '{}': {error}", args.pattern),
            )
        })?;
        let match_options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: args.dot_ignore,
        };
        let mut collected_paths: Vec<PathBuf> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .git_ignore(true)
            .require_git(false)
            .hidden(args.dot_ignore)
            .build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let entry_path = entry.path();
            let match_path = normalize_glob_match_path(entry_path);
            if is_search_root(entry_path, &root)
                || !compiled_pattern.matches_with(&match_path, match_options)
            {
                continue;
            }
            collected_paths.push(entry_path.to_path_buf());
        }
        collected_paths
    } else {
        let match_options = glob::MatchOptions {
            require_literal_leading_dot: args.dot_ignore,
            ..Default::default()
        };
        let all_matches: Vec<_> = glob::glob_with(&args.pattern, match_options)
            .map_err(|error| {
                ToolError::failed(
                    "glob",
                    format!("invalid pattern '{}': {error}", args.pattern),
                )
            })?
            .filter_map(Result::ok)
            .filter(|path| !is_search_root(path, &root))
            .collect();
        all_matches
    };

    let mut candidates: Vec<GlobCandidate> = candidate_paths
        .into_iter()
        .map(GlobCandidate::from_path)
        .collect();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));

    let mut selected_candidates = Vec::new();
    for candidate in candidates {
        if path_matches_type_filter(&candidate.path, args) {
            selected_candidates.push(candidate);
        }
    }

    let mut truncated = false;
    if let Some(limit) = args.limit {
        if selected_candidates.len() > limit {
            selected_candidates.truncate(limit);
            truncated = true;
        }
    }

    let mut matches = GlobMatchCounts::default();
    let mut matched_paths = Vec::with_capacity(selected_candidates.len());
    for candidate in selected_candidates {
        matches.add_path_kind(candidate.path_kind);
        matched_paths.push(candidate.path.display().to_string());
    }

    if args.only_count {
        matched_paths.clear();
    }
    tracing::debug!(matches = matches.total, truncated, "glob searched");
    Ok(GlobResult::new(matched_paths, matches, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    fn glob_args(pattern: impl Into<String>) -> GlobInputArgs {
        GlobInputArgs {
            pattern: pattern.into(),
            git_ignore: true,
            only_count: false,
            only_files: false,
            only_dirs: false,
            limit: None,
            dot_ignore: false,
        }
    }

    fn path_counts(files: usize, directories: usize) -> GlobMatchCounts {
        GlobMatchCounts {
            files,
            directories,
            total: files + directories,
        }
    }

    #[test]
    fn glob_pattern_prefix_scopes_matches_to_subdirectory() {
        crate::test_support::run_locked(async {
            let search_root = TempDir::new().expect("tempdir");
            let docs_directory = search_root.path().join("docs");
            std::fs::create_dir(&docs_directory).expect("mkdir");
            std::fs::write(search_root.path().join("README.md"), "outside").expect("write");
            let markdown_path = docs_directory.join("README.md");
            std::fs::write(&markdown_path, "scoped").expect("write");

            let args = GlobInputArgs {
                pattern: format!("{}/**/*.md", docs_directory.display()),
                git_ignore: false,
                ..glob_args("")
            };
            let result = glob_files(&args).await.expect("glob ok");

            assert_eq!(
                result.matched_paths,
                vec![markdown_path.display().to_string()]
            );
        });
    }

    #[test]
    fn glob_title_arguments_omit_unsupported_fields() {
        let tool = GlobTool::default();
        let call = tool.create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "pattern": "docs/**/*.md",
                "path": "docs",
            }),
        );

        assert_eq!(
            call.title_arguments(),
            vec![("pattern".to_string(), serde_json::json!("docs/**/*.md"))]
        );
    }

    #[test]
    fn glob_tool_orders_pattern_first_and_marks_defaults() {
        let tool = GlobTool::default();
        assert_eq!(tool.argument_order("pattern"), 0);
        assert!(tool.is_default_argument("git_ignore", &Value::Bool(true)));
        assert!(tool.is_default_argument("only_count", &Value::Bool(false)));
        assert!(tool.is_default_argument("only_files", &Value::Bool(false)));
        assert!(tool.is_default_argument("only_dirs", &Value::Bool(false)));
        assert!(tool.is_default_argument("limit", &Value::Null));
        assert!(!tool.is_default_argument("git_ignore", &Value::Bool(false)));
    }

    #[test]
    fn glob_tool_with_timeout_constructor() {
        let tool = GlobTool::with_timeout(
            30,
            SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
                crate::sandbox::Sandbox::new(),
            ))),
        );
        assert_eq!(tool.timeout_secs(), 30);
    }

    #[test]
    fn glob_tool_missing_pattern_is_auto_denied() {
        let tool = GlobTool::default();
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({})),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
        assert!(matches!(
            tool.needs_approval(&serde_json::json!({ "pattern": "" })),
            crate::sandbox::ApprovalCheck::AutoDenied { .. }
        ));
    }

    #[test]
    fn glob_with_git_ignore_false_matches_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let sub = dir.path().join("sub");
            std::fs::create_dir_all(&sub).expect("mkdir");
            for name in ["a.txt", "b.txt"] {
                std::fs::write(dir.path().join(name), name).expect("write");
            }
            std::fs::write(sub.join("c.txt"), "c").expect("write");

            let matches = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            let text = matches.text();
            let lines: Vec<&str> = text.lines().collect();
            assert!(lines.iter().any(|l: &&str| l.ends_with("a.txt")));
            assert!(lines.iter().any(|l: &&str| l.ends_with("b.txt")));
            assert!(!lines.iter().any(|l: &&str| l.ends_with("c.txt")));
        });
    }

    #[test]
    fn glob_with_git_ignore_true_matches_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(dir.path().join("ignored.txt"), "ignored").expect("write");
            std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");

            let result = glob_files(&glob_args(format!("{}/*.txt", dir.path().display())))
                .await
                .expect("glob ok");
            assert!(result
                .matched_paths
                .iter()
                .any(|path| path.ends_with("a.txt")));
            assert!(!result
                .matched_paths
                .iter()
                .any(|path| path.ends_with("ignored.txt")));
        });
    }

    #[test]
    fn glob_with_git_ignore_true_keeps_single_star_within_directory() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let nested_directory = dir.path().join("nested");
            std::fs::create_dir_all(&nested_directory).expect("mkdir");
            let direct_path = dir.path().join("direct.md");
            let nested_path = nested_directory.join("nested.md");
            std::fs::write(&direct_path, "direct").expect("write");
            std::fs::write(&nested_path, "nested").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.md", dir.path().display()),
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert_eq!(
                result.matched_paths,
                vec![direct_path.display().to_string()]
            );
        });
    }

    #[test]
    fn glob_only_files_excludes_dirs() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let subdirectory = dir.path().join("sub");
            std::fs::create_dir_all(&subdirectory).expect("mkdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/**", dir.path().display()),
                only_files: true,
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            assert!(result
                .matched_paths
                .iter()
                .all(|path| std::path::Path::new(path).is_file()));
        });
    }

    #[test]
    fn glob_only_dirs_excludes_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            let subdirectory = dir.path().join("sub");
            std::fs::create_dir_all(&subdirectory).expect("mkdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/**", dir.path().display()),
                only_dirs: true,
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            assert!(!result.matched_paths.is_empty());
            assert!(result
                .matched_paths
                .iter()
                .all(|path| std::path::Path::new(path).is_dir()));
        });
    }

    #[test]
    fn glob_structured_metadata_counts_files_directories_and_total() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("a.txt"), "a").expect("write");
            std::fs::write(dir.path().join("b.txt"), "b").expect("write");
            std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
            let pattern = format!("{}/*", dir.path().display());

            let result = glob_files(&GlobInputArgs {
                pattern: pattern.clone(),
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");
            assert_eq!(result.matches, path_counts(2, 1));
            assert!(!result.truncated);
            assert_eq!(
                result.metadata(),
                Some(json!({
                    "files": 2,
                    "directories": 1,
                    "total": 3,
                    "truncated": false,
                }))
            );

            let files_only = glob_files(&GlobInputArgs {
                pattern: pattern.clone(),
                git_ignore: false,
                only_files: true,
                ..glob_args("")
            })
            .await
            .expect("glob files ok");
            assert_eq!(files_only.matches, path_counts(2, 0));
            assert!(!files_only.truncated);

            let directories_only = glob_files(&GlobInputArgs {
                pattern,
                git_ignore: false,
                only_dirs: true,
                ..glob_args("")
            })
            .await
            .expect("glob directories ok");
            assert_eq!(directories_only.matches, path_counts(0, 1));
            assert!(!directories_only.truncated);
        });
    }

    #[test]
    fn glob_display_summary_omits_default_attributes() {
        let empty_result = GlobResult::new(vec![], path_counts(0, 0), false);
        assert_eq!(
            empty_result.display_summary(),
            vec![("total".to_string(), "0".to_string())]
        );
        let empty_metadata = empty_result.metadata().expect("glob metadata");
        assert_eq!(empty_metadata["files"], json!(0));
        assert_eq!(empty_metadata["directories"], json!(0));
        assert_eq!(empty_metadata["truncated"], json!(false));

        let files_result = GlobResult::new(vec![], path_counts(2, 0), false);
        assert_eq!(
            files_result.display_summary(),
            vec![("files".to_string(), "2".to_string())]
        );

        let directories_result = GlobResult::new(vec![], path_counts(0, 3), true);
        assert_eq!(
            directories_result.display_summary(),
            vec![
                ("directories".to_string(), "3".to_string()),
                ("truncated".to_string(), "true".to_string()),
            ]
        );

        let mixed_result = GlobResult::new(vec![], path_counts(2, 3), false);
        assert_eq!(
            mixed_result.display_summary(),
            vec![
                ("files".to_string(), "2".to_string()),
                ("directories".to_string(), "3".to_string()),
                ("total".to_string(), "5".to_string()),
            ]
        );
    }

    #[test]
    fn glob_bare_double_star_requires_leaf_pattern_for_files() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
            std::fs::write(dir.path().join("file.txt"), "content").expect("write");

            let directories_pattern = format!("{}/**", dir.path().display());
            let directories_result = glob_files(&GlobInputArgs {
                pattern: directories_pattern,
                git_ignore: false,
                only_files: true,
                ..glob_args("")
            })
            .await
            .expect("glob directories pattern");
            assert!(directories_result.matched_paths.is_empty());
            assert_eq!(directories_result.matches, path_counts(0, 0));

            let files_pattern = format!("{}/**/*", dir.path().display());
            let files_result = glob_files(&GlobInputArgs {
                pattern: files_pattern,
                git_ignore: false,
                only_files: true,
                ..glob_args("")
            })
            .await
            .expect("glob files pattern");
            assert_eq!(files_result.matches, path_counts(1, 0));
            assert!(files_result
                .matched_paths
                .iter()
                .any(|path| path.ends_with("file.txt")));
        });
    }

    #[test]
    fn glob_never_includes_search_root_in_paths_or_counts() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
            let pattern = format!("{}/**", dir.path().display());
            let root_path = dir.path().display().to_string();

            for git_ignore in [false, true] {
                let result = glob_files(&GlobInputArgs {
                    pattern: pattern.clone(),
                    git_ignore,
                    ..glob_args("")
                })
                .await
                .expect("glob ok");

                assert!(
                    !result.matched_paths.iter().any(|path| path == &root_path),
                    "search root must never be returned"
                );
                assert_eq!(result.matches, path_counts(0, 1), "git_ignore={git_ignore}");
                assert!(!result.truncated, "git_ignore={git_ignore}");
            }
        });
    }

    #[test]
    fn glob_contradictory_only_files_and_dirs_is_error() {
        crate::test_support::run_locked(async {
            let result = glob_files(&GlobInputArgs {
                pattern: "*.txt".to_string(),
                only_files: true,
                only_dirs: true,
                ..glob_args("")
            })
            .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn glob_limit_caps_results_and_sets_truncated_metadata() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            for index in 0..5 {
                std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                    .expect("write");
            }

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                limit: Some(3),
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert_eq!(result.matched_paths.len(), 3);
            assert_eq!(result.matches, path_counts(3, 0));
            assert!(result.truncated);
            assert_eq!(
                result.metadata(),
                Some(json!({
                    "files": 3,
                    "directories": 0,
                    "total": 3,
                    "truncated": true,
                }))
            );
        });
    }

    #[test]
    fn glob_only_count_returns_empty_content_with_total_in_metadata() {
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            for index in 0..5 {
                std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                    .expect("write");
            }

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                only_count: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert_eq!(result.text(), "5");
            assert!(result.matched_paths.is_empty());
            assert_eq!(result.matches, path_counts(5, 0));
            assert!(!result.truncated);
            assert_eq!(
                result.metadata(),
                Some(json!({
                    "files": 5,
                    "directories": 0,
                    "total": 5,
                    "truncated": false,
                }))
            );
        });
    }

    #[test]
    fn glob_needs_approval_auto_approves_inside_workspace() {
        let tool = GlobTool::default();
        let args = serde_json::json!({ "pattern": "src/**/*.rs" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn glob_needs_approval_allows_dot_paths_inside_workspace() {
        let tool = GlobTool::default();
        let args = serde_json::json!({ "pattern": ".git/**/*" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::AutoApproved
        ));
    }

    #[test]
    fn glob_needs_approval_outside_workspace_requires_user_approval() {
        let tool = GlobTool::default();
        let args = serde_json::json!({ "pattern": "/tmp/**" });
        assert!(matches!(
            tool.needs_approval(&args),
            ApprovalCheck::UserApprovalRequired
        ));
    }

    #[test]
    fn glob_invalid_pattern_is_error() {
        crate::test_support::run_locked(async {
            let result = glob_files(&glob_args("[")).await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn validate_glob_pattern_rejects_negation_outside_brackets() {
        assert!(validate_glob_pattern("docs/**/!*.md").is_err());
        assert!(validate_glob_pattern("!*.rs").is_err());
        assert!(validate_glob_pattern("src/**/!foo").is_err());
        // Quoted-style artifact the LLM produced
        assert!(validate_glob_pattern("docs/**/!*\"").is_err());
    }

    #[test]
    fn validate_glob_pattern_allows_negation_inside_brackets() {
        // [!abc] is valid glob syntax (character class negation)
        assert!(validate_glob_pattern("src/[!_]*.rs").is_ok());
        assert!(validate_glob_pattern("**/*.[!d]").is_ok());
    }

    #[test]
    fn validate_glob_pattern_allows_normal_patterns() {
        assert!(validate_glob_pattern("**/*.rs").is_ok());
        assert!(validate_glob_pattern("docs/**/*.md").is_ok());
        assert!(validate_glob_pattern("src/**/mod.rs").is_ok());
        assert!(validate_glob_pattern("").is_ok());
    }

    #[test]
    fn glob_files_returns_error_for_negation_pattern() {
        let args = GlobInputArgs {
            pattern: "docs/**/!*.md".to_string(),
            git_ignore: false,
            ..glob_args("")
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(glob_files(&args));
        assert!(result.is_err());
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("'!'"),
            "error should mention the offending character: {message}"
        );
        assert!(
            message.contains("not supported"),
            "error should say negation is not supported: {message}"
        );
    }

    #[test]
    fn glob_files_logs_pattern_and_match_count() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("seed file");
        std::fs::write(dir.path().join("b.txt"), "b").expect("seed file");
        let args = GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            ..glob_args("")
        };
        let log = capture_logs(async move {
            let result = glob_files(&args).await;
            assert!(result.is_ok());
        });
        assert!(
            log.contains("glob searched"),
            "glob_files must log its result: {log}"
        );
        assert!(
            log.contains("matches=2"),
            "glob_files must log the match count: {log}"
        );
    }

    #[test]
    fn glob_result_content_sections_empty_and_nonempty() {
        let empty_result = GlobResult::new(vec![], path_counts(0, 0), false);
        assert!(empty_result.content_sections().is_empty());

        let result = GlobResult::new(vec!["src/main.rs".to_string()], path_counts(1, 0), false);
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        assert!(matches!(sections[0], super::Section::Content(_)));
    }

    #[test]
    fn glob_call_tool_call_methods() {
        let id = uuid::Uuid::new_v4();
        let args = serde_json::json!({ "pattern": "**/*.rs" });
        let call = GlobTool::default().create_call(id, args.clone());
        assert_eq!(call.id(), id);
        assert_eq!(call.name(), "glob");
        assert_eq!(call.arguments(), &args);
        assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
        assert_eq!(call.argument_order("pattern"), 0);
        let redacted = call.non_default_redacted_arguments();
        assert!(redacted.iter().any(|(key, _)| key == "pattern"));
    }

    #[test]
    fn glob_default_includes_hidden_files() {
        // dot_ignore defaults to false — hidden files must appear in results.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be present when dot_ignore=false"
            );
        });
    }

    #[test]
    fn glob_dot_ignore_excludes_hidden_files() {
        // dot_ignore=true must exclude any path component starting with '.'.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: false,
                dot_ignore: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                !result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be absent when dot_ignore=true"
            );
        });
    }

    #[test]
    fn glob_dot_ignore_excludes_hidden_files_git_ignore_mode() {
        // dot_ignore=true must also exclude hidden files in the git_ignore=true path.
        crate::test_support::run_locked(async {
            let dir = TempDir::new().expect("tempdir");
            std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
            std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

            let result = glob_files(&GlobInputArgs {
                pattern: format!("{}/*.txt", dir.path().display()),
                git_ignore: true,
                dot_ignore: true,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with("visible.txt")),
                "visible file must be present"
            );
            assert!(
                !result
                    .matched_paths
                    .iter()
                    .any(|p| p.ends_with(".hidden.txt")),
                "hidden file must be absent when dot_ignore=true"
            );
        });
    }

    #[test]
    fn glob_input_args_all_properties_have_descriptions() {
        let schema = GlobTool::default().input_schema();
        super::super::assert_all_properties_have_descriptions(&schema, "GlobInputArgs");
    }

    #[test]
    fn glob_description_defines_search_root_exclusion() {
        let tool = GlobTool::default();
        let description = tool.description();
        assert!(description.contains(
            "The search root itself is never a match and is always excluded from returned paths \
             and all counts"
        ));
        assert!(description.contains("Only descendants of the search root are eligible"));
    }

    #[test]
    fn glob_descriptions_distinguish_git_ignore_from_dot_ignore() {
        let tool = GlobTool::default();
        let clarification = "does not exclude the `.git` directory or other dot-prefixed paths";
        assert!(tool.description().contains(clarification));

        let schema = serde_json::to_value(tool.input_schema()).expect("serialize glob schema");
        let git_ignore_description = schema["properties"]["git_ignore"]["description"]
            .as_str()
            .expect("git_ignore description");
        assert!(git_ignore_description.contains(clarification));
    }

    #[test]
    fn glob_limit_descriptions_define_omission_as_unlimited() {
        let tool = GlobTool::default();
        let clarification = "Omit";
        assert!(tool.description().contains(clarification));
        assert!(tool.description().contains("no result-count limit"));
        assert!(tool.description().contains("arbitrary large value"));

        let schema = serde_json::to_value(tool.input_schema()).expect("serialize glob schema");
        let limit_description = schema["properties"]["limit"]["description"]
            .as_str()
            .expect("limit description");
        assert!(limit_description.contains(clarification));
        assert!(limit_description.contains("no result-count limit"));
        assert!(limit_description.contains("arbitrary large value"));
    }
}
