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

//! Tool trait, registry, and the built-in file-system tools.
//!
//! Schemas are derived from typed argument structs via `schemars`, never
//! hand-written JSON. The registry keeps a per-run `allowed` seam so a future
//! whitelist or headless filtering can restrict the `tools` array without
//! changing the agent loop.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::schema::RootSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ToolAttachment;
use crate::llm::ToolSchema;
use crate::sandbox::ApprovalCheck;

pub(crate) use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;

pub(crate) fn utf8_text_preview(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

pub mod agent_finish;
pub mod backends;
pub mod bash;
pub mod clarify;
pub mod edit;
pub mod glob;
pub mod load_skill;
pub mod read;
pub mod search;
pub mod spawn_subagent;
pub mod web_fetch;
pub mod web_search;
pub mod write;

/// Default execution timeout for tools.
///
/// Single source of truth: [`super::DEFAULT_TOOL_TIMEOUT_SECS`]. Re-exported
/// here so tool modules can `use` it alongside the other tool-trait items
/// without an extra `crate::agent::` import.
pub use super::DEFAULT_TOOL_TIMEOUT_SECS;

/// Transport encoding for file content accepted by the `read` and `write`
/// tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileEncoding {
    /// UTF-8 text; text-oriented reads reject invalid UTF-8.
    #[default]
    Utf8,
    /// Standard padded Base64 without line breaks.
    Base64,
}

/// Failure from a single tool invocation, mapped to the flat
/// `String`-returning [`crate::agent::PendingTool::execute`] contract and to
/// `UiUpdate::ToolError` / `Message::ToolError`.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool '{name}' failed: {message}")]
    Failed {
        name: String,
        message: String,
        /// Output the tool produced before the error or timeout (if any).
        partial_output: Option<String>,
    },
}

impl ToolError {
    pub fn failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        ToolError::Failed {
            name: name.into(),
            message: message.into(),
            partial_output: None,
        }
    }

    pub fn failed_with_output(
        name: impl Into<String>,
        message: impl Into<String>,
        partial_output: String,
    ) -> Self {
        ToolError::Failed {
            name: name.into(),
            message: message.into(),
            partial_output: if partial_output.is_empty() {
                None
            } else {
                Some(partial_output)
            },
        }
    }

    /// Returns the partial output carried by this error, if any.
    pub fn partial_output(&self) -> Option<&str> {
        match self {
            ToolError::Failed { partial_output, .. } => partial_output.as_deref(),
        }
    }
}

/// Successful output from a tool invocation.
///
/// Every tool returns its own concrete type that implements this trait.
/// The trait exposes two concerns that belong at different layers:
/// - [`text`](ToolCallResult::text): the LLM-facing text;
/// - [`properties`](ToolCallResult::properties): domain facts the
///   result carries (e.g. bytes read, paths matched).  Consumers above the
///   tool layer — the agent, the TUI — decide how to use these facts; the
///   tool itself does not know.
pub trait ToolCallResult: Send + Sync + std::fmt::Debug {
    /// Reject success unless the entire serialized result fits the live allowance.
    fn requires_complete_delivery(&self) -> bool {
        false
    }
    /// The text returned to the LLM as the tool's result message.
    fn text(&self) -> String;

    /// Optional text rendered in the user-facing tool bubble's result section.
    ///
    /// This is separate from [`text`](ToolCallResult::text), which is always
    /// the model-facing result. Binary transports may need to keep an encoded
    /// result for the model while suppressing that transport representation in
    /// the UI.
    fn display_text(&self) -> Option<String> {
        Some(self.text())
    }

    /// Structured metadata for this result.
    ///
    /// The complete value is embedded in the `result_metadata` field of the
    /// tool-response envelope. The CLI/TUI projection is provided separately
    /// by [`display_summary`](Self::display_summary).
    ///
    /// Returns `None` for tools whose text already captures everything relevant.
    fn metadata(&self) -> Option<serde_json::Value> {
        None
    }

    /// Optional schema describing the fields returned by [`metadata`](Self::metadata).
    ///
    /// This is LLM-facing documentation, not Rust-side validation and not a
    /// UI projection. When metadata is present, the agent serializes this
    /// schema into `result_metadata_schema` in the `role:"tool"` envelope so
    /// the model can interpret the structured result fields.
    ///
    /// Only fields emitted for this result instance should appear. Conditional
    /// fields may be omitted by returning a narrower schema.
    fn metadata_schema(&self) -> Option<RootSchema> {
        None
    }

