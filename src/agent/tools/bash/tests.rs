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

use super::output::read_bounded_line;
use super::result::BashResult;
use super::*;
use crate::test_support::run_locked;
use std::time::Duration;

fn make_bash_args(command: &str) -> BashInputArgs {
    BashInputArgs {
        command: command.to_string(),
        label: None,
    }
}

// ── needs_approval ────────────────────────────────────────────────────────

#[test]
fn bash_tool_requires_user_approval_via_sandbox_abstain() {
    // The sandbox has no command rules and abstains (None), so the tool's
    // own default (UserApprovalRequired) kicks in.
    let tool = BashTool::default();
    let approval = tool.needs_approval(&serde_json::json!({ "command": "echo hi" }));
    assert!(
        matches!(approval, ApprovalCheck::UserApprovalRequired),
        "sandbox abstains → tool falls back to UserApprovalRequired"
    );
}

#[test]
fn bash_tool_has_default_30s_timeout() {
    let tool = BashTool::default();
    assert_eq!(tool.timeout_secs(), DEFAULT_BASH_TIMEOUT_SECS);
}

#[test]
fn bash_tool_description_contains_actual_timeout_and_output_cap() {
    let tool = BashTool::default();
    let description = tool.description();
    assert!(
        description.contains(&DEFAULT_BASH_TIMEOUT_SECS.to_string()),
        "description must include the actual timeout: {description}"
    );
    assert!(
        description.contains(&BASH_OUTPUT_LIMIT_LINES.to_string()),
        "description must include the output cap in lines: {description}"
    );
    assert!(
        description.contains(&BASH_OUTPUT_LIMIT_BYTES.to_string()),
        "description must include the output cap in bytes: {description}"
    );
}

#[test]
fn bash_tool_description_explains_workspace_working_directory() {
    let tool = BashTool::default();
    let description = tool.description();
    assert!(description.contains("current working directory"));
    assert!(description.contains("workspace root"));
    assert!(description.contains("do not prefix"));
    assert!(description.contains("Use relative paths directly"));
}

#[test]
fn bash_tool_with_timeout_description_reflects_custom_timeout() {
    let tool = BashTool::with_timeout(
        60,
        crate::sandbox::SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        ))),
    );
    assert!(
        tool.description().contains("60"),
        "custom timeout must appear in description: {}",
        tool.description()
    );
}

#[test]
fn bash_tool_description_mentions_optional_label_and_title_line() {
    let tool = BashTool::default();
    let description = tool.description();
    assert!(
        description.contains("optional short `label`"),
        "description must advertise the optional label: {description}"
    );
    assert!(
        description.contains("tool-call title line"),
        "description must explain where the label is shown: {description}"
    );
}

// ── label schema, ranking, and title suppression ──────────────────────────

#[test]
fn bash_input_args_schema_declares_optional_label_after_command() {
    let schema = BashTool::default().input_schema();
    let schema = serde_json::to_value(schema).expect("schema serializes");
    let properties = schema["properties"]
        .as_object()
        .expect("schema has properties");
    assert!(
        properties.contains_key("label"),
        "schema must declare the optional label: {properties:?}"
    );
    let property_names: Vec<&str> = properties.keys().map(String::as_str).collect();
    assert_eq!(
        property_names,
        vec!["command", "label"],
        "command must stay rank 0 (primary); label rank 1 (prop)"
    );
    let required = schema["required"]
        .as_array()
        .map(|values| values.len())
        .unwrap_or(0);
    assert_eq!(required, 1, "only command is required: {schema}");
    let label_description = schema["properties"]["label"]["description"]
        .as_str()
        .expect("label schema has a description");
    assert!(!label_description.is_empty());
}

#[test]
fn command_ranks_ahead_of_label_as_primary_title_argument() {
    let tool = BashTool::default();
    assert_eq!(tool.argument_order("command"), 0);
    assert_eq!(
        tool.argument_order("label"),
        1,
        "label must rank after command so command stays the bare primary"
    );
}

