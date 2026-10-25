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

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use ratatui_textarea::TextArea;
use std::time::Duration;
use uuid::Uuid;
use ylva::agent::tools::{
    ContentSection, DiffSection, SearchResultLine, SearchResultSection, Section,
};
use ylva::agent::{AssistantStatus, Message, ToolCallRequest, ToolResult};
use ylva::presenter::{update, TextareaContext, TuiEvent};
use ylva::tui_reducer_state::{AppState, DomainEvent, ResultSection, TuiMode};

fn empty_textarea() -> TextareaContext {
    TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    }
}

struct ToolBubbleRoundtripCase {
    tool_id: Uuid,
    tool_name: String,
    arguments: serde_json::Value,
    initial_content_sections: Vec<Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    result: String,
    result_metadata: Option<serde_json::Value>,
    result_metadata_schema: Option<serde_json::Value>,
    result_section: ResultSection,
    summary: Vec<(String, String)>,
    content_sections: Vec<Section>,
    duration: Duration,
}

struct ToolErrorBubbleRoundtripCase {
    tool_id: Uuid,
    tool_name: String,
    arguments: serde_json::Value,
    initial_content_sections: Vec<Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    error: String,
    duration: Duration,
    content_sections: Vec<Section>,
}

fn render_app_state_buffer(state: &mut AppState) -> Buffer {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("render test frame");
    terminal.backend().buffer().clone()
}

fn assert_tool_bubble_roundtrip(case: ToolBubbleRoundtripCase) {
    assert_tool_bubble_roundtrip_with_subagent_runs(case, &[]);
}

fn assert_tool_bubble_roundtrip_with_subagent_runs(
    case: ToolBubbleRoundtripCase,
    subagent_runs: &[ylva::store::SubagentRunRecord],
) {
    let persisted_content_sections = if subagent_runs.is_empty() {
        case.content_sections.clone()
    } else {
        case.initial_content_sections.clone()
    };
    let mut live_state = AppState::new();
    live_state.mode = TuiMode::Streaming;
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: case.tool_id,
            name: case.tool_name.clone(),
            arguments: case.arguments.clone(),
            content_sections: case.initial_content_sections.clone(),
            title_arguments: case.title_arguments.clone(),
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: case.tool_id,
            name: case.tool_name.clone(),
            result_section: case.result_section.clone(),
            summary: case.summary.clone(),
            duration: case.duration,
            content_sections: case.content_sections.clone(),
            run_id: None,
        }),
        &empty_textarea(),
    );

    let persisted_messages = [
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: case.tool_id,
                name: case.tool_name.clone(),
                arguments: case.arguments.clone(),
                persisted_title_arguments: case.title_arguments.clone(),
            }],
        },
        Message::ToolResult {
            value: ToolResult {
                id: case.tool_id,
                name: case.tool_name.clone(),
                result: case.result.clone(),
                attachments: vec![],
                result_section_id: Some(case.result_section.id.clone()),
                result_metadata: case.result_metadata.clone(),
                result_metadata_schema: case.result_metadata_schema.clone(),
                summary: case.summary.clone(),
                content_sections: persisted_content_sections,
                duration: case.duration,
            },
        },
    ];
    let replayed_messages = persisted_messages
        .iter()
        .map(|message| {
            let serialized = serde_json::to_string(message).expect("message serializes");
            serde_json::from_str::<Message>(&serialized).expect("message deserializes")
        })
        .collect::<Vec<_>>();
    let mut replayed_state = AppState::new();
    replayed_state.replay_messages(&replayed_messages, subagent_runs, None);
    let replayed_entries = replayed_state.chat_entries.clone();

    // Keep the surrounding frame identical so this compares the actual
    // tool-bubble rendering rather than unrelated live/resumed mode chrome.
    live_state.mode = TuiMode::Idle;
    replayed_state.mode = TuiMode::Idle;
    let live_buffer = render_app_state_buffer(&mut live_state);
    let replayed_buffer = render_app_state_buffer(&mut replayed_state);

    assert_eq!(
        live_state.chat_entries, replayed_entries,
        "{} tool bubble must have the same reducer state before and after resume",
        case.tool_name
    );

    assert_eq!(
        live_buffer, replayed_buffer,
        "{} tool bubble must render identically before and after resume",
        case.tool_name
    );
}