    /// Properties to render in the CLI/TUI tool summary.
    ///
    /// The default renders every property from [`metadata`](Self::metadata).
    /// Tools may omit default-valued properties here without changing the
    /// complete structured metadata sent to the LLM/API.
    fn display_summary(&self) -> Vec<(String, String)> {
        self.metadata()
            .and_then(|metadata| metadata.as_object().cloned())
            .map(|metadata| {
                metadata
                    .into_iter()
                    .map(|(key, value)| {
                        let display = match value {
                            serde_json::Value::String(value) => value,
                            other => other.to_string(),
                        };
                        (key, display)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Provider-neutral media descriptors for this result. Descriptors refer
    /// to the successful result's `text()` value and never own a payload.
    fn attachments(&self) -> Vec<ToolAttachment> {
        vec![]
    }

    /// Rich content sections produced by this result (e.g. file text, command
    /// output). Computed once at execution time.
    fn content_sections(&self) -> Vec<Section> {
        vec![]
    }
}

/// A named content block produced by a tool call.
///
/// Returned by [`ToolCall::pending_content_sections`] and
/// [`ToolCallResult::content_sections`] to expose structured content (file
/// text, command output, code) with optional syntax and line-number metadata.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentSection {
    /// Optional label for this block. Absent when the call produces only one
    /// section and a label would add no information.
    pub label: Option<String>,
    /// The text content of this block.
    pub content: String,
    /// Lowercase file-extension hint for syntax highlighting
    /// (e.g. `"rs"`, `"py"`, `"json"`). `None` = plain text.
    pub file_extension: Option<String>,
    /// 0-based index of the first line of `content` within its source file.
    /// `None` = offset unknown; no line-number annotation.
    pub line_number_offset: Option<usize>,
}

/// A diff block produced by a tool call (e.g. `edit`).
///
/// Lines are unified-diff text from the `similar` crate; `+` lines are
/// additions, `−` lines are removals. Renderers that support colour should
/// paint additions green and removals red.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffSection {
    /// Optional label (e.g. the file path being patched).
    pub label: Option<String>,
    /// Unified diff text.
    pub content: String,
}

/// A single line in a search result — either a match line or a context line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResultLine {
    /// 1-based line number within the source file.
    pub line_number: usize,
    /// `true` for a match line, `false` for a context line.
    pub is_match: bool,
    /// Raw line content with no path/line-number prefix.
    pub content: String,
}

/// A block of search results from a single file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResultSection {
    /// Path of the file that was searched (mirrors the form of the `path`
    /// argument: relative when the input was relative, absolute when absolute).
    pub file_path: String,
    /// Lowercase file-extension hint for syntax-highlighting the content lines.
    /// `None` = plain text.
    pub file_extension: Option<String>,
    /// The matched (and context) lines in ascending line-number order.
    pub lines: Vec<SearchResultLine>,
}

/// A content block produced by a tool call — either a rich file / text
/// section or a unified diff.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Section {
    Content(ContentSection),
    Diff(DiffSection),
    SearchResult(SearchResultSection),
}

/// An invocation object for a specific tool call. Carries the call ID,
/// arguments, and execution logic for a single invocation.
pub trait ToolCall: Send + Sync {
    fn id(&self) -> uuid::Uuid;
    fn name(&self) -> &str;
    fn arguments(&self) -> &serde_json::Value;
    fn timeout_secs(&self) -> u64;

    /// Supply a per-request token ceiling for model-facing output. Tools that
    /// collect potentially large output may combine this with argument limits.
    fn set_runtime_output_token_limit(&mut self, _max_output_tokens: Option<usize>) {}

    /// Returns `true` when `key` names an argument whose value belongs in a
    /// content section rather than an inline invocation summary.
    fn is_body_argument(&self, _key: &str) -> bool {
        false
    }

    /// Whether `value` for `key` equals the call's semantic default, so a
    /// host can suppress it from an invocation title — e.g. `bash` hides an
    /// empty/whitespace-only `label`. Defaults to `false`; absent and `null`
    /// values are already filtered by [`Self::title_arguments`].
    fn is_default_argument(&self, _key: &str, _value: &serde_json::Value) -> bool {
        false
    }

