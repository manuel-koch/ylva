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
//! hand-written JSON. The registry keeps a runtime `allowed` seam so a future
//! whitelist or headless filtering can restrict the `tools` array without
//! changing the agent loop.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::schema::RootSchema;
use serde_json::Value;

use crate::llm::ToolSchema;
use crate::sandbox::ApprovalCheck;

pub mod bash;
pub mod clarify;
pub mod edit;
pub mod fs;
pub mod search;

/// Default execution timeout for file-system tools.
pub const DEFAULT_TIMEOUT_SECS: u64 = 5;

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
    /// The text returned to the LLM as the tool's result message.
    fn text(&self) -> String;

    /// Structured metadata for this result.
    ///
    /// All keys are rendered as `key=value` in the TUI/CLI tool bubble and
    /// embedded in the `result_metadata` field of the tool-response envelope.
    ///
    /// Returns `None` for tools whose text already captures everything relevant.
    fn metadata(&self) -> Option<serde_json::Value> {
        None
    }

    /// JSON Schema describing the fields that are actually present in
    /// `metadata()` for this specific result instance.
    ///
    /// Return `None` (the default) when `metadata()` is `None`. Implementations
    /// that return metadata should return `Some(schema_for!(TheirMetadataStruct))`.
    /// Only fields emitted for this instance need to appear — conditional fields
    /// may be omitted by returning a narrower schema.
    fn metadata_schema(&self) -> Option<RootSchema> {
        None
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSection {
    /// Optional label (e.g. the file path being patched).
    pub label: Option<String>,
    /// Unified diff text.
    pub content: String,
}

/// A single line in a search result — either a match line or a context line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultLine {
    /// 1-based line number within the source file.
    pub line_number: usize,
    /// `true` for a match line, `false` for a context line.
    pub is_match: bool,
    /// Raw line content with no path/line-number prefix.
    pub content: String,
}

/// A block of search results from a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Returns `true` when `key` names an argument whose value belongs in a
    /// content section rather than an inline invocation summary.
    fn is_body_argument(&self, _key: &str) -> bool {
        false
    }

    /// Content sections produced before execution completes (e.g. argument
    /// preview). Defaults to empty.
    fn pending_content_sections(&self) -> Vec<Section> {
        vec![]
    }

    /// Schema-defined display rank for `key`. Lower ranks appear earlier in
    /// invocation summaries; the lowest-ranked argument is the primary one.
    /// Implementations must delegate to their corresponding [`Tool::argument_order`].
    fn argument_order(&self, key: &str) -> usize;

    /// Non-content-section, non-null arguments sorted by schema-defined rank.
    fn title_arguments(&self) -> Vec<(String, serde_json::Value)> {
        let mut args: Vec<(String, serde_json::Value)> = self
            .arguments()
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(k, v)| !v.is_null() && !self.is_body_argument(k))
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
    /// Derived automatically from [`name`](Self::name) by splitting on `_`
    /// and `-`, capitalising the first letter of each word, and joining with
    /// spaces.  Override only when the canonical name cannot produce the
    /// desired label.
    fn title(&self) -> String {
        self.name()
            .split(['-', '_'])
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
    /// override for optional arguments (e.g. `read`'s `mode: bytes`,
    /// `offset: 0`/`null`, `limit: null`).
    fn is_default_argument(&self, _key: &str, _value: &Value) -> bool {
        false
    }

    /// Display-order rank for an argument key: lower ranks are listed earlier
    /// in a host's invocation summary, and the lowest-ranked argument is the
    /// primary (inline) one. Ranks are derived from the tool's schema property
    /// order, which reflects the argument **struct's declaration order** (via
    /// schemars `preserve_order`), so no per-tool ordering needs to be
    /// hand-written. Unknown keys sort last.
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

    /// Default execution timeout (seconds) for this tool.
    fn timeout_secs(&self) -> u64;

    /// Create a [`ToolCall`] invocation object for a specific call, binding
    /// the call ID and arguments to the tool's execution logic.
    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall>;
}