fn assert_tool_error_bubble_roundtrip(case: ToolErrorBubbleRoundtripCase) {
    let mut live_state = AppState::new();
    live_state.mode = TuiMode::Streaming;
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: case.tool_id,
            name: case.tool_name.clone(),
            arguments: case.arguments.clone(),
            content_sections: case.initial_content_sections,
            title_arguments: case.title_arguments.clone(),
            run_id: None,
        }),
        &empty_textarea(),
    );
    live_state.apply_tool_error(
        case.tool_id,
        case.error.clone(),
        case.duration,
        case.content_sections.clone(),
        None,
    );

    let persisted_messages = [
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: case.tool_id,
                name: case.tool_name.clone(),
                arguments: case.arguments,
                persisted_title_arguments: case.title_arguments,
            }],
        },
        Message::ToolError {
            id: case.tool_id,
            name: case.tool_name,
            error: case.error,
            duration: case.duration,
            content_sections: case.content_sections,
        },
    ];
    let replayed_messages = persisted_messages
        .iter()
        .map(|message| {
            let serialized = serde_json::to_string(message).expect("message serializes");
            serde_json::from_str::<Message>(&serialized).expect("message deserializes")
        })
        .collect::<Vec<_>>();
    let mut replayed_state = AppState::new();
    replayed_state.replay_messages(&replayed_messages, &[], None);
    let replayed_entries = replayed_state.chat_entries.clone();

    live_state.mode = TuiMode::Idle;
    replayed_state.mode = TuiMode::Idle;
    let live_buffer = render_app_state_buffer(&mut live_state);
    let replayed_buffer = render_app_state_buffer(&mut replayed_state);

    assert_eq!(
        live_state.chat_entries, replayed_entries,
        "error bubble must have the same reducer state before and after resume"
    );
    assert_eq!(
        live_buffer, replayed_buffer,
        "error bubble must render identically before and after resume"
    );
}

#[test]
fn live_read_tool_bubble_matches_serialized_and_replayed_bubble() {
    let result = "# Ylva\nThe Agent Harness.".to_string();
    let content_sections = vec![Section::Content(ylva::agent::tools::ContentSection {
        label: None,
        content: result.clone(),
        file_extension: Some("md".to_string()),
        line_number_offset: Some(0),
    })];
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "read".to_string(),
        arguments: serde_json::json!({"path": "README.md"}),
        initial_content_sections: vec![],
        title_arguments: vec![(
            "path".to_string(),
            serde_json::Value::String("README.md".to_string()),
        )],
        result: result.clone(),
        result_metadata: None,
        result_metadata_schema: None,
        result_section: ResultSection::plain(result),
        summary: vec![
            ("read_bytes".to_string(), "28".to_string()),
            ("read_lines".to_string(), "1".to_string()),
        ],
        content_sections,
        duration: Duration::from_millis(37),
    });
}

#[test]
fn live_write_tool_bubble_matches_serialized_and_replayed_bubble() {
    let path = "src/example.rs";
    let content = "fn main() {\n    println!(\"hello\");\n}\n".to_string();
    let content_sections = vec![Section::Content(ylva::agent::tools::ContentSection {
        label: None,
        content: content.clone(),
        file_extension: Some("rs".to_string()),
        line_number_offset: Some(0),
    })];
    let bytes_written = content.len();
    let result = format!("wrote {bytes_written} bytes to '{path}'");

    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "write".to_string(),
        arguments: serde_json::json!({
            "path": path,
            "content": content,
        }),
        initial_content_sections: content_sections.clone(),
        title_arguments: vec![(
            "path".to_string(),
            serde_json::Value::String(path.to_string()),
        )],
        result: result.clone(),
        result_metadata: Some(serde_json::json!({"bytes_written": bytes_written})),
        result_metadata_schema: None,
        result_section: ResultSection::plain(result),
        summary: vec![("bytes_written".to_string(), bytes_written.to_string())],
        content_sections,
        duration: Duration::from_millis(42),
    });
}