    /// Content sections produced before execution completes (e.g. argument
    /// preview). Defaults to empty.
    fn pending_content_sections(&self) -> Vec<Section> {
        vec![]
    }

    /// Schema-defined display rank for `key`. Lower ranks appear earlier in
    /// invocation summaries; the lowest-ranked argument is the primary one.
    /// Implementations must delegate to their corresponding
    /// [`Tool::argument_order`] (e.g. `MyTool::new(..).argument_order(key)`) —
    /// its default already derives rank from the schema's declared property
    /// order, so a hand-rolled `match` is redundant and can silently drift out
    /// of sync as fields are added, renamed, or reordered. Only write a custom
    /// `match` if a tool genuinely needs an order that diverges from its
    /// schema's field order.
    fn argument_order(&self, key: &str) -> usize;

    /// Non-content-section, non-null arguments supported by the tool schema,
    /// sorted by schema-defined rank. Unknown input keys are not displayed.
    fn title_arguments(&self) -> Vec<(String, serde_json::Value)> {
        let mut args: Vec<(String, serde_json::Value)> = self
            .arguments()
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(k, v)| {
                !v.is_null()
                    && !self.is_body_argument(k)
                    && !self.is_default_argument(k, v)
                    && self.argument_order(k) != usize::MAX
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        args.sort_by_key(|(k, _)| self.argument_order(k));
        args
    }

    /// The non-default arguments for this call, with sensitive values redacted.
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)>;

    /// Execute the tool, consuming the call object, and return the result.
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>>;
}

/// Derives a display title from a tool's canonical `name` (e.g. `read` →
/// `Read`, `spawn_subagent` → `Spawn Subagent`): split on `-`/`_`, capitalise
/// the first letter of each word, join with spaces. Shared by
/// [`Tool::title`]'s default implementation and the TUI's tool-bubble title
/// rendering, so both stay in sync automatically for any tool name.
pub fn title_case_tool_name(name: &str) -> String {
    name.split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fold a tool's per-call subtype into its display title.
///
/// `subtype_argument` is [`Tool::subtype_argument`]: when that argument is
/// present in `title_arguments`, its value is appended to `title` as
/// `({value})` and the argument is dropped from the returned list, so a
/// renderer shows the variant exactly once — in the title rather than as a
/// bracketed `key=value` prop:
/// `Spawn Subagent (local-discovery) "Refactor the parser"`.
///
/// The title comes back unchanged and the arguments untouched when the tool
/// declares no subtype argument, the argument is absent (a default-valued
/// subtype never reaches a renderer), or its value is not a string. The value
/// is flattened to a single line so a subtype can never break the title row.
///
/// Shared by the CLI status line and the TUI tool bubble so the two cannot
/// drift apart.
pub fn display_title_with_subtype(
    title: &str,
    subtype_argument: Option<&str>,
    title_arguments: &[(String, serde_json::Value)],
) -> (String, Vec<(String, serde_json::Value)>) {
    let Some(subtype_key) = subtype_argument else {
        return (title.to_string(), title_arguments.to_vec());
    };
    let mut subtype_value: Option<String> = None;
    let mut remaining: Vec<(String, serde_json::Value)> = Vec::with_capacity(title_arguments.len());
    for (key, value) in title_arguments {
        if subtype_value.is_none() && key == subtype_key {
            if let Some(raw_value) = value.as_str() {
                subtype_value = Some(single_line_text(raw_value));
                continue;
            }
        }
        remaining.push((key.clone(), value.clone()));
    }
    match subtype_value {
        Some(value) => (format!("{title} ({value})"), remaining),
        None => (title.to_string(), remaining),
    }
}

/// Replace the control characters that would break a single-line title with
/// visible placeholders — the same mapping the TUI applies to title props.
fn single_line_text(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' | '\r' => '↵',
            '\t' => ' ',
            character => character,
        })
        .collect()
}

/// A named, schema-described tool the model may call.
///
/// The trait exposes tool **data** and argument metadata. Actual execution is
/// encapsulated in the [`ToolCall`] object created by [`Tool::create_call`].
/// Argument display metadata a host needs (which arguments are defaulted, and
/// their display order) stays here. Actual rendering (brackets, eliding long
/// values) stays the host's decision.
pub trait Tool: Send + Sync {
    /// The tool's canonical identifier, used by the model and as the registry
    /// key (e.g. `read`, `web_search`).
    fn name(&self) -> &str;

