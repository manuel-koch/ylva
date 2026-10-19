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

//! `search` tool: text search using `ignore` + `regex` crates; no external
//! binary required.
//!
//! Results are formatted in grep style (`filepath:lineno: content` for
//! matches, `filepath-lineno- content` for context lines). One
//! [`SearchResultSection`] is emitted per matching file. Total output is
//! capped at [`SEARCH_OUTPUT_LIMIT_LINES`] whole output lines or
//! [`SEARCH_OUTPUT_LIMIT_BYTES`], whichever is hit first.

use std::path::Path;
use std::sync::LazyLock;

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;

use super::{Tool, ToolCall, DEFAULT_TOOL_TIMEOUT_SECS};
use crate::sandbox::{ApprovalCheck, SandboxContext};

/// Maximum number of whole output lines in the combined search output. Lines
/// beyond this cap are discarded; the result's `metadata()` will carry
/// `truncated`.
pub const SEARCH_OUTPUT_LIMIT_LINES: usize = 1000;

/// Maximum number of bytes of line/match content in the combined search
/// output, independent of [`SEARCH_OUTPUT_LIMIT_LINES`]. Bounds worst-case
/// memory/context usage when matched lines are very long (e.g. a match
/// inside a minified file) that the line cap alone cannot catch. Whichever
/// cap is hit first stops collection; the result's `metadata()` will carry
/// `truncated`.
pub const SEARCH_OUTPUT_LIMIT_BYTES: usize = 65_536;

/// Number of bytes inspected to detect binary files (null-byte heuristic).
const BINARY_PROBE_BYTES: usize = 8 * 1024;

static SEARCH_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Search files under a directory tree for a regex `pattern` (grep-style: \
        `filepath:lineno: content` matches, `filepath-lineno- content` context). Alternation is \
        `|`, e.g. `foo|bar` (not the English words `OR`/`AND`, which are matched as literal \
        text). Binary files skipped; output capped at {} bytes. `glob` (e.g. `**/*.rs`) restricts \
        searched files. `files_with_matches: true` returns only file paths. `only_matches: true` \
        returns only matched substrings, like `grep -o`. `files_with_matches`/`only_matches` are \
        mutually exclusive. The `path` argument accepts `~` and `$ENVVAR`/`${{ENVVAR}}` tokens; \
         they are expanded before use.",
        SEARCH_OUTPUT_LIMIT_BYTES
    )
});

// ── Argument schema ───────────────────────────────────────────────────────────

/// Arguments accepted by the `search` tool.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub struct SearchInputArgs {
    /// Regular expression to search for. Alternation uses `|`, not English
    /// words: `foo|bar` matches "foo" or "bar"; the literal text "foo OR bar"
    /// only matches that exact phrase.
    pub pattern: String,
    /// Directory or file to search within (default: cwd).
    pub path: Option<String>,
    /// Glob restricting which files are searched (e.g. `**/*.rs`), matched against
    /// each path relative to the search root. Omit to search all non-binary files.
    pub glob: Option<String>,
    /// Case-sensitive match (default `false`).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Lines of context before/after each match (default `0`). Ignored when
    /// `only_matches` is `true`.
    #[serde(default)]
    pub context_lines: usize,
    /// Return only paths of files with ≥1 match, no line content. Mutually
    /// exclusive with `only_matches`.
    #[serde(default)]
    pub files_with_matches: bool,
    /// Return only matched substrings, one per occurrence (like `grep -o`);
    /// context lines ignored. Mutually exclusive with `files_with_matches`.
    #[serde(default)]
    pub only_matches: bool,
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// The `search` tool: text search over a directory tree using `regex` and
/// `ignore`.
#[derive(Debug)]
pub struct SearchTool {
    timeout_secs: u64,
    sandbox_context: SandboxContext,
}

impl SearchTool {
    pub const NAME: &'static str = "search";

    pub fn new(sandbox_context: SandboxContext) -> Self {
        SearchTool {
            timeout_secs: 0,
            sandbox_context,
        }
    }

    pub fn with_timeout(timeout_secs: u64, sandbox_context: SandboxContext) -> Self {
        SearchTool {
            timeout_secs,
            sandbox_context,
        }
    }
}

impl Default for SearchTool {
    fn default() -> Self {
        use crate::sandbox::Sandbox;
        SearchTool::new(SandboxContext::new(std::sync::Arc::new(
            std::sync::RwLock::new(Sandbox::new()),
        )))
    }
}

impl Tool for SearchTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        &SEARCH_DESCRIPTION
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        schema_for!(SearchInputArgs)
    }

    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn needs_approval(&self, arguments: &Value) -> ApprovalCheck {
        match arguments.get("path").and_then(|v| v.as_str()) {
            Some(path_str) if !path_str.is_empty() => {
                let sandbox_verdict = self.sandbox_context.check_path(Path::new(path_str));
                let approval = match sandbox_verdict {
                    None => ApprovalCheck::AutoApproved,
                    Some(verdict) => verdict,
                };
                tracing::debug!(
                    tool = "search",
                    path = path_str,
                    sandbox = match &approval {
                        ApprovalCheck::AutoApproved => "abstain→auto-approved",
                        ApprovalCheck::AutoDenied { .. } => "auto-denied",
                        ApprovalCheck::UserApprovalRequired => "user-approval-required",
                    },
                    "tool approval decision"
                );
                approval
            }
            // No explicit path → defaults to cwd (inside workspace) → auto-approved.
            _ => ApprovalCheck::AutoApproved,
        }
    }

    fn timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            DEFAULT_TOOL_TIMEOUT_SECS
        } else {
            self.timeout_secs
        }
    }

    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn create_call(&self, id: uuid::Uuid, arguments: Value) -> Box<dyn ToolCall> {
        Box::new(SearchCall {
            id,
            raw_arguments: arguments,
            timeout_secs: self.timeout_secs(),
            runtime_output_token_limit: None,
        })
    }
}

mod call;
mod engine;
mod result;

pub use call::SearchCall;
pub use result::{SearchResult, SearchResultMetadata};

#[cfg(test)]
mod tests;