/// Central store of every tool the agent may invoke.
///
/// Dynamic filtering (§4D) is a seam: [`ToolRegistry::schemas`] returns every
/// registered schema today; a future `allowed_tools` filter slots in without
/// changing the agent loop.
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
    pub fn create_call(
        &self,
        name: &str,
        id: uuid::Uuid,
        arguments: serde_json::Value,
    ) -> Option<Box<dyn ToolCall>> {
        self.tools
            .get(name)
            .map(|tool| tool.create_call(id, arguments))
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
        description: tool.description().to_string(),
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
    fs::ReadTool::NAME,
    fs::WriteTool::NAME,
    fs::GlobTool::NAME,
    bash::BashTool::NAME,
    search::SearchTool::NAME,
    edit::EditTool::NAME,
];

/// Names of the tools available in a TUI (interactive) context.
/// Superset of [`DEFAULT_CLI_TOOLS`]; adds tools that require a live terminal.
pub const DEFAULT_TUI_TOOLS: &[&str] = &[
    fs::ReadTool::NAME,
    fs::WriteTool::NAME,
    fs::GlobTool::NAME,
    bash::BashTool::NAME,
    search::SearchTool::NAME,
    edit::EditTool::NAME,
    clarify::ClarifyTool::NAME,
];

/// Configuration for [`default_registry`], controlling per-tool approval policies
/// and the explicit set of tools to register.
///
/// Use [`Default`] for the CLI (headless) tool set ([`DEFAULT_CLI_TOOLS`]) or construct
/// with a custom `allowed_tools` list. Per-agent restrictions pass a subset of
/// [`DEFAULT_CLI_TOOLS`] or [`DEFAULT_TUI_TOOLS`] explicitly.
#[derive(Debug)]
pub struct ToolRegistryConfig {
    /// When `true`, `edit` tool calls are auto-approved without a user prompt.
    /// Intended for automated / e2e testing where no human is present to approve.
    pub auto_approve_edit: bool,
    /// When `true`, `write` tool calls are auto-approved without a user prompt.
    /// Intended for automated / e2e testing where no human is present to approve.
    pub auto_approve_write: bool,
    /// Explicit list of tool names to register. Only tools whose name appears
    /// here are added to the registry; all others are skipped regardless of
    /// whether they exist in the candidate pool.
    pub allowed_tools: Vec<String>,
}