    /// A short display label for the tool (e.g. `Read`, `Web Search`).
    /// Derived automatically from [`name`](Self::name) via
    /// [`title_case_tool_name`]. Override only when the canonical name cannot
    /// produce the desired label.
    ///
    /// A subtype fixed by *config* rather than chosen per call is part of this
    /// label, because the tool instance already knows its variant (e.g. the
    /// parenthetical in `Web Search (Brave)`). A subtype the *model* picks per
    /// call belongs in [`subtype_argument`](Self::subtype_argument) instead.
    fn title(&self) -> String {
        title_case_tool_name(self.name())
    }

    /// The title argument that names which subtype of this tool this call
    /// performs — a specification/behavior/variant of the tool selected with
    /// the call (e.g. `spawn_subagent`'s `agent_type`).
    ///
    /// Renderers show it as `{title} ({value})` and drop it from the
    /// bracketed props, so the variant reads as part of the label:
    /// `Spawn Subagent (local-discovery) "Refactor the parser"`. Arguments
    /// equal to their default never reach a renderer (see
    /// [`is_default_argument`](Self::is_default_argument)), so an unspecified
    /// subtype qualifies nothing.
    ///
    /// `None` (the default) for tools without a per-call subtype — including
    /// the ones whose subtype is config-derived and already carried by
    /// [`title`](Self::title).
    fn subtype_argument(&self) -> Option<&'static str> {
        None
    }

    /// A human-readable description fed to the model alongside the schema.
    fn description(&self) -> &str;

    /// The `schemars`-derived parameter schema for the tool's arguments.
    fn input_schema(&self) -> RootSchema;

    /// Validates `arguments` against this tool's [`input_schema`](Tool::input_schema).
    ///
    /// Returns `Ok(())` when the arguments are structurally valid; `Err(reason)` with a
    /// human-readable description otherwise. Called by the agent before
    /// [`needs_approval`](Tool::needs_approval) so structurally invalid calls are rejected
    /// immediately — without an approval prompt — for all tools.
    ///
    /// The default implementation compiles the tool's JSON Schema and validates `arguments`
    /// against it. Tools with cross-field constraints that JSON Schema cannot express should
    /// override this and call [`validate_args_with_schema`] first, then add the extra checks.
    fn validate_args(&self, arguments: &Value) -> Result<(), String> {
        validate_args_with_schema(&self.input_schema(), arguments)
    }

    /// Whether this tool call requires explicit human approval.
    ///
    /// `AutoApproved` → execute immediately without pausing.
    /// `AutoDenied { reason }` → reject immediately, surface reason to the model.
    /// `UserApprovalRequired` → pause for human.
    ///
    /// Defaults to `UserApprovalRequired` (safe-by-default).
    fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
        ApprovalCheck::UserApprovalRequired
    }

    /// Whether an argument value equals the tool's semantic default, so a host
    /// can suppress it from an invocation summary. Defaults to `false`; tools
    /// override for optional arguments (e.g. `read`'s `mode: null`,
    /// `offset: 0`/`null`, `limit: null`).
    fn is_default_argument(&self, _key: &str, _value: &Value) -> bool {
        false
    }

    /// Display-order rank for an argument key: lower ranks are listed earlier
    /// in a host's invocation summary, and the lowest-ranked argument is the
    /// primary (inline) one. Ranks are derived from the tool's schema property
    /// order, which reflects the argument **struct's declaration order** (via
    /// schemars `preserve_order`), so no per-tool ordering needs to be
    /// hand-written. Unknown keys return `usize::MAX` and are omitted from
    /// invocation titles.
    ///
    /// Handles schemas produced by `#[serde(flatten)]` on a tagged enum, where
    /// schemars emits the outer struct's properties inside an `allOf` branch
    /// alongside the inner `oneOf`/`anyOf` for the enum variants.
    fn argument_order(&self, key: &str) -> usize {
        use schemars::schema::Schema;

        fn collect_property_names(schema: &Schema, out: &mut Vec<String>) {
            let Schema::Object(obj) = schema else { return };
            if let Some(object) = &obj.object {
                for name in object.properties.keys() {
                    if !out.contains(name) {
                        out.push(name.clone());
                    }
                }
            }
            if let Some(subschemas) = &obj.subschemas {
                for branch in subschemas.all_of.iter().flatten() {
                    collect_property_names(branch, out);
                }
            }
        }

        let schema = self.input_schema();
        let root = Schema::Object(schema.schema);
        let mut names: Vec<String> = Vec::new();
        collect_property_names(&root, &mut names);
        names
            .iter()
            .position(|name| name.as_str() == key)
            .unwrap_or(usize::MAX)
    }

    /// The supplied arguments that differ from the tool's defaults, as
    /// `(key, value)` pairs, so a host can render an invocation without
    /// hardcoding tool-specific argument names or default values.
    fn non_default_arguments<'a>(&self, arguments: &'a Value) -> Vec<(&'a str, &'a Value)> {
        arguments
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(key, value)| !self.is_default_argument(key.as_str(), value))
            .map(|(key, value)| (key.as_str(), value))
            .collect()
    }

    /// The tool's non-default arguments, with any values that must not reach a
    /// span/log context filtered out or replaced (e.g. `write` drops its
    /// `content`). Every tool implements it so redaction policy sits next to
    /// its argument definitions and stays explicit.
    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)>;

    /// Argument keys whose value is a file-system path or glob pattern that
    /// should be normalized to a cwd-relative form before dispatch, when the
    /// supplied value is an absolute path that falls inside the current
    /// working directory (see [`ToolRegistry::create_call`]). Defaults to
    /// none; tools that accept path-like arguments override this so the
    /// registry never has to guess based on argument names.
    fn path_like_arguments(&self) -> &'static [&'static str] {
        &[]
    }

    /// Default execution timeout (seconds) for this tool.
    fn timeout_secs(&self) -> u64;

    /// Create a [`ToolCall`] invocation object for a specific call, binding
    /// the call ID and arguments to the tool's execution logic.
    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall>;
}

