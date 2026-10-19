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

use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;

use super::{Tool, ToolCall, DEFAULT_TOOL_TIMEOUT_SECS};
use crate::sandbox::{ApprovalCheck, SandboxContext};
use traversal::pattern_root;

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
        expanded before use. Use the optional `limit` to keep large result sets manageable."
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
            runtime_output_token_limit: None,
        })
    }
}

mod call;
mod result;
mod traversal;

pub use call::GlobCall;
pub use result::{GlobMatchCounts, GlobResult, GlobResultMetadata};

#[cfg(test)]
mod tests;
