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

//! Headless (non-terminal) runner: the original `println!`-based host for
//! `AgentLoop`. This is the path used when stdout is not a terminal.

use std::io::Write;
use std::sync::Arc;

use crate::agent::{AgentCommand, AgentLoop, UiUpdate};
use crate::llm::{serialize_messages, Provider};
use crate::{Tool, ToolRegistry};

/// Length beyond which a string value is treated as "long" and elided when
/// rendering a tool invocation.
const STRING_ELIDE_LIMIT: usize = 40;

/// Run the agent loop headless, printing to stdout.
pub async fn run_headless(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
) -> anyhow::Result<()> {
    run_headless_to(provider, registry, initial_prompt, &mut std::io::stdout()).await
}

/// [`run_headless`] with an explicit output writer (used by tests to capture
/// the emitted lines without redirecting process stdout).
pub async fn run_headless_to(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let (mut agent, mut ui_rx) = AgentLoop::new(provider, Arc::clone(&registry));

    // Echo the user's turn before sending the request, so it appears even
    // while the provider is still connecting / retrying (the "Assistant:"
    // prefix is printed when the stream actually starts).
    writeln!(out, "---[ User ]---")?;
    writeln!(out, "{initial_prompt}")?;
    agent
        .send_command(AgentCommand::Prompt(initial_prompt))
        .await;
    let handle = agent.run();

    let mut response_finished = false;
    while !response_finished {
        match ui_rx.recv().await {
            Some(UiUpdate::StreamStarted) => {
                writeln!(out, "\n---[ Assistant ]---")?;
            }
            Some(UiUpdate::AppendText(text)) => {
                write!(out, "{text}")?;
                out.flush()?;
            }
            Some(UiUpdate::ToolPending { name, id, .. }) => {
                // CLI-only host, no interactive approval. read/glob are
                // auto-approved via the normal protocol; a write request is
                // denied and fed back to the model as a ToolError.
                let title = registry
                    .get(&name)
                    .map(|tool| tool.title().to_string())
                    .unwrap_or_else(|| name.clone());
                let needs_approval = registry
                    .get(&name)
                    .is_some_and(|tool| tool.needs_approval());
                if needs_approval {
                    writeln!(
                        out,
                        "[tool] {title} denied (no --auto-approve-write in Phase 3)"
                    )?;
                    agent.send_command(AgentCommand::DenyTool { id }).await;
                } else {
                    writeln!(out, "[tool] {title} auto-approved")?;
                    agent.send_command(AgentCommand::ApproveTool { id }).await;
                }
            }
            Some(UiUpdate::ToolStart {
                name, arguments, ..
            }) => {
                let invocation = registry
                    .get(&name)
                    .map(|tool| render_tool_invocation(tool.as_ref(), &arguments))
                    .unwrap_or_else(|| name.clone());
                writeln!(out, "[tool] {invocation}")?;
            }
            Some(UiUpdate::ToolDone { name, .. }) => {
                let title = registry
                    .get(&name)
                    .map(|tool| tool.title().to_string())
                    .unwrap_or_else(|| name.clone());
                writeln!(out, "[tool] {title} done")?;
            }
            Some(UiUpdate::ToolError { error, .. }) => {
                writeln!(out, "[tool] error: {error}")?;
            }
            Some(UiUpdate::ToolKilled { .. }) => {
                writeln!(out, "[tool] killed")?;
            }
            Some(UiUpdate::SteerQueued) | Some(UiUpdate::SteerApplied) => {}
            Some(UiUpdate::StreamDone) => {
                writeln!(out)?;
                response_finished = true;
            }
            Some(UiUpdate::StreamCancelled) => {
                writeln!(out, "---[ Interrupted ]---")?;
                response_finished = true;
            }
            Some(UiUpdate::Error(error)) => {
                writeln!(out, "---[ Error ]---\n{error}")?;
                response_finished = true;
            }
            None => response_finished = true,
        }
    }

    // Dropping the AgentLoop closes cmd_tx, which makes the outer loop
    // return the final committed history from `handle`.
    drop(agent);

    let final_context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");
    writeln!(
        out,
        "\n---[ Conversation history of {} messages ]---",
        final_context.messages().len()
    )?;
    // Emit the committed history as JSONL (one JSON object per message) in
    // the same wire shape the provider sees, rather than Rust's Debug
    // rendering. A blank line separates the header from the JSONL payload.
    if let Some(array) = serialize_messages(final_context.messages()).as_array() {
        for message in array {
            writeln!(
                out,
                "{}",
                serde_json::to_string(message).expect("message serializes to JSON")
            )?;
        }
    }
    Ok(())
}

