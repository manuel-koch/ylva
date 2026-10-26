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

//! Subagent delegation: `SubagentManager` owns the lifecycle of the
//! running subagents plus an unbounded FIFO of queued spawn requests. A
//! parent `AgentLoop` holds at most one `SubagentManager` (present only for
//! `Role::Root` agents whose `AgentConfig::can_spawn` is `true`). Up to the
//! configured `max_concurrent_subagents` cap runs at the same time across the
//! whole agent tree; spawns beyond the cap are enqueued and started in FIFO
//! order as running subagents finish. `None` means unlimited.
//!
//! `SubagentManager` itself knows nothing about engines, registries, or
//! `AgentFactory` — actually constructing and running a subagent `AgentLoop` is
//! delegated to a `SpawnFn` closure supplied by the caller (`AgentFactory`),
//! keeping this module a plain scheduling/queueing primitive that is cheap to
//! unit test in isolation.

use std::collections::VecDeque;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentCommand, UiUpdate};
use crate::store::Store;

/// A request to run a subagent, either starting immediately or queued
/// behind an already-running subagent.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Identifies this subagent run; distinct from `call_id`.
    pub run_id: Uuid,
    /// The `spawn_subagent` tool-call id this run is tied to. The eventual
    /// `FinishResult` is injected into the parent conversation as a hidden
    /// harness notice referencing this id — not a second `role:"tool"` message,
    /// since exactly one is allowed per `tool_call_id`.
    pub call_id: Uuid,
    /// The task description sent to the subagent as its initial user turn.
    pub task: String,
    /// Optional briefing/context injected as an additional system message in
    /// the subagent's context, ahead of the task.
    pub briefing: Option<String>,
    /// Name of the agent-type configuration the subagent should run as.
    pub agent_type: String,
    /// Short (3-6 word) label summarising what the subagent is doing,
    /// shown as the second line of the drill-in conversation header.
    pub label: String,
}

/// Identifies whether a subagent outcome came from the subagent itself or was
/// produced by Ylva's harness. The latter is serialized as `runtime` for
/// compatibility; it does not refer to an Engine runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishResultOrigin {
    /// The subagent deliberately called `agent_finish`.
    Agent,
    /// The harness produced the outcome because the subagent could not complete
    /// normally (for example, an unknown engine or a panic).
    #[default]
    Runtime,
}

/// Outcome of a completed, cancelled, or unstructured-fallback subagent run.
#[derive(Debug, Clone)]
pub struct FinishResult {
    pub run_id: Uuid,
    pub call_id: Uuid,
    /// Determines whether user-authored Markdown is appropriate for an
    /// error: only a subagent-reported outcome is model-authored content.
    pub origin: FinishResultOrigin,
    /// `"success"`, `"failure"`, or `"unstructured"` (subagent never called
    /// `agent_finish`; last assistant text was captured as a fallback).
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub sources: Vec<String>,
}

impl FinishResult {
    /// Render as the JSON object injected as the `spawn_subagent` tool result
    /// text once the subagent finishes.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "result": self.result,
            "error": self.error,
            "sources": self.sources,
        })
    }

    /// Render the structured hidden harness event used by the parent model to
    /// correlate this terminal outcome and decide whether aggregation is
    /// complete.
    pub fn to_completion_json(&self, label: &str, outstanding_count: usize) -> serde_json::Value {
        serde_json::json!({
            "event": "subagent_completed",
            "run_id": self.run_id.to_string(),
            "tool_call_id": self.call_id.to_string(),
            "label": label,
            "status": self.status,
            "result": self.result,
            "error": self.error,
            "sources": self.sources,
            "outstanding_count": outstanding_count,
            "all_done": outstanding_count == 0,
        })
    }

    /// Renders this outcome as tool-bubble content sections for the TUI:
    /// `"failure"` shows `error` under an `"Error"` label; `"success"` and
    /// `"unstructured"` both show `result` under an `"Output"` label (the
    /// unstructured fallback captures the subagent's last assistant text into
    /// the same field — see `AgentFactory::run_subagent`); a non-empty
    /// `sources` list is always appended as its own `"Sources"` section.
    /// Agent-reported results, errors, and sources are marked
    /// `file_extension: "md"` so
    /// `tui_widgets.rs` renders model-authored content through the project's
    /// Markdown renderer. Harness diagnostics remain plain text so identifiers
    /// and punctuation are displayed literally. Empty/absent fields produce no
    /// section.
    pub fn content_sections(&self) -> Vec<crate::agent::tools::Section> {
        use crate::agent::tools::{ContentSection, Section};

        let markdown_section = |label: &str, content: String| {
            Section::Content(ContentSection {
                label: Some(label.to_string()),
                content,
                file_extension: Some("md".to_string()),
                line_number_offset: None,
            })
        };
        let plain_text_section = |label: &str, content: String| {
            Section::Content(ContentSection {
                label: Some(label.to_string()),
                content,
                file_extension: None,
                line_number_offset: None,
            })
        };

        let mut sections = Vec::new();
        if self.status == "failure" {
            if let Some(error) = self.error.as_ref().filter(|s| !s.is_empty()) {
                let section = match self.origin {
                    FinishResultOrigin::Agent => markdown_section("Error", error.clone()),
                    FinishResultOrigin::Runtime => plain_text_section("Error", error.clone()),
                };
                sections.push(section);
            }
        } else if let Some(result) = self.result.as_ref().filter(|s| !s.is_empty()) {
            sections.push(markdown_section("Output", result.clone()));
        }
        if !self.sources.is_empty() {
            let bullet_list = self
                .sources
                .iter()
                .map(|source| format!("- {source}"))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(markdown_section("Sources", bullet_list));
        }
        sections
    }
}

