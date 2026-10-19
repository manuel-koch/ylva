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

use super::super::invocation_render::*;
use crate::agent::tools::read::ReadTool;
use crate::agent::tools::Tool;

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
    // An absent (null) mode and offset=0 are defaults and must be
    // suppressed; an explicit mode is a real choice and must show.
    let rendered = render_tool_invocation(
        read_tool().as_ref(),
        &serde_json::json!({
            "path": "some/document.md",
            "mode": null,
            "offset": 0,
            "limit": 32,
        }),
    );
    assert_eq!(rendered, "Read some/document.md [limit=32]");
    let rendered_explicit_mode = render_tool_invocation(
        read_tool().as_ref(),
        &serde_json::json!({
            "path": "some/document.md",
            "mode": "lines",
            "offset": 0,
            "limit": 32,
        }),
    );
    assert_eq!(
        rendered_explicit_mode,
        "Read some/document.md [mode=lines, limit=32]"
    );
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
fn render_truncates_long_bracketed_props_with_an_ellipsis() {
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
        rendered.contains("Lorem ipsum dolor sit amet"),
        "the head of an over-budget prop must stay visible: {rendered}"
    );
    assert!(
        rendered.contains('…'),
        "an over-budget prop is truncated, not replaced: {rendered}"
    );
    assert!(
        !rendered.contains("eiusmod tempor incididunt"),
        "the tail is cut to keep the line short: {rendered}"
    );
    assert!(!rendered.contains("elided"), "{rendered}");
    let prop_value = rendered
        .split_once("content=")
        .expect("content prop is rendered")
        .1
        .trim_end_matches(']');
    assert!(
        prop_value.len() <= STRING_ELIDE_LIMIT,
        "a prop respects its budget: {prop_value}"
    );
}

#[test]
fn render_inlines_the_whole_primary_when_it_fits_the_primary_budget() {
    // A search query is the call's payload: 55 bytes clears the old
    // 40-byte prop budget (which used to bracket and elide it into a
    // length placeholder) and fits the primary budget, so it must appear
    // in full, inline.
    let terms = "agent-harnesses that can be run on MacOS with good UX";
    assert!(terms.len() > STRING_ELIDE_LIMIT);
    assert!(terms.len() <= PRIMARY_ARG_DISPLAY_LIMIT);

    let rendered = render_invocation_from_title_arguments(
        "Web Search",
        &[("terms".to_string(), serde_json::json!(terms))],
    );

    assert!(
        rendered.contains(terms),
        "the query must be fully visible: {rendered}"
    );
    assert!(
        !rendered.contains('['),
        "the payload is the inline primary, not a bracketed prop: {rendered}"
    );
    assert!(!rendered.contains("elided"), "{rendered}");
}

#[test]
fn render_truncates_an_over_budget_primary_with_an_ellipsis() {
    let command = "echo \"=== Top-level listing ===\"; ls -la; echo; echo \"=== pwd ===\"; \
                   pwd; find . -type f | wc -l";
    assert!(command.len() > PRIMARY_ARG_DISPLAY_LIMIT);

    let rendered = render_invocation_from_title_arguments(
        "Bash",
        &[("command".to_string(), serde_json::json!(command))],
    );

    assert!(
        rendered.starts_with("Bash echo"),
        "the head of the command must stay visible: {rendered}"
    );
    assert!(
        rendered.contains('…'),
        "an over-budget primary is truncated: {rendered}"
    );
    assert!(
        !rendered.contains('['),
        "the payload is inline, not bracketed: {rendered}"
    );
    assert!(!rendered.contains("elided"), "{rendered}");
    assert!(
        rendered.len() <= "Bash ".len() + PRIMARY_ARG_DISPLAY_LIMIT,
        "the line respects the primary budget: {rendered}"
    );
}

