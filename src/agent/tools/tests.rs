use super::*;
use schemars::schema_for;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, JsonSchema, Deserialize)]
#[expect(dead_code)]
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
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
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
    fn path_like_arguments(&self) -> &'static [&'static str] {
        &["field"]
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

// `TestTool` does not override `title` or `is_default_argument`, so these
// tests exercise the default trait method bodies.

mod registry;
mod sections;
mod titles;