/// Result of [`SubagentManager::spawn`]: whether the request started
/// immediately or was enqueued behind an active subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStatus {
    Started(Uuid),
    Queued(Uuid),
}

impl SpawnStatus {
    pub fn run_id(self) -> Uuid {
        match self {
            SpawnStatus::Started(id) | SpawnStatus::Queued(id) => id,
        }
    }
}

/// Constructs and runs a subagent `AgentLoop` for `request`, reporting its
/// eventual outcome via the `FinishResult` sender captured by the closure
/// (not passed here — see `AgentFactory`) and forwarding live tokens through
/// the given `UiUpdate` sender. Returns a [`SpawnOutcome`] carrying the
/// subagent's own `AgentCommand` sender (so the parent can route
/// `AgentCommand::ForSubagent` verdicts to it — see `running_command_sender`)
/// alongside the `JoinHandle` the manager uses only to know the task exists;
/// completion is observed through `FinishResult`, never by awaiting this
/// handle.
pub type SpawnFn = Box<
    dyn Fn(SpawnRequest, CancellationToken, mpsc::UnboundedSender<UiUpdate>) -> SpawnOutcome
        + Send
        + Sync,
>;

/// Return value of a [`SpawnFn`] call: the newly started (or failed) subagent's
/// command sender, plus the `JoinHandle` driving it to completion.
/// `command_sender` is `None` only when the subagent failed to construct (e.g.
/// an unknown engine name) — that failure is reported via `finish_tx`
/// (`FinishResult`) directly by the `SpawnFn`, so there is no live subagent to
/// route commands to.
pub struct SpawnOutcome {
    pub command_sender: Option<mpsc::Sender<AgentCommand>>,
    pub handle: JoinHandle<()>,
}

struct RunningSubagent {
    run_id: Uuid,
    call_id: Uuid,
    label: String,
    cancel: CancellationToken,
    /// When this subagent actually started (i.e. when `start` ran, not when the
    /// original `spawn` call was made — queued requests don't start the
    /// clock until they're dequeued). Used to compute the elapsed duration
    /// shown on the final `ToolDone` once the subagent finishes.
    started_at: std::time::Instant,
    /// The subagent's own `AgentCommand` sender, used by `running_command_sender`
    /// to route `AgentCommand::ForSubagent` verdicts to it. `None` when the
    /// subagent failed to construct (see `SpawnOutcome::command_sender`).
    command_sender: Option<mpsc::Sender<AgentCommand>>,
    handle: JoinHandle<()>,
}

/// Identifies one subagent run/spawn-call pair that `cancel_all` tore down —
/// either the actively running subagent or one still waiting in the queue.
/// Callers use `call_id` to resolve the corresponding tool-bubble/context
/// entry to a terminal state and `label` to correlate its lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledSpawn {
    pub run_id: Uuid,
    pub call_id: Uuid,
    pub label: String,
}

/// Schedules and tracks concurrently running subagents, with an unbounded
/// FIFO queue for spawn requests beyond the concurrency cap.
pub struct SubagentManager {
    /// Subagents currently running, in start order (deterministic iteration
    /// for `cancel_all`). Never longer than `max_concurrent_subagents`.
    running: Vec<RunningSubagent>,
    queue: VecDeque<SpawnRequest>,
    /// Cap on subagents in the running state at once; `None` is unlimited.
    max_concurrent_subagents: Option<usize>,
    subagent_ui_tx: mpsc::UnboundedSender<UiUpdate>,
    spawn_fn: SpawnFn,
    /// The **parent** session's store, when persistence is enabled: every
    /// subagent run appends a `subagent_start`/`subagent_result` pair to it.
    store: Option<Store>,
}

/// Builds the `UiUpdate::ToolProgress` title-argument pairs shown on the
/// `spawn_subagent` bubble while its subagent is in flight: `status` (`"running"`
/// or `"queued"`) plus the subagent's `run_id`.
fn spawn_progress_title_arguments(status: &str, run_id: Uuid) -> Vec<(String, serde_json::Value)> {
    vec![
        ("status".to_string(), serde_json::json!(status)),
        ("run_id".to_string(), serde_json::json!(run_id.to_string())),
    ]
}

impl SubagentManager {
    /// `max_concurrent_subagents` caps how many subagents may run at once
    /// (`None` is unlimited). Lifecycle updates go to `subagent_ui_tx`.
    pub fn new(
        subagent_ui_tx: mpsc::UnboundedSender<UiUpdate>,
        spawn_fn: SpawnFn,
        max_concurrent_subagents: Option<usize>,
    ) -> Self {
        SubagentManager {
            running: Vec::new(),
            queue: VecDeque::new(),
            max_concurrent_subagents: max_concurrent_subagents.filter(|limit| *limit > 0),
            subagent_ui_tx,
            spawn_fn,
            store: None,
        }
    }

