//! Tool trait, registry, and the built-in file-system tools.
//!
//! Schemas are derived from typed argument structs via `schemars`, never
//! hand-written JSON (blueprint §4D). The registry keeps a runtime `allowed`
//! seam so a future Phase 6 whitelist (or Phase 5 headless filtering) can
//! restrict the `tools` array without changing the agent loop.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
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

/// A named, schema-described, async tool the model may call (blueprint §4D).
///
/// The trait exposes tool **data**, behavior, and the argument display
/// metadata a host needs (which arguments are defaulted, and their display
/// order). Actual rendering (brackets, eliding long values) stays the host's
/// decision.
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

    /// Default execution timeout (seconds) for this tool.
    fn timeout_secs(&self) -> u64;

    /// Execute the tool with parsed `arguments`.
    fn run<'a>(&'a self, arguments: Value) -> BoxFuture<'a, Result<String, ToolError>>;
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
        fn run<'a>(&'a self, _arguments: Value) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move { Ok("ok".to_string()) })
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
