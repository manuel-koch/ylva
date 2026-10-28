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

use super::conversation::project_messages_with_subagent_runs;
use super::*;
use crate::agent::AgentMode;
use std::time::Instant;
use uuid::Uuid;

mod conversation;
mod input_history;
mod layout;
mod model_picker;
mod selection;
use crate::agent::ToolCallRequest;

#[test]
fn collapse_and_clear_cancel_pending_expansion_feedback() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ThinkingMessage {
        text: "Thought".to_string(),
        collapsed: true,
        duration: std::time::Duration::ZERO,
    });
    state.toggle_thinking_collapsed(0);
    assert_eq!(
        state.pending_bubble_expansion,
        Some(BubbleExpansionTarget::Thinking(0))
    );
    state.toggle_thinking_collapsed(0);
    assert_eq!(state.pending_bubble_expansion, None);
    state.toggle_thinking_collapsed(0);
    state.clear_conversation();
    assert_eq!(state.pending_bubble_expansion, None);
    state.toggle_thinking_collapsed(0);
    state.toggle_tool_bubble_collapsed(Uuid::new_v4());
    state.toggle_header_bubble_collapsed();
    assert_eq!(state.pending_bubble_expansion, None);
}

#[test]
fn navigation_clears_expansion_feedback_from_the_previous_view() {
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "Review".to_string(),
        label: "Review".to_string(),
    });
    state.pending_bubble_expansion = Some(BubbleExpansionTarget::Thinking(0));
    state.switch_to_subagent_view(run_id);
    assert_eq!(state.pending_bubble_expansion, None);
    state.pending_bubble_expansion = Some(BubbleExpansionTarget::Thinking(0));
    state.switch_to_root_view();
    assert_eq!(state.pending_bubble_expansion, None);
}

#[test]
fn context_usage_formats_as_tokens_and_fill_percentage() {
    assert_eq!(format_context_usage(64_200, 128_000), "64.2K (50%)");
    assert_eq!(
        format_context_usage_readout(ContextUsage {
            estimated_tokens: 9_854,
            context_length: 98_304,
            source: ContextUsageSource::ModelView,
        }),
        "9.9K (10%)"
    );
    assert_eq!(
        format_context_usage_readout(ContextUsage {
            estimated_tokens: 64_200,
            context_length: 128_000,
            source: ContextUsageSource::SerializedRequest,
        }),
        "64.2K (50%)"
    );
    assert_eq!(format_context_usage(950, 128_000), "950 (1%)");
    assert_eq!(format_context_usage(1_260_000, 2_000_000), "1.3M (63%)");
    assert_eq!(format_context_usage(0, 128_000), "0 (0%)");
    assert_eq!(
        format_context_usage(128_000, 128_000),
        "128.0K (100%)",
        "a full window reads 100%"
    );
    assert_eq!(
        format_context_usage(200_000, 128_000),
        "200.0K (100%)",
        "an estimate overshooting the window clamps the percentage"
    );
    assert_eq!(
        format_context_usage(1_000, 0),
        "1.0K (0%)",
        "a degenerate window reports 0% instead of dividing by zero"
    );
}

#[test]
fn restore_focus_after_help_returns_to_an_outstanding_approval_first() {
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    state.restore_focus_after_help();
    assert_eq!(
        state.focus,
        Focus::MainInput,
        "without an outstanding approval, closing help returns to the normal input"
    );

    let tool_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: tool_id,
        name: "write".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: true,
    });
    state.pending_approval_order.push(tool_id);
    state.focus = Focus::HelpPopup;
    state.restore_focus_after_help();
    assert_eq!(
        state.focus,
        Focus::ToolApproval {
            tool_id,
            tool_name: "write".to_string()
        },
        "closing help with a verdict outstanding must resume the approval focus"
    );
}

#[test]
fn agent_mode_reports_tool_pending_while_help_covers_an_approval() {
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: tool_id,
        name: "write".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: true,
    });
    state.pending_approval_order.push(tool_id);
    state.mode = TuiMode::Streaming;
    state.focus = Focus::HelpPopup;
    assert_eq!(state.agent_mode(), AgentMode::ToolPending);
}

// ── Focus::is_modal ───────────────────────────────────────────────────────

#[test]
fn focus_is_modal_returns_true_only_for_help_popup() {
    assert!(Focus::HelpPopup.is_modal());
    assert!(!Focus::MainInput.is_modal());
    assert!(!Focus::ToolApproval {
        tool_id: Uuid::new_v4(),
        tool_name: "test_tool".to_string(),
    }
    .is_modal());
}