#[test]
fn write_error_bubble_matches_real_summary_fixture_after_resume() {
    let content = r#"### Conversation Summary

**Sky Blue:** Attributed to **Rayleigh scattering**, where atmospheric gas molecules scatter short-wavelength blue and violet light more effectively than longer wavelengths.
"#;
    assert_tool_error_bubble_roundtrip(ToolErrorBubbleRoundtripCase {
            tool_id: Uuid::parse_str("c15523ca-6b84-4ffb-969c-9531d47a768a")
                .expect("real write call id"),
            tool_name: "write".to_string(),
            arguments: serde_json::json!({
                "content": content,
                "path": "../logs/summary.md",
            }),
            initial_content_sections: vec![],
            title_arguments: vec![(
                "path".to_string(),
                serde_json::Value::String("../logs/summary.md".to_string()),
            )],
            error: "Access denied by security policy: path is outside the workspace root; only paths within the workspace are accessible"
                .to_string(),
            duration: Duration::ZERO,
            content_sections: vec![],
        });
}

#[test]
fn live_bash_tool_bubble_matches_serialized_and_replayed_bubble() {
    let result = "hello from bash\n".to_string();
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "bash".to_string(),
        arguments: serde_json::json!({"command": "printf 'hello from bash\\n'"}),
        initial_content_sections: vec![],
        title_arguments: vec![(
            "command".to_string(),
            serde_json::Value::String("printf 'hello from bash\\n'".to_string()),
        )],
        result: result.clone(),
        result_metadata: Some(serde_json::json!({
            "output_bytes": result.len(),
        })),
        result_metadata_schema: None,
        result_section: ResultSection::plain(result.clone()),
        summary: vec![("output_bytes".to_string(), result.len().to_string())],
        content_sections: vec![Section::Content(ContentSection {
            label: Some("Output".to_string()),
            content: result,
            file_extension: None,
            line_number_offset: Some(0),
        })],
        duration: Duration::from_millis(51),
    });
}

#[test]
fn live_glob_tool_bubble_matches_serialized_and_replayed_bubble() {
    let result = "src/lib.rs\nsrc/main.rs".to_string();
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "glob".to_string(),
        arguments: serde_json::json!({"pattern": "src/**/*.rs"}),
        initial_content_sections: vec![],
        title_arguments: vec![(
            "pattern".to_string(),
            serde_json::Value::String("src/**/*.rs".to_string()),
        )],
        result: result.clone(),
        result_metadata: Some(serde_json::json!({
            "matches": 2,
        })),
        result_metadata_schema: None,
        result_section: ResultSection::plain(result.clone()),
        summary: vec![("matches".to_string(), "2".to_string())],
        content_sections: vec![Section::Content(ContentSection {
            label: None,
            content: result,
            file_extension: None,
            line_number_offset: Some(0),
        })],
        duration: Duration::from_millis(23),
    });
}

#[test]
fn live_search_tool_bubble_matches_serialized_and_replayed_bubble() {
    let result = "src/lib.rs:10: fn main()\nsrc/lib.rs-9- context".to_string();
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "search".to_string(),
        arguments: serde_json::json!({
            "pattern": "main",
            "path": "src",
            "glob": "**/*.rs",
            "context_lines": 1,
        }),
        initial_content_sections: vec![],
        title_arguments: vec![
            (
                "pattern".to_string(),
                serde_json::Value::String("main".to_string()),
            ),
            (
                "path".to_string(),
                serde_json::Value::String("src".to_string()),
            ),
            (
                "glob".to_string(),
                serde_json::Value::String("**/*.rs".to_string()),
            ),
            ("context_lines".to_string(), serde_json::json!(1)),
        ],
        result: result.clone(),
        result_metadata: Some(serde_json::json!({
            "matching_files": 1,
            "matching_lines": 1,
        })),
        result_metadata_schema: None,
        result_section: ResultSection::plain(result),
        summary: vec![
            ("matching_files".to_string(), "1".to_string()),
            ("matching_lines".to_string(), "1".to_string()),
        ],
        content_sections: vec![Section::SearchResult(SearchResultSection {
            file_path: "src/lib.rs".to_string(),
            file_extension: Some("rs".to_string()),
            lines: vec![
                SearchResultLine {
                    line_number: 9,
                    is_match: false,
                    content: "context".to_string(),
                },
                SearchResultLine {
                    line_number: 10,
                    is_match: true,
                    content: "fn main()".to_string(),
                },
            ],
        })],
        duration: Duration::from_millis(64),
    });
}

