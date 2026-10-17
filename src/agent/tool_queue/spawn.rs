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

use super::*;
/// Resolve an approved `spawn_subagent` call immediately against
/// `subagent_manager`: unlike ordinary tools, spawning never executes the
/// call's own `ToolCall::run` (which has no access to the manager) — it
/// starts (or enqueues) the subagent directly and pushes a structured admission
/// event as the call's tool result right away. This is *not* the call
/// finishing: `SubagentManager::spawn` reports that status as a
/// `UiUpdate::ToolProgress` (bubble stays `Running`), and `content_sections`
/// (the task/briefing shown to the user) are stashed in
/// `spawn_content_sections` so the eventual real `ToolDone` — sent only
/// once the subagent actually finishes, see `AgentLoop::run`'s `finish_rx`
/// handling — can still show them alongside the subagent's result.
pub(in crate::agent) async fn resolve_spawn_subagent(
    pending: PendingTool,
    subagent_manager: &mut Option<crate::subagent_manager::SubagentManager>,
    context: &mut Context,
    ui_tx: &mpsc::Sender<UiUpdate>,
    spawn_content_sections: &mut HashMap<Uuid, Vec<crate::agent::tools::Section>>,
) {
    let call_id = pending.call.id();
    let name = pending.call.name().to_string();
    let arguments = pending.call.arguments().clone();
    let content_sections = pending.call.pending_content_sections();
    let title_arguments = pending.call.title_arguments();
    let _ = ui_tx
        .send(UiUpdate::ToolStart {
            name: name.clone(),
            id: call_id,
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments,
            run_id: None,
        })
        .await;

    let task = arguments
        .get("task")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let briefing = arguments
        .get("briefing")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let agent_type = arguments
        .get("agent_type")
        .and_then(|value| value.as_str())
        .unwrap_or(tools::spawn_subagent::DEFAULT_SUBAGENT_TYPE)
        .to_string();
    let label = arguments
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    let (status_text, run_id, outstanding_count) = match subagent_manager.as_mut() {
        Some(manager) => {
            let status = manager.spawn(task, briefing, agent_type, label.clone(), call_id);
            let outstanding_count = manager.outstanding_count();
            let (status_text, run_id) = match status {
                crate::subagent_manager::SpawnStatus::Started(run_id) => ("running", run_id),
                crate::subagent_manager::SpawnStatus::Queued(run_id) => ("queued", run_id),
            };
            (status_text, run_id, outstanding_count)
        }
        None => {
            // Structurally unreachable: `spawn_subagent` is only ever
            // registered when a `SubagentManager` is wired. Fail safe
            // rather than panic.
            let error =
                "spawn_subagent is unavailable: this agent cannot spawn subagents".to_string();
            tracing::error!(tool_call_id = %call_id, "spawn_subagent resolved with no SubagentManager wired");
            context.push_tool_error(
                call_id,
                name.clone(),
                error.clone(),
                None,
                std::time::Duration::ZERO,
                content_sections.clone(),
            );
            let _ = ui_tx
                .send(UiUpdate::ToolError {
                    id: call_id,
                    error,
                    duration: std::time::Duration::ZERO,
                    content_sections,
                    run_id: None,
                })
                .await;
            return;
        }
    };

    spawn_content_sections.insert(call_id, content_sections);
    tracing::info!(tool_call_id = %call_id, status = status_text, %run_id, "spawn_subagent resolved");
    let result_value = serde_json::json!({
        "event": "subagent_accepted",
        "tool_call_id": call_id.to_string(),
        "label": label,
        "status": status_text,
        "run_id": run_id.to_string(),
        "outstanding_count": outstanding_count,
        "all_done": false,
    });
    let result_text = serde_json::to_string(&result_value).unwrap_or_default();
    context.push_tool_result(ToolResult {
        id: call_id,
        name,
        result: result_text,
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: spawn_content_sections
            .get(&call_id)
            .cloned()
            .unwrap_or_default(),
        duration: std::time::Duration::ZERO,
    });
    // No `ToolDone` here: `SubagentManager::spawn` already reported the
    // "running"/"queued" status via `UiUpdate::ToolProgress`. The bubble
    // stays `Running` until the subagent truly finishes.
}
