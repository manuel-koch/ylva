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

//! One-level agent delegation: `ChildAgentManager` owns the lifecycle of a
//! single running child agent plus an unbounded FIFO of queued spawn
//! requests. A parent `AgentLoop` holds at most one `ChildAgentManager`
//! (present only for `Role::Root` agents whose `AgentConfig::can_spawn` is
//! `true`); the manager never runs more than one child concurrently — every
//! additional `spawn` call while a child is active is enqueued and started
//! automatically once the running child finishes.
//!
//! `ChildAgentManager` itself knows nothing about engines, registries, or
//! `AgentFactory` — actually constructing and running a child `AgentLoop` is
//! delegated to a `SpawnFn` closure supplied by the caller (`AgentFactory`),
//! keeping this module a plain scheduling/queueing primitive that is cheap to
//! unit test in isolation.

use std::collections::VecDeque;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::{AgentCommand, UiUpdate};

/// A request to run a child agent, either starting immediately or queued
/// behind an already-running child.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Identifies this child run; distinct from `call_id`.
    pub run_id: Uuid,
    /// The `spawn_subagent` tool-call id this run is tied to. The eventual
    /// `FinishResult` is injected into the parent conversation as a trailing
    /// `system` message referencing this id — not a second `role:"tool"`
    /// message, since exactly one is allowed per `tool_call_id`.
    pub call_id: Uuid,
    /// The task description sent to the child as its initial user turn.
    pub task: String,
    /// Optional briefing/context injected as an additional system message in
    /// the child's context, ahead of the task.
    pub briefing: Option<String>,
    /// Name of the agent-type configuration the child should run as.
    pub agent_type: String,
    /// Short (3-6 word) label summarising what the child agent is doing,
    /// shown as the second line of the drill-in conversation header.
    pub label: String,
}

/// Outcome of a completed, cancelled, or unstructured-fallback child run.
#[derive(Debug, Clone)]
pub struct FinishResult {
    pub run_id: Uuid,
    pub call_id: Uuid,
    /// `"success"`, `"failure"`, or `"unstructured"` (child never called
    /// `agent_finish`; last assistant text was captured as a fallback).
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub sources: Vec<String>,
}

impl FinishResult {
    /// Render as the JSON object injected as the `spawn_subagent` tool result
    /// text once the child finishes.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "result": self.result,
            "error": self.error,
            "sources": self.sources,
        })
    }

    /// Renders this outcome as tool-bubble content sections for the TUI:
    /// `"failure"` shows `error` under an `"Error"` label; `"success"` and
    /// `"unstructured"` both show `result` under a `"Result"` label (the
    /// unstructured fallback captures the child's last assistant text into
    /// the same field — see `AgentFactory::run_child`); a non-empty
    /// `sources` list is always appended as its own `"Sources"` section.
    /// Every section is marked `file_extension: "md"` so `tui_widgets.rs` renders it
    /// through the project's Markdown renderer instead of raw JSON/syntect
    /// highlighting. Empty/absent fields produce no section.
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

        let mut sections = Vec::new();
        if self.status == "failure" {
            if let Some(error) = self.error.as_ref().filter(|s| !s.is_empty()) {
                sections.push(markdown_section("Error", error.clone()));
            }
        } else if let Some(result) = self.result.as_ref().filter(|s| !s.is_empty()) {
            sections.push(markdown_section("Result", result.clone()));
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

/// Result of [`ChildAgentManager::spawn`]: whether the request started
/// immediately or was enqueued behind an active child.
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

/// Constructs and runs a child `AgentLoop` for `request`, reporting its
/// eventual outcome via the `FinishResult` sender captured by the closure
/// (not passed here — see `AgentFactory`) and forwarding live tokens through
/// the given `UiUpdate` sender. Returns a [`SpawnOutcome`] carrying the
/// child's own `AgentCommand` sender (so the parent can route
/// `AgentCommand::ForChild` verdicts to it — see `running_command_sender`)
/// alongside the `JoinHandle` the manager uses only to know the task exists;
/// completion is observed through `FinishResult`, never by awaiting this
/// handle.
pub type SpawnFn = Box<
    dyn Fn(SpawnRequest, CancellationToken, mpsc::Sender<UiUpdate>) -> SpawnOutcome + Send + Sync,
>;

/// Return value of a [`SpawnFn`] call: the newly started (or failed) child's
/// command sender, plus the `JoinHandle` driving it to completion.
/// `command_sender` is `None` only when the child failed to construct (e.g.
/// an unknown engine name) — that failure is reported via `finish_tx`
/// (`FinishResult`) directly by the `SpawnFn`, so there is no live child to
/// route commands to.
pub struct SpawnOutcome {
    pub command_sender: Option<mpsc::Sender<AgentCommand>>,
    pub handle: JoinHandle<()>,
}

struct RunningChild {
    run_id: Uuid,
    call_id: Uuid,
    cancel: CancellationToken,
    /// When this child actually started (i.e. when `start` ran, not when the
    /// original `spawn` call was made — queued requests don't start the
    /// clock until they're dequeued). Used to compute the elapsed duration
    /// shown on the final `ToolDone` once the child finishes.
    started_at: std::time::Instant,
    /// The child's own `AgentCommand` sender, used by `running_command_sender`
    /// to route `AgentCommand::ForChild` verdicts to it. `None` when the
    /// child failed to construct (see `SpawnOutcome::command_sender`).
    command_sender: Option<mpsc::Sender<AgentCommand>>,
    #[expect(dead_code)] // kept alive only so the task is not detached early
    handle: JoinHandle<()>,
}

/// Identifies one child run/spawn-call pair that `cancel_all` tore down —
/// either the actively running child or one still waiting in the queue.
/// Callers use `call_id` to resolve the corresponding tool-bubble/context
/// entry to a terminal state instead of leaving it stuck mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelledSpawn {
    pub run_id: Uuid,
    pub call_id: Uuid,
}