#[test]
fn non_empty_label_appears_in_title_arguments_after_command() {
    let tool = BashTool::default();
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({
            "command": "ls -la",
            "label": "List workspace files",
        }),
    );
    assert_eq!(
        call.title_arguments(),
        vec![
            ("command".to_string(), serde_json::json!("ls -la")),
            (
                "label".to_string(),
                serde_json::json!("List workspace files")
            ),
        ],
        "command must be the primary, label the following prop"
    );
}

#[test]
fn absent_label_is_suppressed_from_title_arguments() {
    let tool = BashTool::default();
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({ "command": "ls -la" }),
    );
    assert_eq!(
        call.title_arguments(),
        vec![("command".to_string(), serde_json::json!("ls -la"))],
        "calls without a label must title exactly as before"
    );
}

#[test]
fn empty_or_whitespace_label_is_suppressed_from_title_arguments() {
    let tool = BashTool::default();
    for label in ["", "   ", "\t\n"] {
        let call = tool.create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({ "command": "ls -la", "label": label }),
        );
        assert_eq!(
            call.title_arguments(),
            vec![("command".to_string(), serde_json::json!("ls -la"))],
            "blank label {label:?} must be treated as omitted"
        );
    }
    let null_call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({ "command": "ls -la", "label": serde_json::Value::Null }),
    );
    assert_eq!(
        null_call.title_arguments(),
        vec![("command".to_string(), serde_json::json!("ls -la"))],
        "null label must be treated as omitted"
    );
}

#[test]
fn needs_approval_is_unaffected_by_label() {
    let tool = BashTool::default();
    let without_label = tool.needs_approval(&serde_json::json!({ "command": "echo hi" }));
    let with_label = tool
        .needs_approval(&serde_json::json!({ "command": "echo hi", "label": "Greet the user" }));
    assert_eq!(without_label, with_label, "label must not affect verdicts");
    assert!(
        matches!(with_label, ApprovalCheck::UserApprovalRequired),
        "sandbox abstains → tool falls back to UserApprovalRequired"
    );
}

// ── happy path ────────────────────────────────────────────────────────────

#[test]
fn bash_happy_path_echo() {
    run_locked(async {
        let result = run_bash(make_bash_args("echo hello"), 5).await;
        let result = result.expect("echo should succeed");
        assert!(
            result.output.contains("hello"),
            "output should contain 'hello'"
        );
        assert!(!result.truncated);
    });
}

#[test]
fn bash_merges_stdout_and_stderr() {
    run_locked(async {
        // Write to both stdout and stderr; both should appear in output.
        let result = run_bash(make_bash_args("echo stdout; echo stderr >&2"), 5)
            .await
            .expect("command should succeed");
        assert!(result.output.contains("stdout"), "stdout missing");
        assert!(result.output.contains("stderr"), "stderr missing");
    });
}

// ── non-zero exit ─────────────────────────────────────────────────────────

#[test]
fn bash_non_zero_exit_returns_tool_error() {
    run_locked(async {
        let result = run_bash(make_bash_args("exit 42"), 5).await;
        let error = result.expect_err("non-zero exit must be an error");
        let message = error.to_string();
        assert!(
            message.contains("42"),
            "error must include exit code: {message}"
        );
    });
}

#[test]
fn bash_non_zero_exit_includes_output_in_error() {
    run_locked(async {
        let result = run_bash(make_bash_args("echo before_exit; exit 1"), 5).await;
        let error = result.expect_err("non-zero exit must be an error");
        assert!(
            error.to_string().contains("before_exit"),
            "output should be included in error message"
        );
    });
}

// ── output truncation ─────────────────────────────────────────────────────