/// Central store of every tool the agent may invoke.
///
/// [`ToolRegistry::schemas`] returns every registered schema. Agent
/// construction restricts the registry to the tools allowed by role and type.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Create a [`ToolCall`] for the named tool, or `None` if the tool is not
    /// registered.
    ///
    /// Before dispatch, rewrites each of the tool's
    /// [`Tool::path_like_arguments`] string values in two steps: first the
    /// value's `~`/`$ENVVAR` tokens are expanded (see
    /// [`crate::sandbox::expand_path_tokens`]), then an absolute result that
    /// falls inside the current working directory is rewritten to its
    /// cwd-relative form (see [`crate::sandbox::to_relative_path`]). Some
    /// LLMs emit absolute paths — including verbatim sentinel sources such as
    /// `$YLVA_HOME/AGENTS.md` — even where a relative one would do;
    /// normalizing here means the expanded path is both what actually
    /// executes and what the UI renders for the call, rather than just
    /// cosmetic. A value that expands to a path outside the current
    /// directory, and an already-relative value, keep the expanded form
    /// unchanged — this is not a security boundary, just a
    /// display/consistency nicety; sandbox approval checks run independently
    /// on whatever expanded path is used. Normalization is keyed off the
    /// process's actual cwd rather than a sandbox's configured workspace
    /// root, because that is what relative paths in tool execution (e.g.
    /// `std::fs::read`) resolve against.
    pub fn create_call(
        &self,
        name: &str,
        id: uuid::Uuid,
        mut arguments: serde_json::Value,
    ) -> Option<Box<dyn ToolCall>> {
        let tool = self.tools.get(name)?;
        if let Some(object) = arguments.as_object_mut() {
            if let Ok(current_dir) = std::env::current_dir() {
                for key in tool.path_like_arguments() {
                    let path_str = match object.get(*key) {
                        Some(Value::String(path_str)) => path_str.clone(),
                        _ => continue,
                    };
                    let expanded =
                        crate::sandbox::expand_path_tokens(&path_str, &|variable_name| {
                            std::env::var(variable_name).ok()
                        });
                    let normalized = crate::sandbox::to_relative_path(&expanded, &current_dir)
                        .unwrap_or(expanded);
                    if normalized != path_str {
                        object.insert((*key).to_string(), Value::String(normalized));
                    }
                }
            }
        }
        Some(tool.create_call(id, arguments))
    }

    /// The OpenAI wire schemas for every registered tool (the `tools` array).
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|tool| to_tool_schema(tool.as_ref()))
            .collect()
    }
}