/// Schedules and tracks at most one concurrently running child agent, with
/// an unbounded FIFO queue for additional spawn requests.
pub struct ChildAgentManager {
    running: Option<RunningChild>,
    queue: VecDeque<SpawnRequest>,
    subagent_ui_tx: mpsc::Sender<UiUpdate>,
    spawn_fn: SpawnFn,
}

/// Builds the `UiUpdate::ToolProgress` title-argument pairs shown on the
/// `spawn_subagent` bubble while its child is in flight: `status` (`"running"`
/// or `"queued"`) plus the child's `run_id`.
fn spawn_progress_title_arguments(status: &str, run_id: Uuid) -> Vec<(String, serde_json::Value)> {
    vec![
        ("status".to_string(), serde_json::json!(status)),
        ("run_id".to_string(), serde_json::json!(run_id.to_string())),
    ]
}

impl ChildAgentManager {
    pub fn new(subagent_ui_tx: mpsc::Sender<UiUpdate>, spawn_fn: SpawnFn) -> Self {
        ChildAgentManager {
            running: None,
            queue: VecDeque::new(),
            subagent_ui_tx,
            spawn_fn,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Elapsed time since the running child identified by `run_id` actually
    /// started (i.e. since `start`, not since the original `spawn` call —
    /// time spent queued behind another child doesn't count). Returns `None`
    /// if `run_id` doesn't match the currently running child (e.g. it's
    /// stale, or the child hasn't started yet because it's still queued).
    /// Used to report a real duration on the final `ToolDone` once the child
    /// finishes, rather than a hardcoded zero.
    pub fn running_duration(&self, run_id: Uuid) -> Option<std::time::Duration> {
        self.running
            .as_ref()
            .filter(|running| running.run_id == run_id)
            .map(|running| running.started_at.elapsed())
    }

    /// Number of requests waiting behind the currently running child.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// The running child's own `AgentCommand` sender, if `run_id` matches the
    /// currently running child. Used by the root `AgentLoop` to route an
    /// `AgentCommand::ForChild { run_id, .. }` verdict to the correct child
    /// loop. Returns `None` when `run_id` is stale (the child already
    /// finished, was cancelled, or never started — e.g. still queued, or
    /// failed to construct).
    pub fn running_command_sender(&self, run_id: Uuid) -> Option<mpsc::Sender<AgentCommand>> {
        self.running
            .as_ref()
            .filter(|running| running.run_id == run_id)
            .and_then(|running| running.command_sender.clone())
    }

    /// Start `task`/`briefing`/`agent_type` as a new child run tied to
    /// `call_id`, or enqueue it (FIFO) when a child is already running.
    /// Reports the resulting `"running"`/`"queued"` status as a
    /// `UiUpdate::ToolProgress` — the call is accepted, not yet done; the
    /// eventual `ToolDone` is sent later, only once the child truly
    /// finishes (see `AgentLoop::run`'s `finish_rx` handling).
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
        if self.is_running() {
            let _ = self.subagent_ui_tx.try_send(UiUpdate::ToolProgress {
                id: call_id,
                title_arguments: spawn_progress_title_arguments("queued", run_id),
            });
            self.queue.push_back(request);
            SpawnStatus::Queued(run_id)
        } else {
            self.start(request);
            SpawnStatus::Started(run_id)
        }
    }

    fn start(&mut self, request: SpawnRequest) {
        let run_id = request.run_id;
        let call_id = request.call_id;
        let _ = self.subagent_ui_tx.try_send(UiUpdate::ChildAgentStarted {
            run_id,
            agent_type: request.agent_type.clone(),
            task: request.task.clone(),
            label: request.label.clone(),
        });
        let _ = self.subagent_ui_tx.try_send(UiUpdate::ToolProgress {
            id: call_id,
            title_arguments: spawn_progress_title_arguments("running", run_id),
        });
        let cancel = CancellationToken::new();
        let outcome = (self.spawn_fn)(request, cancel.clone(), self.subagent_ui_tx.clone());
        self.running = Some(RunningChild {
            run_id,
            call_id,
            cancel,
            started_at: std::time::Instant::now(),
            command_sender: outcome.command_sender,
            handle: outcome.handle,
        });
    }

    /// Called by the parent `AgentLoop` when a `FinishResult` for `run_id`
    /// arrives. Clears the running slot (ignoring stale/mismatched ids) and
    /// starts the next queued request, if any (which reports its own
    /// `"running"` progress update via `start`, above).
    pub fn on_child_finished(&mut self, run_id: Uuid) {
        let matches_running = self.running.as_ref().is_some_and(|r| r.run_id == run_id);
        if !matches_running {
            return;
        }
        self.running = None;
        if let Some(next) = self.queue.pop_front() {
            self.start(next);
        }
    }

    /// Cancel the running child (if any) and drain the queue. Called when
    /// the parent agent stops. Returns every cancelled run/call-id pair
    /// (the running child, if any, followed by all drained queue entries in
    /// FIFO order) so the caller can resolve each one's tool bubble/context
    /// entry to a terminal state instead of leaving it stuck mid-flight.
    pub fn cancel_all(&mut self) -> Vec<CancelledSpawn> {
        let mut cancelled = Vec::new();
        if let Some(running) = self.running.take() {
            running.cancel.cancel();
            cancelled.push(CancelledSpawn {
                run_id: running.run_id,
                call_id: running.call_id,
            });
        }
        cancelled.extend(self.queue.drain(..).map(|request| CancelledSpawn {
            run_id: request.run_id,
            call_id: request.call_id,
        }));
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
    /// assert a specific child's token was fired by `cancel_all`.
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

    fn test_manager() -> (ChildAgentManager, RecordedSpawnCalls) {
        let (spawn_fn, calls) = recording_spawn_fn();
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(4);
        (ChildAgentManager::new(subagent_ui_tx, spawn_fn), calls)
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
    async fn spawn_returns_queued_when_a_child_is_already_running() {
        let (mut manager, calls) = test_manager();
        let first = manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        let second = manager.spawn(
            "second".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert!(matches!(first, SpawnStatus::Started(_)));
        assert!(matches!(second, SpawnStatus::Queued(_)));
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

        manager.on_child_finished(first_run_id);
        assert_eq!(manager.queue_len(), 1, "second request now running");
        assert!(manager.is_running());

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2, "first and second requests started");
        assert_eq!(recorded[0].0.task, "first");
        assert_eq!(recorded[1].0.task, "second");
    }

    #[tokio::test]
    async fn on_child_finished_ignores_stale_run_id() {
        let (mut manager, _calls) = test_manager();
        manager.spawn(
            "first".to_string(),
            None,
            "default".to_string(),
            "label".to_string(),
            Uuid::new_v4(),
        );
        assert!(manager.is_running());
        manager.on_child_finished(Uuid::new_v4()); // unrelated / stale id
        assert!(
            manager.is_running(),
            "a mismatched run_id must not clear the running child"
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
            .expect("running child's duration should be available");
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
    async fn running_duration_is_none_when_no_child_is_running() {
        let (manager, _calls) = test_manager();
        assert_eq!(manager.running_duration(Uuid::new_v4()), None);
    }

    #[tokio::test]
    async fn spawn_emits_tool_progress_with_spawned_status_when_started_immediately() {
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::channel(4);
        let mut manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
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
            Some(UiUpdate::ChildAgentStarted { run_id: started_run_id, agent_type, task, .. })
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
    async fn spawn_emits_tool_progress_with_queued_status_when_a_child_is_already_running() {
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::channel(4);
        let mut manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
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

        // First update is the immediately-started first child's lifecycle
        // marker, followed by its "running" progress.
        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::ChildAgentStarted { .. })
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
    async fn on_child_finished_emits_tool_progress_for_the_newly_started_queued_request() {
        // Regression test: starting a queued request from `on_child_finished`
        // must report its own "running" progress update — previously this
        // path emitted no event at all, leaving the queued bubble stuck.
        let (spawn_fn, _calls) = recording_spawn_fn();
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::channel(4);
        let mut manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
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
        // first child's `ChildAgentStarted` + "running" `ToolProgress`, and
        // the second (queued) child's "queued" `ToolProgress`.
        subagent_ui_rx.recv().await;
        subagent_ui_rx.recv().await;
        subagent_ui_rx.recv().await;

        manager.on_child_finished(first_run_id);

        assert!(matches!(
            subagent_ui_rx.recv().await,
            Some(UiUpdate::ChildAgentStarted { .. })
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
    async fn cancel_all_fires_child_token_and_empties_queue() {
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

        let cancelled = manager.cancel_all();
        assert_eq!(
            cancelled.iter().map(|c| c.call_id).collect::<Vec<_>>(),
            vec![first_call_id, second_call_id],
            "the running child must be reported first, then queued entries in FIFO order"
        );
        assert_eq!(manager.queue_len(), 0);
        assert!(!manager.is_running());

        let recorded = calls.lock().unwrap();
        assert!(
            recorded[0].1.is_cancelled(),
            "the running child's token must be cancelled"
        );
    }

    #[tokio::test]
    async fn cancel_all_returns_none_when_nothing_is_running() {
        let (mut manager, _calls) = test_manager();
        assert!(manager.cancel_all().is_empty());
    }

    #[test]
    fn finish_result_to_json_carries_all_fields() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
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
            status: "success".to_string(),
            result: Some("# All done\n\nEverything worked.".to_string()),
            error: None,
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, file_extension) = content_section_text(&sections[0]);
        assert_eq!(label, "Result");
        assert_eq!(content, "# All done\n\nEverything worked.");
        assert_eq!(file_extension, "md");
    }

    #[test]
    fn content_sections_renders_unstructured_result_as_markdown() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            status: "unstructured".to_string(),
            result: Some("last assistant text".to_string()),
            error: None,
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, _) = content_section_text(&sections[0]);
        assert_eq!(label, "Result");
        assert_eq!(content, "last assistant text");
    }

    #[test]
    fn content_sections_renders_failure_error_instead_of_result() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            status: "failure".to_string(),
            result: Some("should be ignored".to_string()),
            error: Some("could not complete the task".to_string()),
            sources: vec![],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let (label, content, _) = content_section_text(&sections[0]);
        assert_eq!(label, "Error");
        assert_eq!(content, "could not complete the task");
    }

    #[test]
    fn content_sections_appends_sources_as_bullet_list() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            status: "success".to_string(),
            result: Some("done".to_string()),
            error: None,
            sources: vec!["a.rs".to_string(), "b.rs".to_string()],
        };
        let sections = result.content_sections();
        assert_eq!(sections.len(), 2);
        let (result_label, ..) = content_section_text(&sections[0]);
        assert_eq!(result_label, "Result");
        let (sources_label, sources_content, _) = content_section_text(&sections[1]);
        assert_eq!(sources_label, "Sources");
        assert_eq!(sources_content, "- a.rs\n- b.rs");
    }

    #[test]
    fn content_sections_empty_when_no_result_error_or_sources() {
        let result = FinishResult {
            run_id: Uuid::new_v4(),
            call_id: Uuid::new_v4(),
            status: "success".to_string(),
            result: None,
            error: None,
            sources: vec![],
        };
        assert!(result.content_sections().is_empty());
    }
}