impl Default for ToolRegistryConfig {
    /// Defaults to [`DEFAULT_CLI_TOOLS`] (the six standard headless tools).
    fn default() -> Self {
        Self {
            auto_approve_edit: false,
            auto_approve_write: false,
            allowed_tools: DEFAULT_CLI_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Build the default tool registry configured by [`ToolRegistryConfig`].
///
/// The full candidate pool is: `read`, `write`, `glob`, `bash`, `search`, `edit`, `clarify`.
/// Only tools whose name appears in `config.allowed_tools` are registered.
pub fn default_registry(
    sandbox: std::sync::Arc<std::sync::RwLock<crate::sandbox::Sandbox>>,
    config: ToolRegistryConfig,
) -> ToolRegistry {
    use crate::sandbox::SandboxContext;

    let allowed = &config.allowed_tools;
    let mut registry = ToolRegistry::new();

    if allowed.iter().any(|name| name == fs::ReadTool::NAME) {
        registry.register(Arc::new(fs::ReadTool::new(SandboxContext::new(
            Arc::clone(&sandbox),
        ))));
    }
    if allowed.iter().any(|name| name == fs::WriteTool::NAME) {
        registry.register(Arc::new(fs::WriteTool::new({
            let mut context = SandboxContext::new(Arc::clone(&sandbox));
            if config.auto_approve_write {
                context = context.with_rule(crate::sandbox::SandboxRule {
                    verdict: crate::sandbox::ApprovalCheck::AutoApproved,
                    pattern: glob::Pattern::new("**").expect("** is a valid glob"),
                });
            }
            context
        })));
    }
    if allowed.iter().any(|name| name == fs::GlobTool::NAME) {
        registry.register(Arc::new(fs::GlobTool::new(SandboxContext::new(
            Arc::clone(&sandbox),
        ))));
    }
    if allowed.iter().any(|name| name == bash::BashTool::NAME) {
        registry.register(Arc::new(bash::BashTool::new(SandboxContext::new(
            Arc::clone(&sandbox),
        ))));
    }
    if allowed.iter().any(|name| name == search::SearchTool::NAME) {
        registry.register(Arc::new(search::SearchTool::new(SandboxContext::new(
            Arc::clone(&sandbox),
        ))));
    }
    if allowed.iter().any(|name| name == edit::EditTool::NAME) {
        let edit_tool = {
            let mut context = SandboxContext::new(Arc::clone(&sandbox));
            if config.auto_approve_edit {
                context = context.with_rule(crate::sandbox::SandboxRule {
                    verdict: crate::sandbox::ApprovalCheck::AutoApproved,
                    pattern: glob::Pattern::new("**").expect("** is a valid glob"),
                });
            }
            edit::EditTool::new(context)
        };
        registry.register(Arc::new(edit_tool));
    }
    if allowed
        .iter()
        .any(|name| name == clarify::ClarifyTool::NAME)
    {
        registry.register(Arc::new(clarify::ClarifyTool));
    }

    registry
}

/// Asserts that every property in the top-level `properties` object of `schema`
/// carries a non-empty `description` string.
///
/// Call this from each tool's input-args schema test to guard against fields that
/// the LLM would see without any explanation.
#[cfg(test)]
pub(crate) fn assert_all_properties_have_descriptions(
    schema: &schemars::schema::RootSchema,
    context: &str,
) {
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{context}: schema has no properties object"));
    for (field_name, property) in properties {
        // $ref properties delegate to a definition — the description lives there.
        if property.get("$ref").is_some() {
            continue;
        }
        let description = property
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        assert!(
            !description.is_empty(),
            "{context}: field `{field_name}` is missing a doc-comment description in the schema"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::schema_for;
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(Debug, JsonSchema, Deserialize)]
    #[allow(dead_code)]
    struct TestArgs {
        field: String,
    }

    #[derive(Debug)]
    struct SimpleOutput(String);

    impl ToolCallResult for SimpleOutput {
        fn text(&self) -> String {
            self.0.clone()
        }
    }

    struct TestToolCall {
        id: uuid::Uuid,
        arguments: serde_json::Value,
        timeout_secs: u64,
    }

    impl ToolCall for TestToolCall {
        fn id(&self) -> uuid::Uuid {
            self.id
        }
        fn name(&self) -> &str {
            "test"
        }
        fn arguments(&self) -> &serde_json::Value {
            &self.arguments
        }
        fn timeout_secs(&self) -> u64 {
            self.timeout_secs
        }
        fn argument_order(&self, _key: &str) -> usize {
            usize::MAX
        }
        fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
            self.arguments
                .as_object()
                .into_iter()
                .flat_map(|map| map.iter())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }
        fn run(
            self: Box<Self>,
        ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>>
        {
            Box::pin(async move {
                let output: Box<dyn ToolCallResult> = Box::new(SimpleOutput("ok".to_string()));
                Ok(output)
            })
        }
    }

    #[derive(Debug, Default)]
    struct TestTool;

    impl Tool for TestTool {
        fn name(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "a test tool"
        }
        fn input_schema(&self) -> RootSchema {
            schema_for!(TestArgs)
        }
        fn needs_approval(&self, _arguments: &Value) -> ApprovalCheck {
            ApprovalCheck::AutoApproved
        }
        fn timeout_secs(&self) -> u64 {
            5
        }
        fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
            self.non_default_arguments(arguments)
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect()
        }
        fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
            Box::new(TestToolCall {
                id,
                arguments,
                timeout_secs: self.timeout_secs(),
            })
        }
    }

    #[test]
    fn registry_register_lookup_and_unknown_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TestTool));
        assert!(registry.get("test").is_some());
        assert!(registry.get("missing").is_none());
        assert_eq!(registry.names(), vec!["test".to_string()]);
    }

    #[test]
    fn registry_schema_is_tool_schema_envelope() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TestTool));
        let schemas = registry.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "test");
        assert_eq!(schemas[0].description, "a test tool");
        assert_eq!(
            schemas[0].parameters["required"],
            serde_json::json!(["field"])
        );
    }

    #[test]
    fn default_registry_registers_cli_tools() {
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
        let registry = default_registry(sandbox, ToolRegistryConfig::default());
        assert_eq!(registry.names().len(), 6);
        assert!(registry.get(fs::ReadTool::NAME).is_some());
        assert!(registry.get(fs::WriteTool::NAME).is_some());
        assert!(registry.get(fs::GlobTool::NAME).is_some());
        assert!(registry.get(bash::BashTool::NAME).is_some());
        assert!(registry.get(search::SearchTool::NAME).is_some());
        assert!(registry.get(edit::EditTool::NAME).is_some());
        assert!(registry.get(clarify::ClarifyTool::NAME).is_none());
    }

    #[test]
    fn default_registry_registers_tui_tools_including_clarify() {
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
        let registry = default_registry(
            sandbox,
            ToolRegistryConfig {
                allowed_tools: DEFAULT_TUI_TOOLS.iter().map(|s| s.to_string()).collect(),
                ..ToolRegistryConfig::default()
            },
        );
        assert_eq!(registry.names().len(), 7);
        assert!(registry.get(clarify::ClarifyTool::NAME).is_some());
    }

    #[test]
    fn default_registry_respects_explicit_allowed_tools_subset() {
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new()));
        let registry = default_registry(
            sandbox,
            ToolRegistryConfig {
                allowed_tools: vec![
                    bash::BashTool::NAME.to_string(),
                    fs::ReadTool::NAME.to_string(),
                ],
                ..ToolRegistryConfig::default()
            },
        );
        assert_eq!(registry.names().len(), 2);
        assert!(registry.get(bash::BashTool::NAME).is_some());
        assert!(registry.get(fs::ReadTool::NAME).is_some());
        assert!(registry.get(edit::EditTool::NAME).is_none());
    }

    #[test]
    fn write_tool_always_needs_user_approval() {
        let write_tool = fs::WriteTool::default();
        let args = serde_json::json!({ "path": "src/main.rs", "content": "hello" });
        assert!(matches!(
            write_tool.needs_approval(&args),
            ApprovalCheck::UserApprovalRequired
        ));
    }

    #[test]
    fn section_content_roundtrip() {
        let section = Section::Content(ContentSection {
            label: None,
            content: "hello".to_string(),
            file_extension: Some("rs".to_string()),
            line_number_offset: Some(0),
        });
        assert!(matches!(section, Section::Content(_)));
    }

    #[test]
    fn section_diff_variant() {
        let section = Section::Diff(DiffSection {
            label: Some("file.rs".to_string()),
            content: "+hello\n-world".to_string(),
        });
        assert!(matches!(section, Section::Diff(_)));
    }

    #[test]
    fn section_search_result_variant() {
        let section = Section::SearchResult(SearchResultSection {
            file_path: "file.rs".to_string(),
            file_extension: Some("rs".to_string()),
            lines: vec![SearchResultLine {
                line_number: 42,
                is_match: true,
                content: "fn main() {}".to_string(),
            }],
        });
        assert!(matches!(section, Section::SearchResult(_)));
    }

    #[test]
    fn tool_call_result_default_metadata_is_none() {
        let output = SimpleOutput("test".to_string());
        assert!(output.metadata().is_none());
        assert!(output.content_sections().is_empty());
    }

    #[test]
    fn tool_call_default_pending_sections_and_argument_order() {
        let call = TestToolCall {
            id: uuid::Uuid::new_v4(),
            arguments: serde_json::json!({ "field": "val" }),
            timeout_secs: 1,
        };
        assert!(call.pending_content_sections().is_empty());
        assert_eq!(call.argument_order("any_key"), usize::MAX);
    }

    // `TestTool` does not override `title` or `is_default_argument`, so these
    // tests exercise the default trait method bodies.
    #[test]
    fn tool_default_title_capitalises_name_words() {
        let tool = TestTool;
        // single word: first letter capitalised
        assert_eq!(tool.title(), "Test");
    }

    #[test]
    fn tool_title_splits_on_hyphen_and_underscore() {
        // Verify the derivation logic directly.
        fn derive(name: &str) -> String {
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
        assert_eq!(derive("web_search"), "Web Search");
        assert_eq!(derive("my-cool-tool"), "My Cool Tool");
        assert_eq!(derive("compile"), "Compile");
        assert_eq!(derive("deploy"), "Deploy");
    }

    #[test]
    fn tool_default_is_default_argument_returns_false() {
        let tool = TestTool;
        assert!(!tool.is_default_argument("field", &serde_json::json!("value")));
    }
}