#[test]
fn live_edit_tool_bubble_matches_serialized_and_replayed_bubble() {
    let path = "src/example.rs";
    let old_string = "old()";
    let new_string = "new()";
    let diff = "@@ -1 +1 @@\n-old()\n+new()\n".to_string();
    let result = format!("Edited '{path}': 1 replacement(s) made");
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "edit".to_string(),
        arguments: serde_json::json!({
            "path": path,
            "mode": "search_replace",
            "old_string": old_string,
            "new_string": new_string,
            "replace_all": false,
        }),
        initial_content_sections: vec![Section::Diff(DiffSection {
            label: Some(path.to_string()),
            content: diff.clone(),
        })],
        title_arguments: vec![
            (
                "path".to_string(),
                serde_json::Value::String(path.to_string()),
            ),
            (
                "mode".to_string(),
                serde_json::Value::String("search_replace".to_string()),
            ),
            ("replace_all".to_string(), serde_json::Value::Bool(false)),
        ],
        result: result.clone(),
        result_metadata: Some(serde_json::json!({
            "replacements_made": 1,
        })),
        result_metadata_schema: None,
        result_section: ResultSection::plain(result),
        summary: vec![("replacements_made".to_string(), "1".to_string())],
        content_sections: vec![Section::Diff(DiffSection {
            label: Some(path.to_string()),
            content: diff,
        })],
        duration: Duration::from_millis(48),
    });
}

#[test]
fn live_multiline_clarify_fixture_matches_after_resume() {
    let tool_id =
        Uuid::parse_str("5c232376-718f-4e84-bbd6-6042d6dfb13c").expect("fixture clarify id");
    let question =
            "Would you like to replace the 'lamp' in the poem with a different animal? Which animal should I use?";
    let context = "The current poem is:\n\nA small lamp glows so bright,\nA tiny beacon in the night.\nIt chases shadows with a spark,\nAnd banishes the dark.";
    let choices = vec![
        "Tiger".to_string(),
        "Elephant".to_string(),
        "Fox".to_string(),
    ];
    let content_sections = vec![
        Section::Content(ContentSection {
            label: Some("Context".to_string()),
            content: context.to_string(),
            file_extension: None,
            line_number_offset: None,
        }),
        Section::Content(ContentSection {
            label: Some("Choices".to_string()),
            content: "1. Tiger\n2. Elephant\n3. Fox".to_string(),
            file_extension: None,
            line_number_offset: None,
        }),
    ];
    let duration = Duration::new(8, 90_442_083);
    // A fresh session persists only the question in title_arguments;
    // context and choices are the completed bubble's body sections.
    let title_arguments = vec![(
        "question".to_string(),
        serde_json::Value::String(question.to_string()),
    )];

    let mut live_state = AppState::new();
    live_state.mode = TuiMode::Streaming;
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::ClarifyPending {
            id: tool_id,
            question: question.to_string(),
            context: Some(context.to_string()),
            choices: Some(choices.clone()),
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: tool_id,
            name: "clarify".to_string(),
            result_section: ResultSection {
                id: ylva::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                content: "Elephant".to_string(),
            },
            summary: vec![],
            duration,
            content_sections: content_sections.clone(),
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut live_state,
        TuiEvent::Domain(DomainEvent::StreamStarted),
        &empty_textarea(),
    );

    let persisted_messages = [
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id: tool_id,
                name: "clarify".to_string(),
                arguments: serde_json::json!({
                    "choices": choices,
                    "context": context,
                    "question": question,
                }),
                persisted_title_arguments: title_arguments,
            }],
        },
        Message::ToolResult {
            value: ToolResult {
                id: tool_id,
                name: "clarify".to_string(),
                result: "Elephant".to_string(),
                attachments: vec![],
                result_section_id: Some(
                    ylva::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                ),
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections,
                duration,
            },
        },
    ];
    let replayed_messages = persisted_messages
        .iter()
        .map(|message| {
            let serialized = serde_json::to_string(message).expect("message serializes");
            serde_json::from_str::<Message>(&serialized).expect("message deserializes")
        })
        .collect::<Vec<_>>();
    let mut replayed_state = AppState::new();
    replayed_state.replay_messages(&replayed_messages, &[], None);
    let replayed_entries = replayed_state.chat_entries.clone();

    live_state.mode = TuiMode::Idle;
    replayed_state.mode = TuiMode::Idle;
    let live_buffer = render_app_state_buffer(&mut live_state);
    let replayed_buffer = render_app_state_buffer(&mut replayed_state);

    assert_eq!(live_state.chat_entries, replayed_entries);
    assert_eq!(live_buffer, replayed_buffer);
}