#[test]
fn render_qualifies_the_title_with_the_subtype_argument() {
    let tool: Arc<dyn Tool> = Arc::new(
        crate::agent::tools::spawn_subagent::SpawnSubagentTool::new(false),
    );

    let with_subtype = render_tool_invocation(
        tool.as_ref(),
        &serde_json::json!({
            "label": "Analyze read tool features",
            "task": "list the read tool's features",
            "agent_type": "local-discovery",
        }),
    );
    assert!(
        with_subtype.starts_with("Spawn Subagent (local-discovery) "),
        "the subtype reads as part of the title: {with_subtype}"
    );
    assert!(
        !with_subtype.contains("agent_type"),
        "the subtype must not be rendered a second time as a prop: {with_subtype}"
    );
    assert!(
        with_subtype.contains("\"Analyze read tool features\""),
        "the label stays the inline primary: {with_subtype}"
    );

    let without_subtype = render_tool_invocation(
        tool.as_ref(),
        &serde_json::json!({
            "label": "Analyze read tool features",
            "task": "list the read tool's features",
        }),
    );
    assert!(
        without_subtype.starts_with("Spawn Subagent "),
        "an unspecified subtype qualifies nothing: {without_subtype}"
    );
    assert!(
        !without_subtype.contains('('),
        "no parenthetical without an explicit subtype: {without_subtype}"
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

#[test]
fn render_denied_invocation_keeps_short_command_inline() {
    let rendered = render_denied_invocation(
        "Bash",
        &[(
            "command".to_string(),
            serde_json::json!("find . -mindepth 1 -type d | wc -l"),
        )],
    );
    assert_eq!(rendered, "Bash find . -mindepth 1 -type d | wc -l");
}

#[test]
fn render_denied_invocation_shows_a_long_command_in_full() {
    // 73-byte command: over the 40-byte prop budget that bounds a normal
    // status line, but within DENIED_ARG_DISPLAY_LIMIT (80), so a denial
    // line must still show it in full.
    let command = "echo \"=== Top-level listing ===\"; ls -la; echo; echo \"=== pwd ===\"; pwd";
    assert!(command.len() > STRING_ELIDE_LIMIT);
    assert!(command.len() <= DENIED_ARG_DISPLAY_LIMIT);
    let rendered = render_denied_invocation(
        "Bash",
        &[("command".to_string(), serde_json::json!(command))],
    );
    assert_eq!(rendered, format!("Bash {command}"));
}

#[test]
fn render_denied_invocation_truncates_very_long_command_with_ellipsis() {
    let command =
        "echo \"=== Files (all, incl. hidden & git-ignored) ===\"; find . -type f | wc -l; \
                   find . -mindepth 1 -type d | wc -l; find . -mindepth 1 | wc -l";
    assert!(command.len() > DENIED_ARG_DISPLAY_LIMIT);
    let rendered = render_denied_invocation(
        "Bash",
        &[("command".to_string(), serde_json::json!(command))],
    );
    assert!(
        rendered.starts_with("Bash echo \"=== Files"),
        "command prefix must stay visible: {rendered}"
    );
    assert!(
        rendered.contains('…'),
        "over-limit command must end with an ellipsis: {rendered}"
    );
    let command_part = rendered.trim_start_matches("Bash ");
    assert!(
        command_part.len() <= DENIED_ARG_DISPLAY_LIMIT,
        "truncated command must respect the display limit: {command_part}"
    );
    assert!(
        !command_part.contains("wc -l; find"),
        "truncated command must not include the tail: {command_part}"
    );
}

#[test]
fn truncate_with_ellipsis_respects_utf8_char_boundaries() {
    let text = "日本語".repeat(50);
    let truncated = truncate_with_ellipsis(&text, DENIED_ARG_DISPLAY_LIMIT);
    assert!(truncated.len() <= DENIED_ARG_DISPLAY_LIMIT);
    assert!(truncated.ends_with('…'));
    assert!(truncated.starts_with("日本語"));
}

#[test]
fn truncate_with_ellipsis_keeps_short_text_unchanged() {
    assert_eq!(
        truncate_with_ellipsis("find . | wc -l", 80),
        "find . | wc -l"
    );
}

#[test]
fn truncate_with_ellipsis_strips_trailing_whitespace_before_the_marker() {
    // The 12th byte of the budget falls on the space after "world";
    // rendering `word …` would read as a broken word.
    let truncated = truncate_with_ellipsis("hello world example", 15);
    assert_eq!(truncated, "hello world…");
    assert!(!truncated.contains(" …"), "{truncated}");
    assert!(truncated.ends_with('…'), "{truncated}");
}