/// Render a tool invocation for the CLI status line, e.g.
/// `Read docs/features.md [limit=32]`.
///
/// This is **host-side presentation policy** (CLI-only in Phase 3): the App
/// decides how a tool is shown, using the tool's [`title`](Tool::title) as the
/// label, the tool's own `argument_order` to pick the primary (inline)
/// argument, and its `non_default_arguments` for the bracketed rest. Host
/// details (brackets, eliding long values) stay here.
fn render_tool_invocation(tool: &dyn Tool, arguments: &serde_json::Value) -> String {
    let mut line = tool.title().to_string();
    let mut non_default = tool.non_default_arguments(arguments);
    non_default.sort_by(|(key_a, _), (key_b, _)| {
        (tool.argument_order(key_a), key_a).cmp(&(tool.argument_order(key_b), key_b))
    });
    let mut rest: Vec<String> = Vec::new();
    for (index, (key, value)) in non_default.into_iter().enumerate() {
        if index == 0 {
            // A short-string primary is shown inline; a long one is dropped to
            // keep the status line short (its content stays in the JSONL
            // history).
            if let Some(text) = value.as_str() {
                if text.len() <= STRING_ELIDE_LIMIT {
                    line.push(' ');
                    line.push_str(text);
                }
            }
            continue;
        }
        rest.push(format!("{key}={}", render_arg_value(value)));
    }
    if !rest.is_empty() {
        line.push_str(&format!(" [{}]", rest.join(", ")));
    }
    line
}

/// Render a single argument value: short strings, numbers, and booleans as-is;
/// long strings elided to a length placeholder.
fn render_arg_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) if text.len() > STRING_ELIDE_LIMIT => {
            format!("<{}-char string elided>", text.chars().count())
        }
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "?".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::fs::ReadTool;

    fn read_tool() -> Arc<dyn Tool> {
        Arc::new(ReadTool::default())
    }

    #[test]
    fn render_keeps_primary_inline_and_non_default_args_in_brackets() {
        let rendered = render_tool_invocation(
            read_tool().as_ref(),
            &serde_json::json!({
                "path": "some/document.md",
                "mode": "lines",
                "offset": 88,
                "limit": 10,
            }),
        );
        assert_eq!(
            rendered,
            "Read some/document.md [mode=lines, offset=88, limit=10]"
        );
    }

    #[test]
    fn render_suppresses_default_valued_args() {
        // mode=bytes and offset=0 are defaults and must be suppressed.
        let rendered = render_tool_invocation(
            read_tool().as_ref(),
            &serde_json::json!({
                "path": "some/document.md",
                "mode": "bytes",
                "offset": 0,
                "limit": 32,
            }),
        );
        assert_eq!(rendered, "Read some/document.md [limit=32]");
    }

    #[test]
    fn render_shows_glob_pattern_inline_via_title() {
        let tool: Arc<dyn Tool> = Arc::new(crate::agent::tools::fs::GlobTool::default());
        let rendered = render_tool_invocation(
            tool.as_ref(),
            &serde_json::json!({ "pattern": "src/**/*.rs" }),
        );
        assert_eq!(rendered, "Glob src/**/*.rs");
    }

    #[test]
    fn render_elides_long_string_values() {
        let tool: Arc<dyn Tool> = Arc::new(crate::agent::tools::fs::WriteTool::default());
        let rendered = render_tool_invocation(
            tool.as_ref(),
            &serde_json::json!({
                "path": "/tmp/out.txt",
                "content": "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt",
            }),
        );
        assert!(rendered.starts_with("Write /tmp/out.txt ["));
        assert!(
            rendered.contains("char string elided>]"),
            "long content should be elided: {rendered}"
        );
        assert!(
            !rendered.contains("Lorem ipsum"),
            "content must not appear: {rendered}"
        );
    }
}
