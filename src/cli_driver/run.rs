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

//! Headless run loop: drives one agent run and renders its updates.

use std::io::Write;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::invocation_render::{
    capitalize_first, display_title_for, render_content_sections, render_denied_invocation,
    render_invocation_from_title_arguments,
};
use super::output::{flush_stream_section, write_cli_styled_line, CliOutputOptions, StreamSection};
use super::title_driver::TitleDriver;
use crate::agent::tools::clarify::ClarifyTool;
use crate::agent::tools::spawn_subagent::SpawnSubagentTool;
use crate::agent::tools::Tool;
use crate::agent::{AgentCommand, AgentLoop, UiUpdate};
use crate::llm::engine::{EngineConfig, EngineImpl};
use crate::llm::Provider;
use crate::run_outcome::RunOutcome;
use crate::sandbox::ApprovalCheck;
use crate::stop_signal::StopSignals;
use crate::theme::{
    ASSISTANT_STREAM_BORDER_COLOR, ERROR_COLOR, INTERRUPTED_COLOR, NOTICE_COLOR,
    THINKING_BORDER_COLOR, TOOL_BUBBLE_BORDER_COLOR, USER_MESSAGE_BORDER_COLOR,
};
use crate::ToolRegistry;

/// Wraps `command` in `AgentCommand::ForSubagent { run_id, .. }` when `run_id`
/// is `Some` — i.e. when the pending tool/clarify call that produced
/// `command` was forwarded up from a subagent (`UiUpdate::ToolPending`/
/// `ClarifyPending::run_id`) rather than raised by the root itself.
/// Otherwise returns `command` unchanged, exactly as sent today.
pub(super) fn route_command(run_id: Option<uuid::Uuid>, command: AgentCommand) -> AgentCommand {
    match run_id {
        Some(run_id) => AgentCommand::ForSubagent {
            run_id,
            command: Box::new(command),
        },
        None => command,
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
        provider_connection: provider.name().to_string(),
        model: String::new(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
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
    ui_rx: mpsc::Receiver<UiUpdate>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    run_cli_to_with_options(
        agent,
        ui_rx,
        registry,
        initial_prompt,
        out,
        CliOutputOptions::plain(),
    )
    .await
}

/// [`run_cli_to`] with explicit terminal-output options.
pub async fn run_cli_to_with_options(
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
    out: &mut (dyn Write + Send),
    output_options: CliOutputOptions,
) -> anyhow::Result<()> {
    run_cli_to_with_title(
        agent,
        ui_rx,
        registry,
        initial_prompt,
        out,
        output_options,
        None,
    )
    .await
    .map(|_outcome| ())
}

/// Drive one headless run and report how it ended. Callers map the returned
/// [`RunOutcome`] to an exit code.
pub(super) async fn run_cli_to_with_title(
    agent: AgentLoop,
    mut ui_rx: mpsc::Receiver<UiUpdate>,
    registry: Arc<ToolRegistry>,
    initial_prompt: String,
    out: &mut (dyn Write + Send),
    output_options: CliOutputOptions,
    title_driver: Option<TitleDriver>,
) -> anyhow::Result<RunOutcome> {
    let mut agent = agent;
    let mut outcome = RunOutcome::default();
    let mut stop_signals = StopSignals::install()?;

    if let Some(title_driver) = &title_driver {
        title_driver.trigger(&agent);
    }

    // Echo the user's turn before sending the request, so it appears even
    // while the provider is still connecting / retrying (the "Assistant:"
    // prefix is printed when the stream actually starts).
    write_cli_styled_line(
        out,
        "---[ User ]---",
        USER_MESSAGE_BORDER_COLOR,
        false,
        output_options,
    )?;
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
    // resolves immediately with a structured admission event (see
    // `resolve_spawn_subagent`), so the parent can go idle (`StreamDone`)
    // while its subagent is still running — the subagent's real outcome only
    // arrives later, tied to the same call id, as either a `ToolDone` (the
    // normal `finish_rx` path and the `AgentCommand::Stop` cancellation
    // path both resolve this way) or, in the structurally-unreachable
    // fail-safe case where no `SubagentManager` is wired, a `ToolError`.
    // Ending the loop on the first `StreamDone` would drop the agent
    // (closing `cmd_tx`) before that terminal event ever fires, silently
    // discarding the subagent's result. Once the final outstanding subagent
    // finishes while the parent is idle, `AgentLoop` re-prompts it once (see
    // the `finish_rx` arm in `AgentLoop::run`) so it can aggregate the
    // results — guaranteeing a further `StreamStarted`/`StreamDone` round
    // that must not be raced past either. So finalizing is deferred entirely to the
    // `StreamDone`/`StreamCancelled` arms below, gated on every spawned call
    // id (tracked by id, not just a count, so any of
    // `ToolDone`/`ToolError`/`ToolKilled` reliably clears it) having
    // reported back — if any are still outstanding at that point, the loop
    // keeps waiting for the eventual aggregate continuation instead of
    // exiting.
    let mut pending_spawn_call_ids: std::collections::HashSet<uuid::Uuid> =
        std::collections::HashSet::new();
    // Set by SIGINT/SIGTERM. The loop then stops waiting for a normal end and ends once
    // the agent reports idle; see the `AgentIdle` and `ToolKilled` arms.
    let mut interrupt_requested = false;
    while !response_finished {
        let next_update = tokio::select! {
            update = ui_rx.recv() => update,
            Some(()) = stop_signals.recv(), if !interrupt_requested => {
                interrupt_requested = true;
                outcome.record_interruption();
                tracing::info!("cli_driver: interrupt requested, stopping the run");
                agent
                    .send_command(AgentCommand::Stop {
                        reason: "interrupted by user".into(),
                    })
                    .await;
                continue;
            }
        };
        if let Some(update) = &next_update {
            outcome.record_update(update);
        }
        match next_update {
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
                    writeln!(out)?;
                    write_cli_styled_line(
                        out,
                        "---[ Assistant Thinking ]---",
                        THINKING_BORDER_COLOR,
                        true,
                        output_options,
                    )?;
                    thinking_started_at = Some(std::time::Instant::now());
                    current_section = StreamSection::Thinking;
                }
                write!(out, "{text}")?;
                out.flush()?;
            }
            Some(UiUpdate::AppendText(text)) => {
                if current_section == StreamSection::Thinking {
                    flush_stream_section(
                        out,
                        &mut current_section,
                        &mut thinking_started_at,
                        output_options,
                    )?;
                }
                if current_section != StreamSection::Response {
                    // Prints once per round: either this is the very first
                    // chunk (no thinking preceded it) or a thinking segment
                    // was just flushed above — either way the response
                    // section is opening for the first time.
                    writeln!(out)?;
                    write_cli_styled_line(
                        out,
                        "---[ Assistant ]---",
                        ASSISTANT_STREAM_BORDER_COLOR,
                        false,
                        output_options,
                    )?;
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
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                writeln!(out)?;
                current_section = StreamSection::None;
                let (title, title_arguments) =
                    display_title_for(&registry, &name, &title_arguments);
                let invocation = render_invocation_from_title_arguments(&title, &title_arguments);
                pending_invocations.insert(id, invocation.clone());
                let approval_check = registry
                    .get(&name)
                    .map(|tool| tool.needs_approval(&arguments))
                    .unwrap_or(ApprovalCheck::UserApprovalRequired);
                match approval_check {
                    ApprovalCheck::AutoApproved => {
                        write_cli_styled_line(
                            out,
                            &format!("[tool] {invocation}"),
                            TOOL_BUBBLE_BORDER_COLOR,
                            false,
                            output_options,
                        )?;
                        render_content_sections(out, &content_sections)?;
                        agent
                            .send_command(route_command(run_id, AgentCommand::ApproveTool { id }))
                            .await;
                    }
                    ApprovalCheck::AutoDenied { reason } => {
                        let denied_invocation = render_denied_invocation(&title, &title_arguments);
                        write_cli_styled_line(
                            out,
                            &format!("[tool] {denied_invocation} → auto-denied: {reason}"),
                            ERROR_COLOR,
                            false,
                            output_options,
                        )?;
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
                        outcome.record_approval_denied();
                        let reason = "tool requires approval, not supported in CLI mode";
                        let denied_invocation = render_denied_invocation(&title, &title_arguments);
                        write_cli_styled_line(
                            out,
                            &format!("[tool] {denied_invocation} → denied: {reason}"),
                            ERROR_COLOR,
                            false,
                            output_options,
                        )?;
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
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                writeln!(out)?;
                current_section = StreamSection::None;
                let clarify_title = ClarifyTool.title();
                let clarify_name = ClarifyTool::NAME;
                write_cli_styled_line(
                    out,
                    &format!(
                        "[tool] {clarify_title} [question={question}] → denied: {clarify_name} is not available in CLI mode"
                    ),
                    ERROR_COLOR,
                    false,
                    output_options,
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
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                writeln!(out)?;
                current_section = StreamSection::None;
                let (title, title_arguments) =
                    display_title_for(&registry, &name, &title_arguments);
                let invocation = render_invocation_from_title_arguments(&title, &title_arguments);
                pending_invocations.insert(id, invocation.clone());
                write_cli_styled_line(
                    out,
                    &format!("[tool] {invocation}"),
                    TOOL_BUBBLE_BORDER_COLOR,
                    false,
                    output_options,
                )?;
                render_content_sections(out, &content_sections)?;
                if name == SpawnSubagentTool::NAME {
                    pending_spawn_call_ids.insert(id);
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: tracking spawned subagent, deferring conversation end"
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
                write_cli_styled_line(
                    out,
                    &format!("[tool] {title} done ({duration_s:.3}s){summary_str}"),
                    TOOL_BUBBLE_BORDER_COLOR,
                    false,
                    output_options,
                )?;
                render_content_sections(out, &content_sections)?;
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned subagent reported ToolDone"
                    );
                }
            }
            Some(UiUpdate::ToolError { id, error, .. }) => {
                if !denied_tool_ids.remove(&id) {
                    write_cli_styled_line(
                        out,
                        &format!("[tool] error: {error}"),
                        ERROR_COLOR,
                        false,
                        output_options,
                    )?;
                }
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned subagent reported ToolError"
                    );
                }
            }
            Some(UiUpdate::ToolAutoDenied { name, reason, .. }) => {
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                writeln!(out)?;
                current_section = StreamSection::None;
                write_cli_styled_line(
                    out,
                    &format!("[tool] auto-denied '{name}': {reason}"),
                    ERROR_COLOR,
                    false,
                    output_options,
                )?;
            }
            Some(UiUpdate::ToolKilled { id, run_id }) => {
                write_cli_styled_line(
                    out,
                    "[tool] killed",
                    INTERRUPTED_COLOR,
                    false,
                    output_options,
                )?;
                if pending_spawn_call_ids.remove(&id) {
                    tracing::debug!(
                        tool_call_id = %id,
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: spawned subagent reported ToolKilled"
                    );
                }
                // A stopped root tool leaves the agent idle without a stream
                // event, so an interrupted run ends here.
                if run_id.is_none() && interrupt_requested && pending_spawn_call_ids.is_empty() {
                    response_finished = true;
                }
            }
            Some(UiUpdate::SteerQueued { .. }) | Some(UiUpdate::SteerApplied { .. }) => {}
            // Headless output has no footer for the model-view readout.
            Some(UiUpdate::ContextUsage { .. } | UiUpdate::SubagentContextUsage { .. }) => {}
            // Headless output renders messages inline as they stream, so a
            // transcript marker has nothing to be inserted into.
            Some(UiUpdate::CompactionApplied { .. }) => {}
            Some(UiUpdate::CompactionActivity { .. }) => {}
            // No-op in headless mode: there's no live bubble to patch a
            // status update onto (see `UiUpdate::ToolProgress`'s doc comment
            // for its TUI-only purpose). The eventual `ToolDone` still
            // prints the real outcome once the subagent actually finishes.
            Some(UiUpdate::ToolProgress { .. }) => {}
            Some(UiUpdate::Notice { text, run_id }) => {
                // A notice (for example a dropped profile reasoning effort)
                // is a real configuration fact, not a transcript-only
                // bubble: print it ahead of the response, tagged when it
                // came from a subagent loop. It never ends the turn, so unlike
                // a root `Error` it leaves `response_finished` alone.
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                match run_id {
                    None => {
                        write_cli_styled_line(
                            out,
                            "---[ Notice ]---",
                            NOTICE_COLOR,
                            false,
                            output_options,
                        )?;
                        writeln!(out, "{text}")?;
                    }
                    Some(run_id) => {
                        write_cli_styled_line(
                            out,
                            &format!("[subagent {run_id}] notice: {text}"),
                            NOTICE_COLOR,
                            false,
                            output_options,
                        )?;
                    }
                }
            }
            Some(UiUpdate::StreamDone) => {
                let previous_section = current_section;
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                if previous_section != StreamSection::Response {
                    writeln!(out)?;
                }
                if pending_spawn_call_ids.is_empty() {
                    if let Some(title_driver) = &title_driver {
                        title_driver.trigger(&agent);
                    }
                    tracing::debug!(
                        "cli_driver: StreamDone with no pending spawns, ending conversation"
                    );
                    response_finished = true;
                } else {
                    tracing::debug!(
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: StreamDone but subagents still outstanding, waiting for them"
                    );
                }
            }
            Some(UiUpdate::StreamCancelled) => {
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                write_cli_styled_line(
                    out,
                    "---[ Interrupted ]---",
                    INTERRUPTED_COLOR,
                    false,
                    output_options,
                )?;
                if pending_spawn_call_ids.is_empty() {
                    tracing::debug!(
                        "cli_driver: StreamCancelled with no pending spawns, ending conversation"
                    );
                    response_finished = true;
                } else {
                    tracing::debug!(
                        pending = pending_spawn_call_ids.len(),
                        "cli_driver: StreamCancelled but subagents still outstanding, waiting for them"
                    );
                }
            }
            // A stop with no stream in flight ends with `AgentIdle` instead of
            // `StreamCancelled`, so an interrupted run also ends here.
            Some(UiUpdate::AgentIdle) => {
                if interrupt_requested && pending_spawn_call_ids.is_empty() {
                    response_finished = true;
                }
            }
            Some(UiUpdate::Error { message, run_id }) => {
                match run_id {
                    None => {
                        flush_stream_section(
                            out,
                            &mut current_section,
                            &mut thinking_started_at,
                            output_options,
                        )?;
                        write_cli_styled_line(
                            out,
                            "---[ Error ]---",
                            ERROR_COLOR,
                            false,
                            output_options,
                        )?;
                        writeln!(out, "{message}")?;
                        response_finished = true;
                    }
                    // A subagent's own stream-level error (e.g. its max-tool-use-
                    // iterations guard tripped) is not the root conversation
                    // ending — the root learns the spawn finished via the
                    // spawn tool's own `ToolDone`/`ToolError` (see above), not
                    // this generic error. Headless mode has no separate subagent
                    // transcript to render into, so just surface it tagged,
                    // without ending the response.
                    Some(run_id) => {
                        write_cli_styled_line(
                            out,
                            &format!("[subagent {run_id}] error: {message}"),
                            ERROR_COLOR,
                            false,
                            output_options,
                        )?;
                    }
                }
            }
            // Headless mode has no separate subagent transcript to render into
            // (see drill-in conversation model, Phase 6.10, TUI-only): subagent
            // tool events already arrive tagged with `run_id` and are
            // rendered inline by the arms above, so only the subagent's own
            // free-form text/thinking tokens and lifecycle markers are
            // silently ignored here.
            Some(
                UiUpdate::SubagentStarted { .. }
                | UiUpdate::SubagentEngineConfigured { .. }
                | UiUpdate::SubagentToken { .. }
                | UiUpdate::SubagentDone { .. },
            ) => {}
            None => {
                flush_stream_section(
                    out,
                    &mut current_section,
                    &mut thinking_started_at,
                    output_options,
                )?;
                response_finished = true;
            }
        }
    }

    // Dropping the AgentLoop closes cmd_tx and lets the agent task finish.
    drop(agent);

    handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");
    if let Some(title_driver) = &title_driver {
        tokio::select! {
            () = title_driver.wait_for_pending_tasks() => {}
            Some(()) = stop_signals.recv() => {
                // SIGINT/SIGTERM during title generation: stop waiting and exit.
                outcome.record_interruption();
            }
        }
    }
    Ok(outcome)
}