/// Wrap a tool's `schemars` root schema into the OpenAI function-tool envelope.
fn to_tool_schema(tool: &dyn Tool) -> ToolSchema {
    let parameters =
        serde_json::to_value(tool.input_schema()).unwrap_or_else(|_| serde_json::json!({}));
    ToolSchema {
        name: tool.name().to_string(),
        description: format!(
            "{}\n\nAcross all tools combined, a single assistant response may contain at most {} distinct tool calls. Keep each batch within this limit; if more work is needed, continue in a later response.",
            tool.description(),
            crate::agent::MAX_TOOL_CALLS_PER_RESPONSE,
        ),
        parameters,
    }
}

/// Validates `arguments` against the compiled JSON Schema derived from `schema`.
///
/// This is the building block used by the [`Tool::validate_args`] default implementation.
/// Tool overrides that add cross-field constraints should call this first, then layer
/// their own checks on top.
pub(super) fn validate_args_with_schema(
    schema: &RootSchema,
    arguments: &Value,
) -> Result<(), String> {
    let schema_value = serde_json::to_value(schema)
        .map_err(|error| format!("failed to serialize schema: {error}"))?;
    let compiled = jsonschema::JSONSchema::compile(&schema_value)
        .map_err(|error| format!("invalid schema: {error}"))?;
    if let Err(errors) = compiled.validate(arguments) {
        let messages: Vec<String> = errors.map(|error| error.to_string()).collect();
        return Err(messages.join("; "));
    }
    Ok(())
}

/// Names of the tools available in a CLI (headless) context.
pub const DEFAULT_CLI_TOOLS: &[&str] = &[
    load_skill::LoadSkillTool::NAME,
    read::ReadTool::NAME,
    write::WriteTool::NAME,
    glob::GlobTool::NAME,
    bash::BashTool::NAME,
    search::SearchTool::NAME,
    edit::EditTool::NAME,
    web_search::WebSearchTool::NAME,
    web_fetch::WebFetchTool::NAME,
];

/// Names of the tools available in a TUI (interactive) context.
/// Superset of [`DEFAULT_CLI_TOOLS`]; adds tools that require a live terminal.
pub const DEFAULT_TUI_TOOLS: &[&str] = &[
    load_skill::LoadSkillTool::NAME,
    read::ReadTool::NAME,
    write::WriteTool::NAME,
    glob::GlobTool::NAME,
    bash::BashTool::NAME,
    search::SearchTool::NAME,
    edit::EditTool::NAME,
    clarify::ClarifyTool::NAME,
    web_search::WebSearchTool::NAME,
    web_fetch::WebFetchTool::NAME,
];

/// One tool whose approval prompt can be bypassed via a `--auto-approve-*`
/// CLI flag or [`ToolRegistryConfig::auto_approve`] entry. `clarify` has no
/// variant here: it always requires user approval (there is no sandbox or
/// bypass concept for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoApproveTool {
    Read,
    Write,
    Glob,
    Bash,
    Search,
    Edit,
    WebSearch,
    WebFetch,
    Spawn,
}