    /// The configured cap on concurrently running subagents; `None` is unlimited.
    pub fn max_concurrent_subagents(&self) -> Option<usize> {
        self.max_concurrent_subagents
    }

    /// Enables persistence: `subagent_start`/`subagent_result` records for every
    /// subagent run are appended to the parent session identified by `store`.
    pub fn with_store(mut self, store: Store) -> Self {
        self.store = Some(store);
        self
    }

    pub(crate) fn set_store(&mut self, store: Option<Store>) {
        self.store = store;
    }

    /// The parent session's store, when persistence is enabled. Used by the
    /// parent `AgentLoop` to persist a finished subagent's own transcript.
    pub fn store(&self) -> Option<Store> {
        self.store.clone()
    }

    /// Append the `subagent_result` record for a finished subagent run to the
    /// parent session. Called by the parent `AgentLoop` when a
    /// [`FinishResult`] arrives, before `on_subagent_finished` clears the
    /// running slot.
    pub fn record_subagent_result(&self, finish_result: &FinishResult) {
        let Some(store) = &self.store else {
            return;
        };
        store.record_subagent_result(
            finish_result.run_id,
            finish_result.call_id,
            &finish_result.status,
            finish_result
                .result
                .clone()
                .or_else(|| finish_result.error.clone()),
            finish_result.sources.clone(),
            finish_result.origin,
        );
    }

    /// Whether at least one subagent is running.
    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Whether another subagent may start right now without exceeding the cap.
    pub fn has_free_slot(&self) -> bool {
        self.max_concurrent_subagents
            .is_none_or(|limit| self.running.len() < limit)
    }

    /// Elapsed time since the running subagent identified by `run_id` actually
    /// started (i.e. since `start`, not since the original `spawn` call —
    /// time spent queued behind the cap doesn't count). Returns `None` if
    /// `run_id` isn't a running subagent (e.g. it's stale, or it is still
    /// queued). Used to report a real duration on the final `ToolDone` once
    /// the subagent finishes, rather than a hardcoded zero.
    pub fn running_duration(&self, run_id: Uuid) -> Option<std::time::Duration> {
        self.running
            .iter()
            .find(|running| running.run_id == run_id)
            .map(|running| running.started_at.elapsed())
    }

    /// Number of requests waiting behind the concurrency cap.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Number of subagent runs that have not reached a terminal outcome: the
    /// running subagents plus the queued requests.
    pub fn outstanding_count(&self) -> usize {
        self.running.len() + self.queue.len()
    }

    /// Number of subagent runs that would remain after the specified run reaches
    /// a terminal outcome. Unknown or already-terminal run ids leave the
    /// current count unchanged.
    pub fn outstanding_count_after_completion(&self, run_id: Uuid) -> usize {
        let tracked = self
            .running
            .iter()
            .any(|subagent| subagent.run_id == run_id)
            || self.queue.iter().any(|request| request.run_id == run_id);
        self.outstanding_count()
            .saturating_sub(usize::from(tracked))
    }

    /// The running subagent's own `AgentCommand` sender, if `run_id` matches a
    /// running subagent. Used by the root `AgentLoop` to route an
    /// `AgentCommand::ForSubagent { run_id, .. }` verdict to the correct subagent
    /// loop. Returns `None` when `run_id` is stale (the subagent already
    /// finished, was cancelled, or never started — e.g. still queued, or
    /// failed to construct).
    pub fn running_command_sender(&self, run_id: Uuid) -> Option<mpsc::Sender<AgentCommand>> {
        self.running
            .iter()
            .find(|running| running.run_id == run_id)
            .and_then(|running| running.command_sender.clone())
    }

    /// Return the display label associated with a subagent run that has not
    /// reached a terminal outcome. The lookup covers both running and queued
    /// runs so completion notices can correlate results without depending on
    /// execution order.
    pub fn label_for(&self, run_id: Uuid) -> Option<&str> {
        if let Some(running) = self
            .running
            .iter()
            .find(|subagent| subagent.run_id == run_id)
        {
            return Some(running.label.as_str());
        }
        self.queue
            .iter()
            .find(|request| request.run_id == run_id)
            .map(|request| request.label.as_str())
    }

    /// Start `task`/`briefing`/`agent_type` as a new subagent run tied to
    /// `call_id` when a slot under the concurrency cap is free, or enqueue it
    /// (FIFO) otherwise. Reports the resulting `"running"`/`"queued"` status
    /// as a `UiUpdate::ToolProgress` — the call is accepted, not yet done; the
    /// eventual `ToolDone` is sent later, only once the subagent truly finishes
    /// (see `AgentLoop::run`'s `finish_rx` handling).
    pub fn spawn(
        &mut self,
        task: String,
        briefing: Option<String>,
        agent_type: String,
        label: String,
        call_id: Uuid,
    ) -> SpawnStatus {
        let run_id = Uuid::new_v4();
        let request = SpawnRequest {
            run_id,
            call_id,
            task,
            briefing,
            agent_type,
            label,
        };
        // Starting out of order is never allowed: a request may start directly
        // only when nothing is waiting ahead of it.
        if self.queue.is_empty() && self.has_free_slot() {
            self.start(request);
            SpawnStatus::Started(run_id)
        } else {
            self.send_lifecycle_update(
                run_id,
                "tool_progress_queued",
                UiUpdate::ToolProgress {
                    id: call_id,
                    title_arguments: spawn_progress_title_arguments("queued", run_id),
                },
            );
            self.queue.push_back(request);
            SpawnStatus::Queued(run_id)
        }
    }