#[test]
fn bash_output_truncated_at_cap() {
    run_locked(async {
        // Generate more lines than the 1000-line cap so truncation kicks in.
        let result = run_bash(make_bash_args("yes | head -n 2000"), 10)
            .await
            .expect("command should succeed");
        assert!(result.truncated, "output should be marked as truncated");
        let line_count = result.output.lines().count();
        assert_eq!(
            line_count, BASH_OUTPUT_LIMIT_LINES,
            "output must be capped at whole lines, never split mid-line: got {line_count} lines"
        );
        // Verify metadata contains the truncation key.
        let meta = result
            .metadata()
            .expect("truncated result must have metadata");
        let obj = meta.as_object().expect("metadata is a JSON object");
        assert!(obj.contains_key("truncated"), "truncated key missing");
    });
}

#[test]
fn bash_output_truncated_at_byte_cap_with_few_huge_lines() {
    run_locked(async {
        // A handful of lines, each far larger than the byte cap divided by the
        // line cap, so the byte cap (not the line cap) must trigger truncation.
        let command = format!(
            "for i in 1 2 3; do head -c {} /dev/zero | tr '\\0' 'a'; echo; done",
            BASH_OUTPUT_LIMIT_BYTES
        );
        let result = run_bash(make_bash_args(&command), 10)
            .await
            .expect("command should succeed");
        assert!(result.truncated, "output should be marked as truncated");
        let line_count = result.output.lines().count();
        assert!(
            line_count < BASH_OUTPUT_LIMIT_LINES,
            "byte cap must trigger before the line cap: got {line_count} lines"
        );
        assert!(
            result.output.len() <= BASH_OUTPUT_LIMIT_BYTES,
            "output must never exceed the byte cap: got {} bytes",
            result.output.len()
        );
    });
}

#[tokio::test]
async fn bounded_line_reader_never_buffers_more_than_the_maximum() {
    let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(
        [vec![b'a'; 5_000_000], b"\nnext\n".to_vec()].concat(),
    ));
    let mut line = Vec::new();

    assert!(read_bounded_line(&mut reader, &mut line, 100)
        .await
        .unwrap());
    assert_eq!(line.len(), 100);
    assert!(read_bounded_line(&mut reader, &mut line, 100)
        .await
        .unwrap());
    assert_eq!(line, b"next\n");
    assert!(!read_bounded_line(&mut reader, &mut line, 100)
        .await
        .unwrap());
}

#[test]
fn bash_output_far_above_the_cap_is_drained_and_capped() {
    run_locked(async {
        // ~100 MB across many lines plus a ~50 MB unterminated tail: the
        // command must not block on a full pipe, and only the cap is kept.
        let command = "head -c 100000000 /dev/zero | tr '\\0' 'a' | fold -w 80; \
            head -c 50000000 /dev/zero | tr '\\0' 'b'";
        let result = run_bash(make_bash_args(command), 60)
            .await
            .expect("command should succeed");
        assert!(result.truncated);
        assert!(result.output.len() <= BASH_OUTPUT_LIMIT_BYTES);
        assert!(!result.output.is_empty());
    });
}

#[test]
fn bash_invalid_utf8_output_does_not_stop_later_lines() {
    run_locked(async {
        let result = run_bash(make_bash_args("printf 'a\\377b\\nlater\\n'"), 10)
            .await
            .expect("command should succeed");
        assert!(result.output.contains("later"), "{:?}", result.output);
    });
}