/// Configuration for [`default_registry`], controlling per-tool approval policies
/// and the explicit set of tools to register.
///
/// Use [`Default`] for the CLI (headless) tool set ([`DEFAULT_CLI_TOOLS`]) or construct
/// with a custom `allowed_tools` list. Per-agent restrictions pass a subset of
/// [`DEFAULT_CLI_TOOLS`] or [`DEFAULT_TUI_TOOLS`] explicitly.
#[derive(Debug)]
pub struct ToolRegistryConfig {
    /// Immutable, policy-filtered skills for this agent. Empty means no loader.
    pub skills: Arc<crate::skills::policy::CallableSkills>,
    /// The set of tools whose approval prompt is bypassed (auto-approved)
    /// without a user prompt. Intended for automated / e2e testing where no
    /// human is present to approve.
    pub auto_approve: std::collections::HashSet<AutoApproveTool>,
    /// Explicit list of tool names to register. Only tools whose name appears
    /// here are added to the registry; all others are skipped regardless of
    /// whether they exist in the candidate pool.
    pub allowed_tools: Vec<String>,
    /// When `true`, registers `spawn_subagent`. Set only for `Role::Root`
    /// agents whose `AgentConfig::can_spawn` is `true`; structurally absent
    /// otherwise (never gated by `allowed_tools`).
    pub include_spawn_subagent: bool,
    /// Agent types and model-facing descriptions advertised by
    /// `spawn_subagent`, when that tool is registered.
    pub spawn_agent_types: Vec<spawn_subagent::AgentTypeInfo>,
    /// Present only for `Role::Child` agents: registers `agent_finish`, wired to
    /// send its result through this config's channel/ids.
    pub agent_finish: Option<agent_finish::AgentFinishToolConfig>,
    /// Per-tool sandbox rule lists parsed from the profile's `[tools.<name>]`
    /// tables (`ProfileConfig::tools`), keyed by tool name. A tool name with
    /// no entry here falls back to `SandboxContext`'s built-in default floor
    /// (`outside_workspace`/`dot_path` → `UserApprovalRequired`); an entry
    /// fully replaces that floor for its tool. See `docs/configuration.md`'s
    /// "Sandbox" section.
    pub tool_sandbox_rules: HashMap<String, Vec<crate::sandbox::SandboxRuleConfig>>,
    /// The profile's `[web_search]` section: which backend serves
    /// `web_search`, its internal HTTP timeout, and its default result
    /// limit.
    pub web_search: crate::config::WebSearchConfig,
    /// The profile's `[web_fetch]` section: which backend serves
    /// `web_fetch` and its internal HTTP timeout.
    pub web_fetch: crate::config::WebFetchConfig,
}

impl Default for ToolRegistryConfig {
    /// Defaults to [`DEFAULT_CLI_TOOLS`] (the standard headless tool set).
    fn default() -> Self {
        Self {
            skills: Arc::default(),
            auto_approve: std::collections::HashSet::new(),
            allowed_tools: DEFAULT_CLI_TOOLS.iter().map(|s| s.to_string()).collect(),
            include_spawn_subagent: false,
            spawn_agent_types: Vec::new(),
            agent_finish: None,
            tool_sandbox_rules: HashMap::new(),
            web_search: crate::config::WebSearchConfig::default(),
            web_fetch: crate::config::WebFetchConfig::default(),
        }
    }
}