    /// Sends a lifecycle update synchronously. The subagent UI channel is
    /// unbounded, so the send never blocks the root loop that drains it and
    /// never drops an update while the root is alive.
    fn send_lifecycle_update(&self, run_id: Uuid, kind: &'static str, update: UiUpdate) {
        if self.subagent_ui_tx.send(update).is_err() {
            tracing::debug!(
                %run_id,
                update_kind = kind,
                "skipped subagent lifecycle update: subagent UI channel is closed"
            );
        }
    }

    fn start(&mut self, request: SpawnRequest) {
        let run_id = request.run_id;
        let call_id = request.call_id;
        let label = request.label.clone();
        self.send_lifecycle_update(
            run_id,
            "subagent_started",
            UiUpdate::SubagentStarted {
                run_id,
                call_id,
                agent_type: request.agent_type.clone(),
                task: request.task.clone(),
                label: request.label.clone(),
            },
        );
        self.send_lifecycle_update(
            run_id,
            "tool_progress_running",
            UiUpdate::ToolProgress {
                id: call_id,
                title_arguments: spawn_progress_title_arguments("running", run_id),
            },
        );
        if let Some(store) = &self.store {
            store.record_subagent_start(
                run_id,
                call_id,
                &request.agent_type,
                &request.task,
                &request.label,
            );
        }
        let cancel = CancellationToken::new();
        let outcome = (self.spawn_fn)(request, cancel.clone(), self.subagent_ui_tx.clone());
        self.running.push(RunningSubagent {
            run_id,
            call_id,
            label,
            cancel,
            started_at: std::time::Instant::now(),
            command_sender: outcome.command_sender,
            handle: outcome.handle,
        });
    }

    /// Starts queued requests, in FIFO order, while a slot under the cap is
    /// free. Each started request reports its own `"running"` progress via
    /// `start`.
    fn start_queued_while_slots_free(&mut self) {
        while self.has_free_slot() {
            let Some(next) = self.queue.pop_front() else {
                break;
            };
            self.start(next);
        }
    }

    /// Called by the parent `AgentLoop` when a `FinishResult` for `run_id`
    /// arrives. Removes that run only (ignoring stale or unknown ids), then
    /// starts queued requests in FIFO order while slots are free.
    ///
    /// Returns the number of subagent runs still awaiting a terminal outcome
    /// after this result has been applied.
    pub fn on_subagent_finished(&mut self, run_id: Uuid) -> usize {
        let Some(position) = self
            .running
            .iter()
            .position(|subagent| subagent.run_id == run_id)
        else {
            return self.outstanding_count();
        };
        self.running.remove(position);
        self.start_queued_while_slots_free();
        self.outstanding_count()
    }