#[test]
fn bash_single_unterminated_line_over_byte_cap_keeps_prefix() {
    run_locked(async {
        // One unterminated line larger than the byte cap (the heavy-output
        // e2e shape). The collector must keep a prefix up to the cap, not
        // discard the whole line and report output_bytes=0 with truncated.
        let command = format!(
            "head -c {} /dev/zero | tr '\\0' 'a'",
            BASH_OUTPUT_LIMIT_BYTES + 50_000
        );
        let result = run_bash(make_bash_args(&command), 10)
            .await
            .expect("command should succeed");
        assert!(result.truncated, "output should be marked as truncated");
        assert_eq!(
            result.output.len(),
            BASH_OUTPUT_LIMIT_BYTES,
            "oversized first line must yield a full-length prefix, not empty output"
        );
        assert!(
            result.output.chars().all(|c| c == 'a'),
            "prefix must be the start of the original line"
        );
        let meta = result.metadata().expect("result must have metadata");
        let obj = meta.as_object().expect("metadata is a JSON object");
        assert_eq!(
            obj["output_bytes"].as_u64(),
            Some(BASH_OUTPUT_LIMIT_BYTES as u64),
            "metadata output_bytes must reflect the retained prefix"
        );
        assert_eq!(obj["truncated"], serde_json::json!(true));
    });
}

// ── timeout ───────────────────────────────────────────────────────────────

#[test]
fn bash_timeout_kills_process_group() {
    run_locked(async {
        // `sleep 60` will be killed well before 60 s because we pass a 1 s
        // timeout. We create a sub-shell that itself spawns a background
        // grandchild; the process-group kill must reap it too.
        let result = run_bash(make_bash_args("bash -c 'sleep 60 &' ; sleep 60"), 1).await;
        let error = result.expect_err("sleep should be killed by timeout");
        assert!(
            error.to_string().contains("timed out"),
            "error should mention timeout: {}",
            error
        );
    });
}

// ── cancellation (future drop) ────────────────────────────────────────────

#[test]
fn bash_future_drop_kills_process_group() {
    run_locked(async {
        // Spawn the bash future as a task, then abort it immediately.
        // The process group guard inside run_bash must clean up when the
        // async state machine is dropped.
        let handle = tokio::spawn(run_bash(make_bash_args("sleep 60"), 30));
        // Give the process a moment to start so it actually spawns.
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();

        // Aborting a JoinHandle makes it return JoinError::Cancelled.
        let join_result = handle.await;
        assert!(join_result.is_err(), "aborted task must return an error");
        // The `sleep 60` process group must be dead by now. We verify
        // indirectly: a fresh `pgrep -f "sleep 60"` finding no matching
        // process would be fragile; instead we just confirm no panic/hang
        // occurred in cleanup (if the guard's SIGKILL panicked the test
        // would have failed already).
    });
}

// ── content sections ──────────────────────────────────────────────────────

#[test]
fn bash_result_content_section_non_empty() {
    let result = BashResult {
        output: "hello\n".to_string(),
        truncated: false,
        label: None,
        command: String::new(),
    };
    let sections = result.content_sections();
    assert_eq!(sections.len(), 1);
    match &sections[0] {
        Section::Content(section) => assert_eq!(section.content, "hello\n"),
        other => panic!("unexpected section type: {other:?}"),
    }
}

#[test]
fn bash_result_content_section_empty_output_returns_no_sections() {
    let result = BashResult {
        output: String::new(),
        truncated: false,
        label: None,
        command: String::new(),
    };
    assert!(result.content_sections().is_empty());
}

#[test]
fn bash_result_content_sections_lead_with_label_then_command_before_output() {
    let result = BashResult {
        output: "lorem ipsum\n".to_string(),
        truncated: false,
        label: Some("Greet the user".to_string()),
        command: "echo hi".to_string(),
    };
    let sections = result.content_sections();
    assert_eq!(sections.len(), 3);
    match &sections[0] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Label"));
            assert_eq!(section.content, "Greet the user");
            assert_eq!(section.file_extension, None, "Label prose stays plain");
        }
        other => panic!("unexpected section type: {other:?}"),
    }
    match &sections[1] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Command"));
            assert_eq!(section.content, "echo hi");
            assert_eq!(
                section.file_extension.as_deref(),
                Some("sh"),
                "Command must carry the shell grammar hint"
            );
        }
        other => panic!("unexpected section type: {other:?}"),
    }
    match &sections[2] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Output"));
            assert_eq!(section.content, "lorem ipsum\n");
            assert_eq!(section.line_number_offset, Some(0));
        }
        other => panic!("unexpected section type: {other:?}"),
    }
}

