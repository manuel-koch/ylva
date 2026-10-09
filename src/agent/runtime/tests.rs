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

fn run_state() -> (RunState, mpsc::Receiver<UiUpdate>) {
    let (ui_tx, ui_rx) = mpsc::channel(100);
    let (agent_tx, _agent_rx) = mpsc::channel(100);
    let (compaction, _compaction_rx) = compaction::CompactionRuntime::new(
        None,
        compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
        Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
    );
    (
        RunState {
            engine: crate::agent::tests::test_engine(),
            registry: Arc::new(ToolRegistry::new()),
            ui_tx,
            agent_tx,
            parent_cancel: None,
            system_prompt: None,
            own_run_id: None,
            child_agent_manager: None,
            max_tool_iterations: DEFAULT_MAX_TOOL_USE_ITERATIONS,
            batch_cap: MAX_EXECUTED_TOOL_CALLS_PER_BATCH,
            agent_type: None,
            session_id: None,
            current_store: Arc::new(Mutex::new(None)),
            store_for_commands: None,
            spawn_content_sections: HashMap::new(),
            is_child: true,
            context: Context::new(),
            compaction,
            startup_checks_finished: false,
            request_waiting_for_startup_checks: false,
            model_change_capacity_check: None,
            mode: AgentMode::Idle,
            current_cancel: None,
            current_tool_cancel: None,
            pending_steers: VecDeque::new(),
            pending_tools: VecDeque::new(),
            iterations: 0,
            clarify_started_at: None,
            generation: 1,
            cancelled_run_ids: std::collections::HashSet::new(),
            pending_child_finish_reprompt: false,
            iteration_cap_exceeded: None,
            stream_error: None,
            execution_span: tracing::Span::none(),
        },
        ui_rx,
    )
}

#[test]
fn ignored_inputs_poll_again_instead_of_completing_an_idle_child() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        let transcript = state.context.messages().to_vec();
        assert_eq!(
            state
                .handle_command(Some(AgentCommand::Prompt(String::new())))
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(
            state
                .handle_agent_event(
                    0,
                    AgentEvent::StreamDone {
                        partial: "stale reply".to_string(),
                    },
                )
                .await,
            LoopControl::PollAgain
        );
        let run_id = Uuid::new_v4();
        state.cancelled_run_ids.insert(run_id);
        assert_eq!(
            state
                .handle_subagent_finish(crate::child_agent_manager::FinishResult {
                    run_id,
                    call_id: Uuid::new_v4(),
                    origin: crate::child_agent_manager::FinishResultOrigin::Runtime,
                    status: "success".to_string(),
                    result: Some("stale child result".to_string()),
                    error: None,
                    sources: vec![],
                })
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(state.mode, AgentMode::Idle);
        assert_eq!(state.context.messages(), transcript);
        assert!(updates.try_recv().is_err());
        assert!(!state.cancelled_run_ids.contains(&run_id));
    });
}

#[test]
fn terminal_and_normal_handlers_preserve_distinct_completion_outcomes() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        assert_eq!(
            state
                .handle_command(Some(AgentCommand::DenyTool {
                    id: Uuid::new_v4(),
                    reason: String::new(),
                }))
                .await,
            LoopControl::CheckCompletion
        );
        assert_eq!(
            state
                .handle_agent_event(
                    1,
                    AgentEvent::StreamDone {
                        partial: "committed answer".to_string(),
                    },
                )
                .await,
            LoopControl::CheckCompletion
        );
        assert!(matches!(
            state.context.messages().last(),
            Some(Message::Assistant {
                content,
                status: AssistantStatus::Complete,
                ..
            }) if content == "committed answer"
        ));
        assert_eq!(
            updates.try_recv().expect("completion update"),
            UiUpdate::StreamDone
        );
        assert_eq!(state.handle_command(None).await, LoopControl::Finished);
    });
}