/// Build the default tool registry configured by [`ToolRegistryConfig`].
///
/// Only tools whose name appears in `config.allowed_tools` are registered.
pub fn default_registry(
    sandbox: std::sync::Arc<std::sync::RwLock<crate::sandbox::Sandbox>>,
    config: ToolRegistryConfig,
) -> ToolRegistry {
    use crate::sandbox::SandboxContext;

    let allowed = &config.allowed_tools;
    let mut registry = ToolRegistry::new();

    // Builds a tool's SandboxContext, replacing the built-in default floor
    // with `config.tool_sandbox_rules`'s entry for `tool_name` (a profile's
    // `[tools.<name>]` table) when one is configured; otherwise the default
    // floor (`outside_workspace`/`dot_path` → `UserApprovalRequired`) applies.
    // When
    // `auto_approve_tool` is set in `config.auto_approve`, a catch-all
    // `AutoApproved` rule is appended last, so it always wins regardless of
    // `[tools.<name>]` config.
    let sandbox_context_for = |tool_name: &str, auto_approve_tool: AutoApproveTool| {
        let mut context = SandboxContext::new(Arc::clone(&sandbox));
        if let Some(rules) = config.tool_sandbox_rules.get(tool_name) {
            context = context.with_tool_rules(tool_name, rules.clone());
        }
        if tool_name == read::ReadTool::NAME {
            context = context.with_skill_sources(Arc::new(
                config
                    .skills
                    .skills
                    .values()
                    .map(|entry| entry.skill.source.canonical_path.clone())
                    .collect(),
            ));
        }
        if config.auto_approve.contains(&auto_approve_tool) {
            context = context.with_rule(crate::sandbox::SandboxRule {
                verdict: crate::sandbox::ApprovalCheck::AutoApproved,
                pattern: ::glob::Pattern::new("**").expect("** is a valid glob"),
            });
        }
        context
    };

    // Registers `name` only when present in `allowed`, wiring its sandbox
    // context via `sandbox_context_for` and building the tool with `make`.
    let mut register_sandboxed_tool =
        |name: &'static str,
         auto_approve_tool: AutoApproveTool,
         make: &dyn Fn(SandboxContext) -> Arc<dyn Tool>| {
            if allowed.iter().any(|allowed_name| allowed_name == name) {
                registry.register(make(sandbox_context_for(name, auto_approve_tool)));
            }
        };

    register_sandboxed_tool(read::ReadTool::NAME, AutoApproveTool::Read, &|context| {
        Arc::new(read::ReadTool::new(context))
    });
    register_sandboxed_tool(write::WriteTool::NAME, AutoApproveTool::Write, &|context| {
        Arc::new(write::WriteTool::new(context))
    });
    register_sandboxed_tool(glob::GlobTool::NAME, AutoApproveTool::Glob, &|context| {
        Arc::new(glob::GlobTool::new(context))
    });
    register_sandboxed_tool(bash::BashTool::NAME, AutoApproveTool::Bash, &|context| {
        Arc::new(bash::BashTool::new(context))
    });
    register_sandboxed_tool(
        search::SearchTool::NAME,
        AutoApproveTool::Search,
        &|context| Arc::new(search::SearchTool::new(context)),
    );
    register_sandboxed_tool(edit::EditTool::NAME, AutoApproveTool::Edit, &|context| {
        Arc::new(edit::EditTool::new(context))
    });

    if allowed
        .iter()
        .any(|name| name == load_skill::LoadSkillTool::NAME)
        && !config.skills.skills.is_empty()
    {
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_tool_rules(
            load_skill::LoadSkillTool::NAME,
            config
                .tool_sandbox_rules
                .get(load_skill::LoadSkillTool::NAME)
                .cloned()
                .unwrap_or_default(),
        );
        registry.register(Arc::new(load_skill::LoadSkillTool::new(
            config.skills.clone(),
            context,
        )));
    }

    // `web_search` has no path or command argument, but it still resolves a
    // sandbox context: that context carries its `[tools.web_search]`
    // `network_request` rules and the `--auto-approve-web-search`
    // catch-all (see `SandboxContext::check_network_request`).
    //
    // Backend construction is fallible but never fails here: profile load
    // validated `[web_search]` first, so a construction error would be an
    // invariant violation. Panicking on it keeps a misconfigured backend
    // loud instead of silently substituting another one.
    if allowed
        .iter()
        .any(|name| name == web_search::WebSearchTool::NAME)
    {
        let web_search_backend = config
            .web_search
            .build_backend()
            .expect("web search backend builds from a profile config validated at load");
        registry.register(Arc::new(web_search::WebSearchTool::new(
            web_search_backend,
            &config.web_search,
            sandbox_context_for(web_search::WebSearchTool::NAME, AutoApproveTool::WebSearch),
        )));
    }

    // `web_fetch` has no path or command argument, but it still resolves a
    // sandbox context: that context carries its `[tools.web_fetch]`
    // `network_request` rules and the `--auto-approve-web-fetch`
    // catch-all (see `SandboxContext::check_network_request`).
    if allowed
        .iter()
        .any(|name| name == web_fetch::WebFetchTool::NAME)
    {
        registry.register(Arc::new(web_fetch::WebFetchTool::new(
            config.web_fetch.build_backend(),
            &config.web_fetch,
            sandbox_context_for(web_fetch::WebFetchTool::NAME, AutoApproveTool::WebFetch),
        )));
    }

    if allowed
        .iter()
        .any(|name| name == clarify::ClarifyTool::NAME)
    {
        registry.register(Arc::new(clarify::ClarifyTool));
    }
    // Neither `spawn_subagent` nor `agent_finish` is gated by `allowed_tools`: the
    // former is structural (present only when the agent can spawn children),
    // the latter is injected only into `Role::Child` registries.
    if config.include_spawn_subagent {
        registry.register(Arc::new(
            spawn_subagent::SpawnSubagentTool::new(
                config.auto_approve.contains(&AutoApproveTool::Spawn),
            )
            .with_agent_types(config.spawn_agent_types),
        ));
    }
    if let Some(agent_finish_config) = config.agent_finish.clone() {
        registry.register(Arc::new(agent_finish::AgentFinishTool::new(
            agent_finish_config,
        )));
    }

    registry
}

#[cfg(test)]
mod tests;