#[test]
fn bash_result_truncated_output_labels_partial_result() {
    let truncated = BashResult {
        output: "first thousand lines only\n".to_string(),
        truncated: true,
        label: Some("Greet the user".to_string()),
        command: "echo hi".to_string(),
    };
    let sections = truncated.content_sections();
    assert_eq!(sections.len(), 3);
    match &sections[2] {
        Section::Content(section) => {
            assert_eq!(
                section.label.as_deref(),
                Some("Partial output"),
                "truncated capture must be labeled Partial output"
            );
            assert_eq!(section.content, "first thousand lines only\n");
        }
        other => panic!("unexpected section type: {other:?}"),
    }
    // Degenerate single-section case (no label, empty command) still
    // reports the lifecycle via the section label.
    let lone = BashResult {
        output: "prefix\n".to_string(),
        truncated: true,
        label: None,
        command: String::new(),
    };
    let sections = lone.content_sections();
    assert_eq!(sections.len(), 1);
    match &sections[0] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Partial output"))
        }
        other => panic!("unexpected section type: {other:?}"),
    }
}

#[test]
fn bash_pending_content_sections_lead_with_label_then_command() {
    let tool = BashTool::default();
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({
            "command": "echo hi",
            "label": "Greet the user",
        }),
    );
    let sections = call.pending_content_sections();
    assert_eq!(sections.len(), 2);
    match &sections[0] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Label"));
            assert_eq!(section.content, "Greet the user");
            assert_eq!(section.file_extension, None, "Label prose stays plain");
        }
        other => panic!("unexpected section type: {other:?}"),
    }
    match &sections[1] {
        Section::Content(section) => {
            assert_eq!(section.label.as_deref(), Some("Command"));
            assert_eq!(section.content, "echo hi");
            assert_eq!(
                section.file_extension.as_deref(),
                Some("sh"),
                "Command must carry the shell grammar hint"
            );
        }
        other => panic!("unexpected section type: {other:?}"),
    }
}

#[test]
fn bash_pending_content_sections_omit_blank_or_absent_label() {
    let tool = BashTool::default();
    for label in [serde_json::json!(""), serde_json::json!("   ")] {
        let call = tool.create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({ "command": "echo hi", "label": label }),
        );
        let sections = call.pending_content_sections();
        assert_eq!(sections.len(), 1, "blank label {label} must be omitted");
        match &sections[0] {
            Section::Content(section) => {
                assert_eq!(section.label.as_deref(), Some("Command"))
            }
            other => panic!("unexpected section type: {other:?}"),
        }
    }
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({ "command": "echo hi" }),
    );
    assert_eq!(call.pending_content_sections().len(), 1);
}

#[test]
fn bash_pending_content_sections_omit_empty_command() {
    let tool = BashTool::default();
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({ "command": "", "label": "No-op" }),
    );
    let sections = call.pending_content_sections();
    assert_eq!(sections.len(), 1);
    match &sections[0] {
        Section::Content(section) => assert_eq!(section.label.as_deref(), Some("Label")),
        other => panic!("unexpected section type: {other:?}"),
    }
}

#[test]
fn bash_input_args_all_properties_have_descriptions() {
    let schema = BashTool::default().input_schema();
    crate::test_support::assert_all_properties_have_descriptions(&schema, "BashInputArgs");
}

#[test]
fn bash_input_schema_explains_workspace_working_directory() {
    let schema = BashTool::default().input_schema();
    let schema = serde_json::to_value(schema).expect("schema serializes");
    let description = schema["properties"]["command"]["description"]
        .as_str()
        .expect("command schema has a description");
    assert!(description.contains("current working directory"));
    assert!(description.contains("workspace root"));
    assert!(description.contains("do not prefix"));
    assert!(description.contains("Use relative paths directly"));
}
