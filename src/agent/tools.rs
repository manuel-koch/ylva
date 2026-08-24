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
//! hand-written JSON (blueprint §4D). The registry keeps a runtime `allowed`
//! seam so a future Phase 6 whitelist (or Phase 5 headless filtering) can
//! restrict the `tools` array without changing the agent loop.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::schema::RootSchema;
use serde_json::Value;

use crate::llm::ToolSchema;

pub mod fs;

/// Default execution timeout for file-system tools (blueprint §4D: 5s).
pub const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Failure from a single tool invocation, mapped to the flat
/// `String`-returning [`crate::agent::PendingTool::execute`] contract and to
/// `UiUpdate::ToolError` / `Message::ToolError`.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool '{name}' failed: {message}")]
    Failed { name: String, message: String },
}

impl ToolError {
    pub fn failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        ToolError::Failed {
            name: name.into(),
            message: message.into(),
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
    fn text(&self) -> &str;

    /// Key/value domain properties of the result (e.g. `("bytes_written",
    /// "42")`).  Returned as an ordered vec so callers can present them in a
    /// stable, meaningful sequence.  Defaults to an empty vec for tools whose
    /// output is fully captured by [`text`](Self::text).
    fn properties(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Rich content sections produced by this result (e.g. file text, command
    /// output). Computed once at execution time.
    fn content_sections(&self) -> Vec<ContentSection> {
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
    fn pending_content_sections(&self) -> Vec<ContentSection> {
        vec![]
    }

    /// Schema-defined display rank for `key`. Lower ranks appear earlier in
    /// invocation summaries; the lowest-ranked argument is the primary one.
    /// Defaults to `usize::MAX` (sort last — preserves LLM-supplied key order).
    fn argument_order(&self, _key: &str) -> usize {
        usize::MAX
    }

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

/// A named, schema-described tool the model may call (blueprint §4D).
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
    /// Defaults to [`name`](Self::name); a tool author overrides it when the
    /// canonical name is not a friendly label.
    fn title(&self) -> &str {
        self.name()
    }

    /// A human-readable description fed to the model alongside the schema.
    fn description(&self) -> &str;

    /// The `schemars`-derived parameter schema for the tool's arguments.
    fn schema(&self) -> RootSchema;

    /// Whether this tool requires explicit approval. The loop treats it only
    /// as a driver hint; approval policy lives at the driver/tool boundary.
    ///
    /// Defaults to `true` (**safe-by-default**): any tool requires approval
    /// unless it explicitly opts out. Read-only tools override to `false` —
    /// an interim/fake marker pending Phase 6 path-based approval (Q4/Q5).
    fn needs_approval(&self) -> bool {
        true
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
    fn argument_order(&self, key: &str) -> usize {
        let schema = self.schema();
        let properties = schema.schema.object.map(|object| object.properties);
        match properties {
            Some(properties) => properties
                .keys()
                .position(|name| name.as_str() == key)
                .unwrap_or(properties.len()),
            None => 0,
        }
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
    let parameters = serde_json::to_value(tool.schema()).unwrap_or_else(|_| serde_json::json!({}));
    ToolSchema {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        parameters,
    }
}

/// Convenience: build the Phase 3 registry with the three file-system tools.
pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(fs::ReadTool::default()));
    registry.register(Arc::new(fs::WriteTool::default()));
    registry.register(Arc::new(fs::GlobTool::default()));
    registry
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
        fn text(&self) -> &str {
            &self.0
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
        fn schema(&self) -> RootSchema {
            schema_for!(TestArgs)
        }
        fn needs_approval(&self) -> bool {
            false
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
    fn default_registry_registers_all_three_tools() {
        let registry = default_registry();
        assert_eq!(registry.names().len(), 3);
        assert!(registry.get("read").is_some());
        assert!(registry.get("write").is_some());
        assert!(registry.get("glob").is_some());
    }
}
