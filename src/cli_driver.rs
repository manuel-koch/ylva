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

use crate::agent::tools::clarify::ClarifyTool;
use crate::agent::tools::spawn_subagent::SpawnSubagentTool;
use crate::agent::tools::{default_registry, Tool, ToolRegistryConfig};
use crate::agent::{AgentCommand, AgentLoop, Role, UiUpdate};
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::engine::{EngineConfig, EngineImpl, DEFAULT_CONTEXT_LENGTH};
use crate::llm::{serialize_messages, Provider};
use crate::sandbox::{ApprovalCheck, Sandbox};
use crate::ToolRegistry;

fn capitalize_first(s: &str) -> String {
    crate::agent::tools::title_case_tool_name(s)
}

/// Wraps `command` in `AgentCommand::ForChild { run_id, .. }` when `run_id`
/// is `Some` — i.e. when the pending tool/clarify call that produced
/// `command` was forwarded up from a child agent (`UiUpdate::ToolPending`/
/// `ClarifyPending::run_id`) rather than raised by the root itself.
/// Otherwise returns `command` unchanged, exactly as sent today.
fn route_command(run_id: Option<uuid::Uuid>, command: AgentCommand) -> AgentCommand {
    match run_id {
        Some(run_id) => AgentCommand::ForChild {
            run_id,
            command: Box::new(command),
        },
        None => command,
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
        // A literal identifier (path, glob pattern, regex, ...) reads fine
        // bare even after a multi-word title, since it visually contrasts
        // with the surrounding words. Free-form prose doesn't — e.g.
        // `spawn_subagent`'s "Spawn Subagent" title followed by a bare
        // multi-word `label` blurs into one run-on phrase. Quote only when
        // *both* the title and the primary value are multi-word, so the
        // quotes appear exactly where that ambiguity exists.
        let is_multi_word = |s: &str| s.split_whitespace().count() > 1;
        if is_multi_word(title) && is_multi_word(&primary) {
            line.push_str(&format!("\"{primary}\""));
        } else {
            line.push_str(&primary);
        }
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
        let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
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

/// Which kind of assistant output is currently being written to the CLI
/// stream, mirroring the TUI's `partial_thinking_text` /
/// `partial_assistant_text` split (see `AppState`). The CLI has no bubbles to
/// flush into, so a section switch is rendered as a header/footer line
/// instead, but the switching rules are the same: a response chunk closes an
/// open thinking segment and vice versa, and a tool call or stream end closes
/// whatever segment is open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamSection {
    None,
    Thinking,
    Response,
}

/// Close an open `Thinking` section, printing its elapsed duration footer
/// (mirrors `AppState::flush_partial_thinking_message`). No-op for `None` /
/// `Response`. Leaves `current_section` at `None` either way — callers that
/// need a header for what comes next (e.g. `AppendText`) print it themselves,
/// since the header text depends on what section is opening, not on what
/// just closed.
fn flush_thinking_section(
    out: &mut (dyn Write + Send),
    current_section: &mut StreamSection,
    thinking_started_at: &mut Option<std::time::Instant>,
) -> anyhow::Result<()> {
    if *current_section == StreamSection::Thinking {
        let elapsed = thinking_started_at
            .take()
            .map(|started_at| started_at.elapsed())
            .unwrap_or_default();
        writeln!(out, "\n[thought for {:.3}s]", elapsed.as_secs_f64())?;
    }
    *current_section = StreamSection::None;
    Ok(())
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
    // Tracks whether the assistant is currently emitting thinking/reasoning
    // output or its final reply, so switches between the two (and switches
    // caused by a tool call) are rendered in the right order — see
    // `StreamSection` above.
    let mut current_section = StreamSection::None;
    let mut thinking_started_at: Option<std::time::Instant> = None;
    // Headless mode has nobody to keep the process alive after the visible
    // conversation turn ends, unlike the TUI. A `spawn_subagent` call
    // resolves immediately with `{"status": "running"/"queued"}` (see
    // `resolve_spawn_subagent`), so the parent can go idle (`StreamDone`)
    // while its child is still running — the child's real outcome only
    // arrives later, tied to the same call id, as either a `ToolDone` (the
    // normal `finish_rx` path and the `AgentCommand::Stop` cancellation
    // path both resolve this way) or, in the structurally-unreachable
    // fail-safe case where no `ChildAgentManager` is wired, a `ToolError`.
    // Ending the loop on the first `StreamDone` would drop the agent
    // (closing `cmd_tx`) before that terminal event ever fires, silently
    // discarding the child's result. Worse, if the parent was idle when the
    // child finishes, `AgentLoop` re-prompts it immediately (see the
    // `finish_rx` arm in `AgentLoop::run`) so it can react to the result —
    // guaranteeing a further `StreamStarted`/`StreamDone` round that must
    // not be raced past either. So finalizing is deferred entirely to the
    // `StreamDone`/`StreamCancelled` arms below, gated on every spawned call
    // id (tracked by id, not just a count, so any of
    // `ToolDone`/`ToolError`/`ToolKilled` reliably clears it) having
    // reported back — if any are still outstanding at that point, the loop
    // keeps waiting for the guaranteed follow-up round instead of exiting.
    let mut pending_spawn_call_ids: std::collections::HashSet<uuid::Uuid> =
        std::collections::HashSet::new();
    while !response_finished {
        match ui_rx.recv().await {
            Some(UiUpdate::StreamStarted) => {
                // The header is deferred to the first `AppendThinking` /
                // `AppendText` of this round, since its text depends on
                // which section opens first (`---[ Assistant Thinking ]---`
                // vs `---[ Assistant ]---`).
                current_section = StreamSection::None;
                thinking_started_at = None;
            }
            Some(UiUpdate::AppendThinking(text)) => {
                if current_section != StreamSection::Thinking {
                    // A thinking chunk following response text means the
                    // model switched back to thinking; the response segment
                    // needs no footer, just a new thinking header. This is
                    // also the header for the very first chunk of the round
                    // when it happens to be thinking (merged with the
                    // "Assistant" label, since there's no separate response
                    // section yet to distinguish it from).
                    writeln!(out, "\n---[ Assistant Thinking ]---")?;
                    thinking_started_at = Some(std::time::Instant::now());
                    current_section = StreamSection::Thinking;
                }
                write!(out, "{text}")?;
                out.flush()?;
            }
            Some(UiUpdate::AppendText(text)) => {
                if current_section == StreamSection::Thinking {
                    flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                }
                if current_section != StreamSection::Response {
                    // Prints once per round: either this is the very first
                    // chunk (no thinking preceded it) or a thinking segment
                    // was just flushed above — either way the response
                    // section is opening for the first time.
                    writeln!(out, "\n---[ Assistant ]---")?;
                }
                current_section = StreamSection::Response;
                write!(out, "{text}")?;
                out.flush()?;
            }
            Some(UiUpdate::ToolPending {
                name,
                id,
                title_arguments,
                content_sections,
                arguments,
                run_id,
            }) => {
                // A tool call always closes whatever thinking/response
                // segment preceded it, mirroring `flush_partial_assistant_message`.
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                current_section = StreamSection::None;
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
                        agent
                            .send_command(route_command(run_id, AgentCommand::ApproveTool { id }))
                            .await;
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
                            .send_command(route_command(
                                run_id,
                                AgentCommand::DenyTool { id, reason },
                            ))
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
                            .send_command(route_command(
                                run_id,
                                AgentCommand::DenyTool {
                                    id,
                                    reason: reason.into(),
                                },
                            ))
                            .await;
                    }
                }
            }
            Some(UiUpdate::ClarifyPending {
                id,
                question,
                run_id,
                ..
            }) => {
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                current_section = StreamSection::None;
                let clarify_title = ClarifyTool.title();
                let clarify_name = ClarifyTool::NAME;
                writeln!(
                    out,
                    "[tool] {clarify_title} [question={question}] → denied: {clarify_name} is not available in CLI mode"
                )?;
                denied_tool_ids.insert(id);
                let _ = id;
                agent
                    .send_command(route_command(
                        run_id,
                        AgentCommand::AnswerClarify(String::new()),
                    ))
                    .await;
            }
            Some(UiUpdate::ToolStart {
                name,
                id,
                title_arguments,
                content_sections,
                ..
            }) => {
                // First appearance of this tool call (auto-run tool with no
                // approval step, so no prior `ToolPending`): flush any
                // thinking/response text accumulated before it, mirroring
                // `update_tool_bubble_to_running`'s flush for the same case.
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                current_section = StreamSection::None;
                let title = registry
                    .get(&name)
                    .map(|tool| tool.title().to_string())
                    .unwrap_or_else(|| capitalize_first(&name));
                let invocation = render_invocation_from_title_arguments(&title, &title_arguments);
                pending_invocations.insert(id, invocation.clone());
                writeln!(out, "[tool] {invocation}")?;
                render_content_sections(out, &content_sections)?;
                if name == SpawnSubagentTool::NAME {
                    pending_spawn_call_ids.insert(id);
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: tracking spawned child, deferring conversation end"
                    );
                }
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
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned child reported ToolDone"
                    );
                }
            }
            Some(UiUpdate::ToolError { id, error, .. }) => {
                if !denied_tool_ids.remove(&id) {
                    writeln!(out, "[tool] error: {error}")?;
                }
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned child reported ToolError"
                    );
                }
            }
            Some(UiUpdate::ToolAutoDenied { name, reason, .. }) => {
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                current_section = StreamSection::None;
                writeln!(out, "[tool] auto-denied '{name}': {reason}")?;
            }
            Some(UiUpdate::ToolKilled { id, .. }) => {
                writeln!(out, "[tool] killed")?;
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned child reported ToolKilled"
                    );
                }
            }
            Some(UiUpdate::SteerQueued { .. }) | Some(UiUpdate::SteerApplied { .. }) => {}
            // No-op in headless mode: there's no live bubble to patch a
            // status update onto (see `UiUpdate::ToolProgress`'s doc comment
            // for its TUI-only purpose). The eventual `ToolDone` still
            // prints the real outcome once the child actually finishes.
            Some(UiUpdate::ToolProgress { .. }) => {}
            Some(UiUpdate::StreamDone) => {
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                writeln!(out)?;
                if pending_spawn_call_ids.is_empty() {
                    tracing::debug!(
                        "cli_driver: StreamDone with no pending spawns, ending conversation"
                    );
                    response_finished = true;
                } else {
                    tracing::debug!(
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: StreamDone but children still outstanding, waiting for them"
                    );
                }
            }
            Some(UiUpdate::StreamCancelled) => {
                flush_thinking_section(out, &mut current_section, &mut thinking_started_at)?;
                writeln!(out, "---[ Interrupted ]---")?;
                if pending_spawn_call_ids.is_empty() {
                    tracing::debug!(
                        "cli_driver: StreamCancelled with no pending spawns, ending conversation"
                    );
                    response_finished = true;
                } else {
                    tracing::debug!(
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: StreamCancelled but children still outstanding, waiting for them"
                    );
                }
            }
            Some(UiUpdate::Error { message, run_id }) => {
                match run_id {
                    None => {
                        flush_thinking_section(
                            out,
                            &mut current_section,
                            &mut thinking_started_at,
                        )?;
                        writeln!(out, "---[ Error ]---\n{message}")?;
                        response_finished = true;
                    }
                    // A child's own stream-level error (e.g. its max-tool-use-
                    // iterations guard tripped) is not the root conversation
                    // ending — the root learns the spawn finished via the
                    // spawn tool's own `ToolDone`/`ToolError` (see above), not
                    // this generic error. Headless mode has no separate child
                    // transcript to render into, so just surface it tagged,
                    // without ending the response.
                    Some(run_id) => {
                        writeln!(out, "[child {run_id}] error: {message}")?;
                    }
                }
            }
            // Headless mode has no separate child transcript to render into
            // (see drill-in conversation model, Phase 6.10, TUI-only): child
            // tool events already arrive tagged with `run_id` and are
            // rendered inline by the arms above, so only the child's own
            // free-form text/thinking tokens and lifecycle markers are
            // silently ignored here.
            Some(
                UiUpdate::ChildAgentStarted { .. }
                | UiUpdate::ChildAgentToken { .. }
                | UiUpdate::ChildAgentDone { .. },
            ) => {}
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
    use crate::agent::tools::read::ReadTool;

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
        let tool: Arc<dyn Tool> = Arc::new(crate::agent::tools::glob::GlobTool::default());
        let rendered = render_tool_invocation(
            tool.as_ref(),
            &serde_json::json!({ "pattern": "src/**/*.rs" }),
        );
        assert_eq!(rendered, "Glob src/**/*.rs");
    }

    #[test]
    fn render_elides_long_string_values() {
        let tool: Arc<dyn Tool> = Arc::new(crate::agent::tools::write::WriteTool::default());
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

    #[test]
    fn render_invocation_quotes_primary_when_title_and_primary_are_both_multi_word() {
        let rendered = render_invocation_from_title_arguments(
            "Spawn Subagent",
            &[(
                "label".to_string(),
                serde_json::json!("Analyze TUI Features"),
            )],
        );
        assert_eq!(rendered, "Spawn Subagent \"Analyze TUI Features\"");
    }

    #[test]
    fn render_invocation_leaves_bare_primary_when_title_is_single_word() {
        // Single-word title ("Glob") reads fine against a bare (even
        // multi-word-looking) pattern, so no quoting is added.
        let rendered = render_invocation_from_title_arguments(
            "Glob",
            &[("pattern".to_string(), serde_json::json!("src/**/*.rs"))],
        );
        assert_eq!(rendered, "Glob src/**/*.rs");
    }

    #[test]
    fn render_invocation_leaves_bare_primary_when_value_is_single_word() {
        // Multi-word title but a single-word primary value: no ambiguity, no
        // quoting needed.
        let rendered = render_invocation_from_title_arguments(
            "Spawn Subagent",
            &[("label".to_string(), serde_json::json!("Refactor"))],
        );
        assert_eq!(rendered, "Spawn Subagent Refactor");
    }
}
