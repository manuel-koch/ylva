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

use tokio::sync::mpsc;

use crate::agent::tools::default_registry;
use crate::agent::{AgentCommand, AgentLoop, Role, UiUpdate};
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::engine::{EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH};
use crate::llm::{serialize_messages, Provider};
use crate::sandbox::{ApprovalCheck, Sandbox};
use crate::ToolRegistry;

#[cfg(test)]
use crate::Tool;

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn render_invocation_from_title_arguments(
    title: &str,
    title_arguments: &[(String, serde_json::Value)],
) -> String {
    let mut line = title.to_string();
    let is_short = |s: &str| s.len() <= STRING_ELIDE_LIMIT;
    let scalar_primary = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::String(s) if is_short(s) => Some(s.as_str().to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let mut iter = title_arguments.iter().peekable();
    // Consume the first entry only if it is a bare-worthy scalar (not a boolean).
    if let Some(primary) = iter
        .next_if(|(_, v)| scalar_primary(v).is_some())
        .and_then(|(_, v)| scalar_primary(v))
    {
        line.push(' ');
        line.push_str(&primary);
    }
    let rest: Vec<String> = iter
        .map(|(k, v)| format!("{k}={}", render_arg_value(v)))
        .collect();
    if !rest.is_empty() {
        line.push_str(&format!(" [{}]", rest.join(", ")));
    }
    line
}

fn render_content_sections(
    out: &mut dyn Write,
    sections: &[crate::agent::tools::Section],
) -> anyhow::Result<()> {
    for section in sections {
        match section {
            crate::agent::tools::Section::Content(content_section) => {
                if let Some(label) = &content_section.label {
                    writeln!(out, "  --- {label} ---")?;
                }
                let line_count = content_section.content.lines().count();
                let last_line_num = content_section.line_number_offset.unwrap_or(0) + line_count;
                let digits = last_line_num.to_string().len();
                for (i, line) in content_section.content.lines().enumerate() {
                    if let Some(offset) = content_section.line_number_offset {
                        write!(out, "  {:>digits$}: ", offset + i + 1)?;
                    } else {
                        write!(out, "  ")?;
                    }
                    writeln!(out, "{line}")?;
                }
            }
            crate::agent::tools::Section::Diff(diff_section) => {
                if let Some(label) = &diff_section.label {
                    writeln!(out, "  --- {label} ---")?;
                }
                for line in diff_section.content.lines() {
                    writeln!(out, "  {line}")?;
                }
            }
            crate::agent::tools::Section::SearchResult(search_section) => {
                writeln!(out, "  --- {} ---", search_section.file_path)?;
                let gutter_width = search_section
                    .lines
                    .iter()
                    .map(|l| l.line_number)
                    .max()
                    .map(|n| n.to_string().len())
                    .unwrap_or(1);
                for line in &search_section.lines {
                    if line.is_match {
                        writeln!(
                            out,
                            "  {:>gutter_width$}: {}",
                            line.line_number, line.content
                        )?;
                    } else {
                        writeln!(
                            out,
                            "  {:>gutter_width$}- {}",
                            line.line_number, line.content
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Length beyond which a string value is treated as "long" and elided when
/// rendering a tool invocation.
const STRING_ELIDE_LIMIT: usize = 40;

/// Top-level headless (non-terminal) application entry point.
///
/// Constructs an [`AgentLoop`] via [`AgentFactory`] and delegates to
/// [`run_cli_to`] with the configured writer.
pub struct ConsoleApp {
    factory: Arc<AgentFactory>,
    profile: ProfileConfig,
    agent_type: String,
    prompt: String,
    writer: Box<dyn Write + Send>,
}

impl ConsoleApp {
    pub fn new(
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
        agent_type: String,
        prompt: String,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        ConsoleApp {
            factory,
            profile,
            agent_type,
            prompt,
            writer,
        }
    }
}

impl App for ConsoleApp {
    async fn run(mut self) -> anyhow::Result<u8> {
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()));
        let registry = Arc::new(default_registry(sandbox));
        let (agent, ui_rx) = self
            .factory
            .create(&self.profile, &self.agent_type, Role::Root)?;
        run_cli_to(agent, ui_rx, registry, self.prompt, &mut *self.writer)
            .await
            .map(|_| 0u8)
    }
}

/// Run the agent loop headless, printing to stdout.
pub async fn run_cli(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
) -> anyhow::Result<()> {
    // TODO Phase 5 Step 9: replace with AgentFactory
    let engine_config = EngineConfig {
        provider: provider.name().to_string(),
        model: String::new(),
        temperature: None,
        context_length: DEFAULT_CONTEXT_LENGTH,
        api_base_url: None,
        api_key: None,
    };
    let engine = Arc::new(EngineImpl::new(provider, engine_config));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(
        agent,
        ui_rx,
        registry,
        initial_prompt,
        &mut std::io::stdout(),
    )
    .await
}

/// [`run_cli`] with an explicit output writer (used by tests to capture
/// the emitted lines without redirecting process stdout).
#[doc(hidden)]
pub async fn run_cli_to(
    agent: AgentLoop,
    mut ui_rx: mpsc::Receiver<UiUpdate>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut agent = agent;

    // Echo the user's turn before sending the request, so it appears even
    // while the provider is still connecting / retrying (the "Assistant:"
    // prefix is printed when the stream actually starts).
    writeln!(out, "---[ User ]---")?;
    writeln!(out, "{initial_prompt}")?;
    agent
        .send_command(AgentCommand::Prompt(initial_prompt))
        .await;
    let handle = agent.run();

    let mut denied_tool_ids: std::collections::HashSet<uuid::Uuid> =
        std::collections::HashSet::new();
    let mut pending_invocations: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
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
            Some(UiUpdate::ToolPending {
                name,
                id,
                title_arguments,
                content_sections,
                arguments,
                ..
            }) => {
                let title = registry
                    .get(&name)
                    .map(|tool| tool.title().to_string())
                    .unwrap_or_else(|| capitalize_first(&name));
                let invocation = render_invocation_from_title_arguments(&title, &title_arguments);
                pending_invocations.insert(id, invocation.clone());
                let approval_check = registry
                    .get(&name)
                    .map(|tool| tool.needs_approval(&arguments))
                    .unwrap_or(ApprovalCheck::UserApprovalRequired);
                match approval_check {
                    ApprovalCheck::AutoApproved => {
                        writeln!(out, "[tool] {invocation}")?;
                        render_content_sections(out, &content_sections)?;
                        agent.send_command(AgentCommand::ApproveTool { id }).await;
                    }
                    ApprovalCheck::AutoDenied { reason } => {
                        let short_args: Vec<_> = title_arguments
                            .iter()
                            .filter(|(_, v)| !matches!(v, serde_json::Value::String(s) if s.len() > STRING_ELIDE_LIMIT))
                            .cloned()
                            .collect();
                        let denied_invocation =
                            render_invocation_from_title_arguments(&title, &short_args);
                        writeln!(out, "[tool] {denied_invocation} → auto-denied: {reason}")?;
                        render_content_sections(out, &content_sections)?;
                        denied_tool_ids.insert(id);
                        agent
                            .send_command(AgentCommand::DenyTool { id, reason })
                            .await;
                    }
                    ApprovalCheck::UserApprovalRequired => {
                        let reason = "tool requires approval, not supported in CLI mode";
                        let short_args: Vec<_> = title_arguments
                            .iter()
                            .filter(|(_, v)| !matches!(v, serde_json::Value::String(s) if s.len() > STRING_ELIDE_LIMIT))
                            .cloned()
                            .collect();
                        let denied_invocation =
                            render_invocation_from_title_arguments(&title, &short_args);
                        writeln!(out, "[tool] {denied_invocation} → denied: {reason}")?;
                        render_content_sections(out, &content_sections)?;
                        denied_tool_ids.insert(id);
                        agent
                            .send_command(AgentCommand::DenyTool {
                                id,
                                reason: reason.into(),
                            })
                            .await;
                    }
                }
            }
            Some(UiUpdate::ToolStart {
                name,
                id,
                title_arguments,
                content_sections,
                ..
            }) => {
                let title = registry
                    .get(&name)
                    .map(|tool| tool.title().to_string())
                    .unwrap_or_else(|| capitalize_first(&name));
                let invocation = render_invocation_from_title_arguments(&title, &title_arguments);
                pending_invocations.insert(id, invocation.clone());
                writeln!(out, "[tool] {invocation}")?;
                render_content_sections(out, &content_sections)?;
            }
            Some(UiUpdate::ToolDone {
                id,
                name,
                summary,
                duration,
                content_sections,
                ..
            }) => {
                let title = pending_invocations.remove(&id).unwrap_or_else(|| {
                    registry
                        .get(&name)
                        .map(|tool| tool.title().to_string())
                        .unwrap_or_else(|| capitalize_first(&name))
                });
                let duration_s = duration.as_secs_f64();
                let summary_str = if summary.is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> =
                        summary.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    format!(" {}", parts.join(" "))
                };
                writeln!(out, "[tool] {title} done ({duration_s:.3}s){summary_str}")?;
                render_content_sections(out, &content_sections)?;
            }
            Some(UiUpdate::ToolError { id, error, .. }) => {
                if !denied_tool_ids.remove(&id) {
                    writeln!(out, "[tool] error: {error}")?;
                }
            }
            Some(UiUpdate::ToolAutoDenied { name, reason, .. }) => {
                writeln!(out, "[tool] auto-denied '{name}': {reason}")?;
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
#[cfg(test)]
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