// ── ChatArea::contains geometry ───────────────────────────────────────────

// ── InputAreaLayout::contains geometry ───────────────────────────────────

// ── HelpPopupLayout::hit ──────────────────────────────────────────────────

// ── TuiMode construction and equality ─────────────────────────────────────

#[test]
fn tui_mode_idle_equals_idle() {
    assert_eq!(TuiMode::Idle, TuiMode::Idle);
}

#[test]
fn tui_mode_idle_does_not_equal_pending() {
    assert_ne!(TuiMode::Idle, TuiMode::Pending);
}

#[test]
fn tui_mode_double_press_abort_holds_pressed_at_and_return_to() {
    let now = Instant::now();
    let mode = TuiMode::DoublePressAbort {
        pressed_at: now,
        return_to: Box::new(TuiMode::Idle),
        armed_by: ConfirmKey::CtrlC,
    };
    if let TuiMode::DoublePressAbort { return_to, .. } = &mode {
        assert_eq!(**return_to, TuiMode::Idle);
    } else {
        panic!("expected DoublePressAbort");
    }
}

#[test]
fn agent_mode_maps_clarify_pending_to_clarify_pending() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Test?".to_string(),
        context: None,
        choices: None,
    };
    assert_eq!(state.agent_mode(), AgentMode::ClarifyPending);
}

#[test]
fn agent_mode_reports_tool_running_when_a_tool_bubble_is_running() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: "read".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: true,
    });
    assert_eq!(state.agent_mode(), AgentMode::ToolRunning);
}

#[test]
fn agent_mode_reports_tool_running_when_idle_but_a_spawned_subagent_is_still_running() {
    // Regression: the root loop's own turn can settle to `TuiMode::Idle`
    // (e.g. after `spawn_subagent` acked with "running") while the
    // spawned subagent is still working — its `spawn_subagent` tool bubble
    // stays `Running` until the subagent actually finishes. The footer must
    // not report `Idle` in that case; that made a genuinely-running
    // subagent look finished/stuck (see docs/known-test-issues.md).
    let mut state = AppState::new();
    state.mode = TuiMode::Idle;
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: "spawn_subagent".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: true,
    });
    assert_eq!(state.agent_mode(), AgentMode::ToolRunning);
}

#[test]
fn agent_mode_reports_idle_when_no_tool_bubble_is_running() {
    let state = AppState::new();
    assert_eq!(state.agent_mode(), AgentMode::Idle);
}

#[test]
fn agent_mode_reports_streaming_when_no_tool_bubble_is_running() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: "read".to_string(),
        state: Box::new(ToolCallState::Done {
            arguments: serde_json::json!({}),
            result_section: ResultSection {
                id: "output".to_string(),
                content: String::new(),
            },
            content_sections: vec![],
            summary: vec![],
            duration: std::time::Duration::ZERO,
            title_arguments: vec![],
        }),
        collapsed: true,
    });
    assert_eq!(state.agent_mode(), AgentMode::Streaming);
}

// ── Thinking bubble flush / toggle ────────────────────────────────────────

// ── Header bubble scroll offset ───────────────────────────────────────────

#[test]
fn typing_settles_after_the_idle_delay_but_never_later_than_the_cap() {
    let mut state = AppState::new();
    assert_eq!(state.typing_settles_at(), None);

    let start = Instant::now();
    state.note_keystroke(start);
    assert!(state.is_typing());
    assert_eq!(state.typing_settles_at(), Some(start + PROMPT_IDLE_DELAY));

    // Keystrokes arriving faster than the idle delay extend the burst, up to
    // the cap measured from the burst start.
    let mut now = start;
    while now < start + PROMPT_DEFERRAL_CAP {
        now += PROMPT_IDLE_DELAY / 2;
        state.note_keystroke(now);
    }
    assert_eq!(state.typing_settles_at(), Some(start + PROMPT_DEFERRAL_CAP));

    state.settle_typing();
    assert!(!state.is_typing());
    assert_eq!(state.typing_settles_at(), None);
}

#[test]
fn a_keystroke_after_an_idle_gap_starts_a_new_burst() {
    let mut state = AppState::new();
    let start = Instant::now();
    state.note_keystroke(start);
    let later = start + PROMPT_DEFERRAL_CAP * 2;
    state.note_keystroke(later);
    assert_eq!(state.typing_settles_at(), Some(later + PROMPT_IDLE_DELAY));
}