#[test]
fn live_agent_finish_tool_bubble_matches_serialized_and_replayed_bubble() {
    let result = "The subagent completed the requested inspection.".to_string();
    assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
        tool_id: Uuid::new_v4(),
        tool_name: "agent_finish".to_string(),
        arguments: serde_json::json!({
            "status": "success",
            "result": result,
            "sources": ["src/lib.rs"],
        }),
        initial_content_sections: vec![],
        title_arguments: vec![
            (
                "status".to_string(),
                serde_json::Value::String("success".to_string()),
            ),
            (
                "result".to_string(),
                serde_json::Value::String(result.clone()),
            ),
            ("sources".to_string(), serde_json::json!(["src/lib.rs"])),
        ],
        result: "Reported success to the parent agent.".to_string(),
        result_metadata: None,
        result_metadata_schema: None,
        result_section: ResultSection::plain("Reported success to the parent agent."),
        summary: vec![],
        content_sections: vec![],
        duration: Duration::from_millis(19),
    });
}

#[test]
fn live_spawn_subagent_tool_bubble_matches_serialized_and_replayed_bubble() {
    let task = "Inspect the parser and report any unsafe assumptions.";
    let briefing = "Focus on input validation and error propagation.";
    let subagent_result = "The parser validates all external input.";
    let sources = vec!["src/parser.rs".to_string()];
    let run_id = Uuid::new_v4();
    let call_id = Uuid::new_v4();
    let task_section = Section::Content(ContentSection {
        label: Some("Task".to_string()),
        content: task.to_string(),
        file_extension: Some("md".to_string()),
        line_number_offset: None,
    });
    let briefing_section = Section::Content(ContentSection {
        label: Some("Briefing".to_string()),
        content: briefing.to_string(),
        file_extension: Some("md".to_string()),
        line_number_offset: None,
    });
    let result_section = Section::Content(ContentSection {
        label: Some("Output".to_string()),
        content: subagent_result.to_string(),
        file_extension: Some("md".to_string()),
        line_number_offset: None,
    });
    let sources_section = Section::Content(ContentSection {
        label: Some("Sources".to_string()),
        content: "- src/parser.rs".to_string(),
        file_extension: Some("md".to_string()),
        line_number_offset: None,
    });
    let final_content_sections = vec![
        task_section.clone(),
        briefing_section.clone(),
        result_section,
        sources_section,
    ];

    assert_tool_bubble_roundtrip_with_subagent_runs(
        ToolBubbleRoundtripCase {
            tool_id: call_id,
            tool_name: "spawn_subagent".to_string(),
            arguments: serde_json::json!({
                "label": "Inspect parser safety",
                "task": task,
                "briefing": briefing,
            }),
            initial_content_sections: vec![task_section, briefing_section],
            title_arguments: vec![(
                "label".to_string(),
                serde_json::Value::String("Inspect parser safety".to_string()),
            )],
            result: serde_json::to_string(&serde_json::json!({
                "status": "running",
                "run_id": run_id,
            }))
            .expect("spawn result serializes"),
            result_metadata: None,
            result_metadata_schema: None,
            result_section: ResultSection::plain(""),
            summary: vec![("status".to_string(), "success".to_string())],
            content_sections: final_content_sections,
            duration: Duration::from_millis(91),
        },
        &[ylva::store::SubagentRunRecord {
            run_id,
            call_id,
            parent_session: Uuid::new_v4(),
            agent_type: "default".to_string(),
            task: task.to_string(),
            label: "Inspect parser safety".to_string(),
            created_at: "2026-09-11T07:00:00.000Z".to_string(),
            completed_at: Some("2026-09-11T07:00:00.091Z".to_string()),
            status: Some("success".to_string()),
            result: Some(subagent_result.to_string()),
            sources,
            origin: ylva::subagent_manager::FinishResultOrigin::Agent,
            duration: Some(Duration::from_millis(91)),
        }],
    );
}