    /// Cancel every running subagent and drain the queue. Called when the parent
    /// agent stops or its context is cleared. Returns every cancelled
    /// run/call-id/label tuple: the running subagents in start order, then the
    /// drained queue entries in FIFO order. The caller resolves each one's
    /// tool bubble/context entry to a terminal state.
    ///
    /// Awaits every cancelled subagent's task before returning, so no subagent is
    /// still running (or still emitting updates) once the caller proceeds.
    pub async fn cancel_all(&mut self) -> Vec<CancelledSpawn> {
        let mut cancelled = Vec::new();
        let mut handles = Vec::new();
        for running in self.running.drain(..) {
            running.cancel.cancel();
            handles.push(running.handle);
            cancelled.push(CancelledSpawn {
                run_id: running.run_id,
                call_id: running.call_id,
                label: running.label,
            });
        }
        cancelled.extend(self.queue.drain(..).map(|request| CancelledSpawn {
            run_id: request.run_id,
            call_id: request.call_id,
            label: request.label,
        }));
        for handle in handles {
            // A panicked subagent surfaces through its own finish path; here we
            // only need the task to have stopped.
            let _ = handle.await;
        }
        cancelled
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Recorded `(request, cancel)` pairs from a test's `spawn_fn` stub.
    type RecordedSpawnCalls = Arc<Mutex<Vec<(SpawnRequest, CancellationToken)>>>;

    /// Builds a `spawn_fn` stub that records every request it is asked to
    /// start (in call order) and immediately returns a completed, no-op
    /// task. The returned `CancellationToken`s are also recorded so tests can
    /// assert a specific subagent's token was fired by `cancel_all`.
    fn recording_spawn_fn() -> (SpawnFn, RecordedSpawnCalls) {
        let calls: RecordedSpawnCalls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let spawn_fn: SpawnFn = Box::new(move |request, cancel, _subagent_ui_tx| {
            recorded.lock().unwrap().push((request, cancel));
            SpawnOutcome {
                command_sender: None,
                handle: tokio::spawn(async {}),
            }
        });
        (spawn_fn, calls)
    }

    fn test_manager() -> (SubagentManager, RecordedSpawnCalls) {
        test_manager_with_limit(Some(1))
    }

    fn test_manager_with_limit(
        max_concurrent_subagents: Option<usize>,
    ) -> (SubagentManager, RecordedSpawnCalls) {
        let (spawn_fn, calls) = recording_spawn_fn();
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        (
            SubagentManager::new(subagent_ui_tx, spawn_fn, max_concurrent_subagents),
            calls,
        )
    }

    fn spawn_task(manager: &mut SubagentManager, task: &str) -> SpawnStatus {
        manager.spawn(
            task.to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        )
    }

    #[tokio::test]
    async fn spawn_starts_immediately_when_idle() {
        let (mut manager, calls) = test_manager();
        let status = manager.spawn(
            "task".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert!(matches!(status, SpawnStatus::Started(_)));
        assert!(manager.is_running());
        assert_eq!(calls.lock().unwrap().len(), 1, "spawn_fn called once");
    }

    #[tokio::test]
    async fn subagent_start_and_subagent_result_are_appended_to_the_parent_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let parent_session_id = Uuid::new_v4();
        let store = Store::create(
            &chats_root,
            temp.path(),
            parent_session_id,
            "openrouter",
            "gpt-4o",
            None,
        )
        .expect("parent store created");
        let (mut manager, _calls) = test_manager();
        manager = manager.with_store(store.clone());

        let call_id = Uuid::new_v4();
        let status = manager.spawn(
            "investigate the flaky test".to_string(),
            None,
            "explorer".to_string(),
            "label".to_string(),
            call_id,
        );
        let SpawnStatus::Started(run_id) = status else {
            panic!("expected the spawn to start immediately")
        };
        manager.record_subagent_result(&FinishResult {
            run_id,
            call_id,
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("found it".to_string()),
            error: None,
            sources: vec!["src/agent.rs".to_string()],
        });
        store.flush().await;

        let lines = std::fs::read_to_string(store.path()).expect("session file readable");
        let events: Vec<crate::store::StoreEvent> = lines
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every line parses"))
            .collect();
        let start = events
            .iter()
            .find_map(|event| match event {
                crate::store::StoreEvent::SubagentStart {
                    id,
                    parent_session,
                    agent_type,
                    task,
                    ..
                } => Some((*id, *parent_session, agent_type.clone(), task.clone())),
                _ => None,
            })
            .expect("a subagent_start record was written");
        assert_eq!(start.0, run_id);
        assert_eq!(start.1, parent_session_id, "linked to the parent session");
        assert_eq!(start.2, "explorer");
        assert_eq!(start.3, "investigate the flaky test");

        let result = events
            .iter()
            .find_map(|event| match event {
                crate::store::StoreEvent::SubagentResult {
                    id,
                    parent_session,
                    status,
                    result,
                    sources,
                    ..
                } => Some((
                    *id,
                    *parent_session,
                    status.clone(),
                    result.clone(),
                    sources.clone(),
                )),
                _ => None,
            })
            .expect("a subagent_result record was written");
        assert_eq!(result.0, run_id);
        assert_eq!(result.1, parent_session_id);
        assert_eq!(result.2, "success");
        assert_eq!(result.3.as_deref(), Some("found it"));
        assert_eq!(result.4, vec!["src/agent.rs".to_string()]);
    }

    #[tokio::test]
    async fn a_manager_without_a_store_records_nothing_and_still_spawns() {
        let (mut manager, calls) = test_manager();
        manager.record_subagent_result(&FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: None,
            error: None,
            sources: vec![],
        });
        manager.spawn(
            "task".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spawn_returns_queued_when_a_subagent_is_already_running() {
        let (mut manager, calls) = test_manager();
        let first = manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "first label".to_string(),
            Uuid::new_v4(),
        );
        let second = manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "second label".to_string(),
            Uuid::new_v4(),
        );
        let SpawnStatus::Started(first_run_id) = first else {
            panic!("expected first spawn to start immediately")
        };
        let SpawnStatus::Queued(second_run_id) = second else {
            panic!("expected second spawn to be queued")
        };
        assert_eq!(manager.outstanding_count(), 2);
        assert_eq!(manager.outstanding_count_after_completion(first_run_id), 1);
        assert_eq!(manager.outstanding_count_after_completion(second_run_id), 1);
        assert_eq!(manager.label_for(first_run_id), Some("first label"));
        assert_eq!(manager.label_for(second_run_id), Some("second label"));
        assert_eq!(manager.queue_len(), 1);
        // Only the first request actually started a task.
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn multiple_queued_spawns_are_drained_in_fifo_order_after_finish() {
        let (mut manager, calls) = test_manager();
        let first = manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        let SpawnStatus::Started(first_run_id) = first else {
            panic!("expected first spawn to start immediately")
        };
        manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        manager.spawn(
            "third".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert_eq!(manager.queue_len(), 2);

        manager.on_subagent_finished(first_run_id);
        assert_eq!(manager.queue_len(), 1, "second request now running");
        assert!(manager.is_running());

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2, "first and second requests started");
        assert_eq!(recorded[0].0.task, "first");
        assert_eq!(recorded[1].0.task, "second");
    }

    #[tokio::test]
    async fn on_subagent_finished_ignores_stale_run_id() {
        let (mut manager, _calls) = test_manager();
        manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert!(manager.is_running());
        manager.on_subagent_finished(Uuid::new_v4()); // unrelated / stale id
        assert!(
            manager.is_running(),
            "a mismatched run_id must not clear the running subagent"
        );
    }

    #[tokio::test]
    async fn running_duration_returns_elapsed_time_for_the_current_run_id() {
        let (mut manager, _calls) = test_manager();
        let SpawnStatus::Started(run_id) = manager.spawn(
            "task".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        ) else {
            panic!("expected the spawn to start immediately")
        };

        let duration = manager
            .running_duration(run_id)
            .expect("running subagent's duration should be available");
        assert!(duration >= std::time::Duration::ZERO);
    }

    #[tokio::test]
    async fn running_duration_is_none_for_a_stale_or_mismatched_run_id() {
        let (mut manager, _calls) = test_manager();
        manager.spawn(
            "task".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );

        assert_eq!(manager.running_duration(Uuid::new_v4()), None);
    }

    #[tokio::test]
    async fn running_duration_is_none_when_no_subagent_is_running() {
        let (manager, _calls) = test_manager();
        assert_eq!(manager.running_duration(Uuid::new_v4()), None);
    }

    #[tokio::test]
    async fn spawn_emits_tool_progress_with_spawned_status_when_started_immediately() {
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(1));
        let call_id = Uuid::new_v4();

        let status = manager.spawn(
            "task".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            call_id,
        );
        let SpawnStatus::Started(run_id) = status else {
            panic!("expected the spawn to start immediately")
        };

        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::SubagentStarted { run_id: started_run_id, agent_type, task, .. })
                if started_run_id == run_id && agent_type == "default" && task == "task"
        ));
        match subagent_ui_rx.recv().await {
            Some(UiUpdate::ToolProgress {
                id,
                title_arguments,
            }) => {
                assert_eq!(id, call_id);
                assert_eq!(
                    title_arguments,
                    vec![
                        ("status".to_string(), serde_json::json!("running")),
                        ("run_id".to_string(), serde_json::json!(run_id.to_string())),
                    ]
                );
            }
            other => panic!("expected a ToolProgress update, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_emits_tool_progress_with_queued_status_when_a_subagent_is_already_running() {
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(1));
        let first_call_id = Uuid::new_v4();
        let second_call_id = Uuid::new_v4();

        manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            first_call_id,
        );
        let second_status = manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            second_call_id,
        );
        let SpawnStatus::Queued(queued_run_id) = second_status else {
            panic!("expected the second spawn to be queued")
        };

        // First update is the immediately-started first subagent's lifecycle
        // marker, followed by its "running" progress.
        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::SubagentStarted { .. })
        ));
        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::ToolProgress { id, .. }) if id == first_call_id
        ));
        // Second progress update reports the second call as queued.
        match subagent_ui_rx.recv().await {
            Some(UiUpdate::ToolProgress {
                id,
                title_arguments,
            }) => {
                assert_eq!(id, second_call_id);
                assert_eq!(
                    title_arguments,
                    vec![
                        ("status".to_string(), serde_json::json!("queued")),
                        (
                            "run_id".to_string(),
                            serde_json::json!(queued_run_id.to_string())
                        ),
                    ]
                );
            }
            other => panic!("expected a ToolProgress update, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn on_subagent_finished_emits_tool_progress_for_the_newly_started_queued_request() {
        // Regression test: starting a queued request from `on_subagent_finished`
        // must report its own "running" progress update — previously this
        // path emitted no event at all, leaving the queued bubble stuck.
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(1));
        let first_call_id = Uuid::new_v4();
        let second_call_id = Uuid::new_v4();

        let SpawnStatus::Started(first_run_id) = manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            first_call_id,
        ) else {
            panic!("expected the first spawn to start immediately")
        };
        manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            second_call_id,
        );

        // Drain the three updates already emitted by the spawns above: the
        // first subagent's `SubagentStarted` + "running" `ToolProgress`, and
        // the second (queued) subagent's "queued" `ToolProgress`.
        subagent_ui_rx.recv().await;
        subagent_ui_rx.recv().await;
        subagent_ui_rx.recv().await;

        manager.on_subagent_finished(first_run_id);

        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::SubagentStarted { .. })
        ));
        match subagent_ui_rx.recv().await {
            Some(UiUpdate::ToolProgress { id, title_arguments }) => {
                assert_eq!(id, second_call_id);
                let status = title_arguments
                    .iter()
                    .find(|(key, _)| key == "status")
                    .map(|(_, value)| value.clone());
                assert_eq!(status, Some(serde_json::json!("running")));
            }
            other => panic!(
                "expected a ToolProgress update for the newly-started queued request, got: {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn cancel_all_fires_subagent_token_and_empties_queue() {
        let (mut manager, calls) = test_manager();
        let first_call_id = Uuid::new_v4();
        let second_call_id = Uuid::new_v4();
        manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            first_call_id,
        );
        manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            second_call_id,
        );
        assert_eq!(manager.queue_len(), 1);

        let cancelled = manager.cancel_all().await;
        assert_eq!(
            cancelled.iter().map(|c| c.call_id).collect::<Vec<_>>(),
            vec![first_call_id, second_call_id],
            "the running subagent must be reported first, then queued entries in FIFO order"
        );
        assert_eq!(manager.queue_len(), 0);
        assert!(!manager.is_running());

        let recorded = calls.lock().unwrap();
        assert!(
            recorded[0].1.is_cancelled(),
            "the running subagent's token must be cancelled"
        );
    }

    #[tokio::test]
    async fn cancel_all_returns_none_when_nothing_is_running() {
        let (mut manager, _calls) = test_manager();
        assert!(manager.cancel_all().await.is_empty());
    }

    #[tokio::test]
    async fn limit_stores_finite_and_unlimited_values() {
        let (manager, _calls) = test_manager_with_limit(Some(3));
        assert_eq!(manager.max_concurrent_subagents(), Some(3));
        let (unlimited, _calls) = test_manager_with_limit(None);
        assert_eq!(unlimited.max_concurrent_subagents(), None);
        let (zero, _calls) = test_manager_with_limit(Some(0));
        assert_eq!(zero.max_concurrent_subagents(), None);
    }

    #[tokio::test]
    async fn limit_three_starts_three_subagents_and_queues_the_rest() {
        let (mut manager, calls) = test_manager_with_limit(Some(3));
        let statuses: Vec<SpawnStatus> = (0..5)
            .map(|index| spawn_task(&mut manager, &format!("task {index}")))
            .collect();

        let started = statuses
            .iter()
            .filter(|status| matches!(status, SpawnStatus::Started(_)))
            .count();
        let queued = statuses
            .iter()
            .filter(|status| matches!(status, SpawnStatus::Queued(_)))
            .count();
        assert_eq!((started, queued), (3, 2));
        assert_eq!(calls.lock().unwrap().len(), 3, "only three subagents start");
        assert_eq!(manager.queue_len(), 2);
        assert_eq!(manager.outstanding_count(), 5);
    }

    #[tokio::test]
    async fn finishing_one_of_three_running_subagents_starts_exactly_one_queued_subagent_in_fifo_order(
    ) {
        let (mut manager, calls) = test_manager_with_limit(Some(3));
        let first = spawn_task(&mut manager, "first");
        spawn_task(&mut manager, "second");
        spawn_task(&mut manager, "third");
        spawn_task(&mut manager, "fourth");
        spawn_task(&mut manager, "fifth");
        let SpawnStatus::Started(first_run_id) = first else {
            panic!("expected the first spawn to start immediately")
        };

        assert_eq!(manager.on_subagent_finished(first_run_id), 4);
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 4, "exactly one queued subagent started");
        assert_eq!(
            recorded[3].0.task, "fourth",
            "queued subagents start in FIFO order"
        );
        assert_eq!(manager.queue_len(), 1);
        assert_eq!(manager.outstanding_count(), 4);
    }

    #[tokio::test]
    async fn unlimited_limit_starts_every_spawn_immediately() {
        let (mut manager, calls) = test_manager_with_limit(None);
        for index in 0..5 {
            let status = spawn_task(&mut manager, &format!("task {index}"));
            assert!(matches!(status, SpawnStatus::Started(_)));
        }
        assert_eq!(calls.lock().unwrap().len(), 5);
        assert_eq!(manager.queue_len(), 0);
        assert_eq!(manager.outstanding_count(), 5);
    }

    #[tokio::test]
    async fn stale_or_unknown_run_id_does_not_free_a_slot() {
        let (mut manager, calls) = test_manager_with_limit(Some(2));
        spawn_task(&mut manager, "first");
        spawn_task(&mut manager, "second");
        spawn_task(&mut manager, "third");

        assert_eq!(manager.on_subagent_finished(Uuid::new_v4()), 3);
        assert_eq!(calls.lock().unwrap().len(), 2, "no queued subagent started");
        assert_eq!(manager.queue_len(), 1);
    }

    #[tokio::test]
    async fn outstanding_count_equals_running_plus_queued_at_each_step() {
        let (mut manager, _calls) = test_manager_with_limit(Some(2));
        let SpawnStatus::Started(first) = spawn_task(&mut manager, "first") else {
            panic!("expected start")
        };
        assert_eq!(manager.outstanding_count(), 1);
        spawn_task(&mut manager, "second");
        assert_eq!(manager.outstanding_count(), 2);
        spawn_task(&mut manager, "third");
        assert_eq!(manager.outstanding_count(), 3);
        manager.on_subagent_finished(first);
        assert_eq!(manager.outstanding_count(), 2);
    }

    #[tokio::test]
    async fn cancel_all_with_two_running_and_one_queued_cancels_each_subagent() {
        let (mut manager, calls) = test_manager_with_limit(Some(2));
        spawn_task(&mut manager, "first");
        spawn_task(&mut manager, "second");
        spawn_task(&mut manager, "third");

        let cancelled = manager.cancel_all().await;
        assert_eq!(cancelled.len(), 3, "one cancelled completion per subagent");
        assert_eq!(manager.outstanding_count(), 0);
        let recorded = calls.lock().unwrap();
        assert!(recorded[0].1.is_cancelled());
        assert!(recorded[1].1.is_cancelled());
    }

    #[tokio::test]
    async fn running_command_sender_is_none_for_a_queued_run() {
        let (mut manager, _calls) = test_manager_with_limit(Some(1));
        spawn_task(&mut manager, "first");
        let SpawnStatus::Queued(queued_run_id) = spawn_task(&mut manager, "second") else {
            panic!("expected the second spawn to be queued")
        };
        assert!(manager.running_command_sender(queued_run_id).is_none());
        assert_eq!(manager.label_for(queued_run_id), Some("label"));
    }

    #[tokio::test]
    async fn lifecycle_updates_are_never_dropped_under_volume() {
        let (spawn_fn, calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager = SubagentManager::new(subagent_ui_tx, spawn_fn, None);

        // Well past the former bounded capacity, with nothing draining yet.
        const SPAWN_COUNT: usize = 1500;
        for index in 0..SPAWN_COUNT {
            spawn_task(&mut manager, &format!("task-{index}"));
        }

        assert_eq!(
            calls.lock().unwrap().len(),
            SPAWN_COUNT,
            "every spawn must still start a subagent"
        );
        let mut received_update_count = 0;
        while subagent_ui_rx.try_recv().is_ok() {
            received_update_count += 1;
        }
        assert!(
            received_update_count >= SPAWN_COUNT,
            "expected at least one lifecycle update per spawn, got {received_update_count}"
        );
    }

    #[test]
    fn finish_result_to_json_carries_all_fields() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("done".to_string()),
            error: None,
            sources: vec!["a.txt".to_string()],
        };
        let json = result.to_json();
        assert_eq!(json["status"], "success");
        assert_eq!(json["result"], "done");
        assert!(json["error"].is_null());
        assert_eq!(json["sources"], serde_json::json!(["a.txt"]));
    }

    #[test]
    fn finish_result_completion_json_carries_correlation_and_aggregation_fields() {
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let result = FinishResult {
            run_id,
            call_id,
            origin: FinishResultOrigin::Agent,
            status: "failure".to_string(),
            result: None,
            error: Some("failed".to_string()),
            sources: vec!["a.txt".to_string()],
        };

        let json = result.to_completion_json("Inspect files", 2);

        assert_eq!(json["event"], "subagent_completed");
        assert_eq!(json["run_id"], run_id.to_string());
        assert_eq!(json["tool_call_id"], call_id.to_string());
        assert_eq!(json["label"], "Inspect files");
        assert_eq!(json["status"], "failure");
        assert_eq!(json["error"], "failed");
        assert_eq!(json["sources"], serde_json::json!(["a.txt"]));
        assert_eq!(json["outstanding_count"], 2);
        assert_eq!(json["all_done"], false);
    }

    fn content_section_text(section: &crate::agent::tools::Section) -> (&str, &str, &str) {
        match section {
            crate::agent::tools::Section::Content(content) => (
                content.label.as_deref().unwrap_or(""),
                content.content.as_str(),
                content.file_extension.as_deref().unwrap_or(""),
            ),
            _ => panic!("expected a Content section"),
        }
    }

    #[test]
    fn content_sections_renders_success_result_as_markdown() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("# All done\n\nEverything worked.".to_string()),
            error: None,
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, file_extension) = content_section_text(&sections[0]);
        assert_eq!(label, "Output");
        assert_eq!(content, "# All done\n\nEverything worked.");
        assert_eq!(file_extension, "md");
    }

    #[test]
    fn content_sections_renders_unstructured_result_as_markdown() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Runtime,
            status: "unstructured".to_string(),
            result: Some("last assistant text".to_string()),
            error: None,
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, _) = content_section_text(&sections[0]);
        assert_eq!(label, "Output");
        assert_eq!(content, "last assistant text");
    }

    #[test]
    fn content_sections_renders_failure_error_as_plain_text_instead_of_markdown() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Runtime,
            status: "failure".to_string(),
            result: Some("should be ignored".to_string()),
            error: Some("unknown engine: local-discovery_".to_string()),
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, file_extension) = content_section_text(&sections[0]);
        assert_eq!(label, "Error");
        assert_eq!(content, "unknown engine: local-discovery_");
        assert!(
            file_extension.is_empty(),
            "diagnostic errors must not be interpreted as Markdown"
        );
    }

    #[test]
    fn content_sections_renders_agent_reported_failure_as_markdown() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "failure".to_string(),
            result: None,
            error: Some("**The subagent could not complete the task.**".to_string()),
            sources: vec![],
        };
        let sections = result.content_sections();
        let (_, _, file_extension) = content_section_text(&sections[0]);
        assert_eq!(file_extension, "md");
    }

    #[test]
    fn content_sections_appends_sources_as_bullet_list() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("done".to_string()),
            error: None,
            sources: vec!["a.rs".to_string(), "b.rs".to_string()],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 2);
        let (result_label, ..) = content_section_text(&sections[0]);
        assert_eq!(result_label, "Output");
        let (sources_label, sources_content, _) = content_section_text(&sections[1]);
        assert_eq!(sources_label, "Sources");
        assert_eq!(sources_content, "- a.rs\n- b.rs");
    }

    #[test]
    fn content_sections_empty_when_no_result_error_or_sources() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            origin: FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: None,
            error: None,
            sources: vec![],
        };
        assert!(result.content_sections().is_empty());
    }
}
