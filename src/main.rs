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

use std::sync::Arc;
use std::time::Duration;

use ylva::llm::{serialize_messages, MockProvider, OpenAiProvider, Provider};
use ylva::{default_registry, AgentCommand, AgentLoop, Tool, UiUpdate};

/// Fallback demo prompt used when neither `--prompt` nor piped stdin supplies
/// the initial user turn.
const DEFAULT_DEMO_PROMPT: &str = "What makes the sky look blue?";

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("{}", ylva::BUILD_INFO.render());
        return;
    }

    let provider: Arc<dyn Provider> = select_provider();
    let registry = Arc::new(default_registry());
    let (mut agent, mut ui_rx) = AgentLoop::new(provider, Arc::clone(&registry));

    let prompt = initial_prompt(std::env::args().skip(1).collect());
    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
    runtime.block_on(async {
        // Echo the user's turn before sending the request, so it appears even
        // while the provider is still connecting / retrying (the "Assistant:"
        // prefix is printed when the stream actually starts).
        println!("---[ User ]---\n{prompt}");
        agent
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        let handle = agent.run();

        let mut response_finished = false;
        while !response_finished {
            match ui_rx.recv().await {
                Some(UiUpdate::StreamStarted) => {
                    println!("\n---[ Assistant ]---");
                }
                Some(UiUpdate::AppendText(text)) => print!("{text}"),
                Some(UiUpdate::ToolPending { name, id }) => {
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
                        println!("[tool] {title} denied (no --auto-approve-write in Phase 3)");
                        agent.send_command(AgentCommand::DenyTool { id }).await;
                    } else {
                        println!("[tool] {title} auto-approved");
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
                    println!("[tool] {invocation}");
                }
                Some(UiUpdate::ToolDone {
                    name, result: _, ..
                }) => {
                    // for now, don't output any tool result
                    let title = registry
                        .get(&name)
                        .map(|tool| tool.title().to_string())
                        .unwrap_or_else(|| name.clone());
                    println!("[tool] {title} done");
                }
                Some(UiUpdate::ToolError { error, .. }) => {
                    println!("[tool] error: {error}");
                }
                Some(UiUpdate::ToolKilled { .. }) => {
                    println!("[tool] killed");
                }
                Some(UiUpdate::StreamDone) => {
                    println!();
                    response_finished = true;
                }
                Some(UiUpdate::StreamCancelled) => {
                    println!("---[ Interrupted ]---");
                    response_finished = true;
                }
                Some(UiUpdate::Error(error)) => {
                    println!("---[ Error ]---\n{error}");
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
        println!(
            "\n---[ Conversation history of {} messages ]---",
            final_context.messages().len()
        );
        // Emit the committed history as JSONL (one JSON object per message) in
        // the same wire shape the provider sees, rather than Rust's Debug
        // rendering. A blank line separates the header from the JSONL payload.
        if let Some(array) = serialize_messages(final_context.messages()).as_array() {
            for message in array {
                println!(
                    "{}",
                    serde_json::to_string(message).expect("message serializes to JSON")
                );
            }
        }
    });
}

/// The initial user turn sent to the agent, resolved by precedence:
/// `--prompt <text>`, else piped stdin read to EOF, else the hardcoded demo
/// prompt. stdin is only consumed when it is **not** a terminal (something is
/// piped in); no interactive prompt-read is attempted in the CLI-only phase.
/// The resolved prompt is trimmed (leading/trailing whitespace removed) before
/// it is echoed and sent.
fn initial_prompt(args: Vec<String>) -> String {
    let prompt = if let Some(text) = prompt_from_arg(&args) {
        text
    } else if let Some(text) = prompt_from_stdin() {
        text
    } else {
        DEFAULT_DEMO_PROMPT.to_string()
    };
    prompt.trim().to_string()
}

/// Extract the value of the first `--prompt <text>` argument, if any.
fn prompt_from_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--prompt" {
            return iter.next().cloned();
        }
    }
    None
}

/// Read all of stdin when it is not a terminal. Returns `None` when stdin is a
/// terminal (no piped input to consume) or contains only whitespace.
fn prompt_from_stdin() -> Option<String> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer).ok()?;
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

/// Default Ollama model when `OLLAMA_MODEL` is unset.
const DEFAULT_OLLAMA_MODEL: &str = "gemma4:e4b-mlx";
/// Default OpenRouter model when `OPENROUTER_MODEL` is unset.
const DEFAULT_OPENROUTER_MODEL: &str = "deepseek/deepseek-v4-flash-0731";

/// Select the Phase 2 demo provider from environment variables with
/// **Ollama-first** precedence. Falls back to the `MockProvider` when no
/// backend is configured (still headless; no CLI/config parsing in Phase 2).
fn select_provider() -> Arc<dyn Provider> {
    if let Ok(base_url) = std::env::var("OLLAMA_BASE_URL") {
        if !base_url.is_empty() {
            let model =
                std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
            let api_key = std::env::var("OLLAMA_API_KEY").ok();
            report_provider("ollama", Some(&base_url), Some(&model), api_key.is_some());
            return Arc::new(OpenAiProvider::new(base_url, model, api_key));
        }
    }
    if let Ok(base_url) = std::env::var("OPENROUTER_BASE_URL") {
        if !base_url.is_empty() {
            let model = std::env::var("OPENROUTER_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENROUTER_MODEL.to_string());
            let api_key = std::env::var("OPENROUTER_API_KEY").ok();
            report_provider(
                "openrouter",
                Some(&base_url),
                Some(&model),
                api_key.is_some(),
            );
            return Arc::new(OpenAiProvider::new(base_url, model, api_key));
        }
    }
    report_provider("mock", None, None, false);
    Arc::new(MockProvider::new(
        vec![
            "The sky looks blue ".to_string(),
            "because air molecules scatter ".to_string(),
            "blue light more than other colors.".to_string(),
        ],
        Duration::from_millis(60),
    ))
}

/// Print the selected provider wiring to stderr when `YLVA_VERBOSE` is set, so
/// a misconfigured backend (wrong `base_url`, model, or auth) is visible at
/// startup instead of surfacing as a confusing downstream error.
fn report_provider(kind: &str, base_url: Option<&str>, model: Option<&str>, has_api_key: bool) {
    if !verbose_enabled() {
        return;
    }
    let base = base_url.unwrap_or("-");
    let model = model.unwrap_or("-");
    let auth = if has_api_key { "set" } else { "unset" };
    eprintln!("[ylva] provider={kind} base_url={base} model={model} api_key={auth}");
}

fn verbose_enabled() -> bool {
    std::env::var("YLVA_VERBOSE").is_ok_and(|value| !value.is_empty())
}

/// Length beyond which a string value is treated as "long" and elided when
/// rendering a tool invocation.
const STRING_ELIDE_LIMIT: usize = 40;

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

    fn read_tool() -> Arc<dyn Tool> {
        Arc::new(ylva::agent::tools::fs::ReadTool::default())
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
        let tool: Arc<dyn Tool> = Arc::new(ylva::agent::tools::fs::GlobTool::default());
        let rendered = render_tool_invocation(
            tool.as_ref(),
            &serde_json::json!({ "pattern": "src/**/*.rs" }),
        );
        assert_eq!(rendered, "Glob src/**/*.rs");
    }

    #[test]
    fn render_elides_long_string_values() {
        let tool: Arc<dyn Tool> = Arc::new(ylva::agent::tools::fs::WriteTool::default());
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
