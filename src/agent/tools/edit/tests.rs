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

use super::super::{Section, ToolCallResult};
use super::patch::apply_edit;
use super::*;
use similar::TextDiff;
use std::io::Write;
use tempfile::NamedTempFile;

fn write_temp(content: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(content.as_bytes()).unwrap();
    file.flush().unwrap();
    file
}

fn make_args_search_replace(path: &str, old_string: &str, new_string: &str) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "mode": "search_replace",
        "old_string": old_string,
        "new_string": new_string,
    })
}

fn make_args_line_range(
    path: &str,
    start_line: usize,
    end_line: usize,
    new_string: &str,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "mode": "line_range_replace",
        "start_line": start_line,
        "end_line": end_line,
        "new_string": new_string,
    })
}

// ── SearchReplace ──────────────────────────────────────────────────────

fn search_replace_args(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> EditFileArgs {
    EditFileArgs {
        path: path.to_string(),
        mode: EditMode::SearchReplace,
        new_string: new_string.to_string(),
        old_string: Some(old_string.to_string()),
        replace_all,
        start_line: None,
        end_line: None,
    }
}

fn line_range_args(
    path: &str,
    start_line: usize,
    end_line: usize,
    new_string: &str,
) -> EditFileArgs {
    EditFileArgs {
        path: path.to_string(),
        mode: EditMode::LineRangeReplace,
        new_string: new_string.to_string(),
        old_string: None,
        replace_all: false,
        start_line: Some(start_line),
        end_line: Some(end_line),
    }
}

#[test]
fn search_replace_happy_path() {
    let content = "hello world\ngoodbye world\n";
    let (patched, outcome) = apply_edit(
        content,
        &search_replace_args("test.txt", "hello world", "hi world", false),
    )
    .unwrap();
    assert_eq!(patched, "hi world\ngoodbye world\n");
    assert!(matches!(
        outcome,
        EditApplied::SearchReplace {
            replacements_made: 1
        }
    ));
}

#[test]
fn search_replace_diff_contains_old_and_new() {
    let original = "line one\nline two\n";
    let patched = "line one\nline TWO\n";
    let diff = TextDiff::from_lines(original, patched)
        .unified_diff()
        .context_radius(DIFF_CONTEXT_LINES)
        .header("a/test.txt", "b/test.txt")
        .to_string();
    assert!(diff.contains("-line two"));
    assert!(diff.contains("+line TWO"));
}

#[test]
fn search_replace_not_found_returns_error() {
    let content = "hello world\n";
    let result = apply_edit(
        content,
        &search_replace_args("test.txt", "not present", "replacement", false),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn search_replace_replaces_only_first_occurrence() {
    let content = "foo bar foo\n";
    let (patched, outcome) = apply_edit(
        content,
        &search_replace_args("test.txt", "foo", "baz", false),
    )
    .unwrap();
    assert_eq!(patched, "baz bar foo\n");
    assert!(matches!(
        outcome,
        EditApplied::SearchReplace {
            replacements_made: 1
        }
    ));
}

#[test]
fn search_replace_replace_all_replaces_every_occurrence() {
    let content = "foo bar foo baz foo\n";
    let (patched, outcome) = apply_edit(
        content,
        &search_replace_args("test.txt", "foo", "qux", true),
    )
    .unwrap();
    assert_eq!(patched, "qux bar qux baz qux\n");
    assert!(matches!(
        outcome,
        EditApplied::SearchReplace {
            replacements_made: 3
        }
    ));
}

// ── LineRange ──────────────────────────────────────────────────────────

#[test]
fn line_range_happy_path() {
    let content = "line1\nline2\nline3\n";
    let (patched, outcome) =
        apply_edit(content, &line_range_args("test.txt", 2, 2, "replaced")).unwrap();
    assert_eq!(patched, "line1\nreplaced\nline3\n");
    assert!(matches!(
        outcome,
        EditApplied::LineRangeReplace {
            start_line: 2,
            end_line: 2
        }
    ));
}

#[test]
fn line_range_diff_present() {
    let original = "a\nb\nc\n";
    let patched = "a\nX\nc\n";
    let diff = TextDiff::from_lines(original, patched)
        .unified_diff()
        .context_radius(DIFF_CONTEXT_LINES)
        .header("a/f", "b/f")
        .to_string();
    assert!(diff.contains("-b"));
    assert!(diff.contains("+X"));
}

#[test]
fn line_range_out_of_bounds_returns_error() {
    let content = "line1\nline2\n";
    let result = apply_edit(content, &line_range_args("test.txt", 1, 5, "x"));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("out of bounds"));
}

#[test]
fn line_range_start_greater_than_end_returns_error() {
    let content = "line1\nline2\n";
    let result = apply_edit(content, &line_range_args("test.txt", 3, 1, "x"));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must not exceed"));
}

#[tokio::test]
async fn nonexistent_file_returns_error() {
    let args = make_args_search_replace("/no/such/file.txt", "x", "y");
    let id = uuid::Uuid::new_v4();
    let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
    let result = Box::new(call).run().await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[tokio::test]
async fn nonexistent_file_via_line_range_returns_error() {
    let args = make_args_line_range("/no/such/file.txt", 1, 1, "x");
    let id = uuid::Uuid::new_v4();
    let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
    let result = Box::new(call).run().await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn pending_content_sections_returns_diff_section() {
    let file = write_temp("hello\nworld\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let id = uuid::Uuid::new_v4();
    let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
    let sections = call.pending_content_sections();
    assert_eq!(sections.len(), 1);
    assert!(matches!(sections[0], Section::Diff(_)));
    if let Section::Diff(ref diff) = sections[0] {
        assert_eq!(diff.label, Some(path));
        assert!(diff.content.contains("-hello"));
        assert!(diff.content.contains("+hi"));
    }
}

#[test]
fn title_arguments_excludes_old_and_new_string_when_diff_is_available() {
    // old_string/new_string are already fully represented by the diff
    // section, so they must not also appear in the inline title args.
    let file = write_temp("hello\nworld\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
    let title_arguments = call.title_arguments();
    assert!(
        title_arguments
            .iter()
            .all(|(k, _)| k != "old_string" && k != "new_string"),
        "old_string/new_string should be excluded from title_arguments: {title_arguments:?}"
    );
    // Other scalar args (path, mode) remain visible.
    assert!(title_arguments.iter().any(|(k, _)| k == "path"));
    assert!(title_arguments.iter().any(|(k, _)| k == "mode"));
}

#[test]
fn title_arguments_keeps_old_and_new_string_when_no_diff_was_computed() {
    // When old_string isn't found, no diff section is produced, so
    // old_string/new_string are the only place left to see what was
    // searched for — keep them in the title args.
    let file = write_temp("hello\nworld\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "not-present", "hi");
    let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
    assert!(call.pending_content_sections().is_empty());
    let title_arguments = call.title_arguments();
    assert!(title_arguments.iter().any(|(k, _)| k == "old_string"));
    assert!(title_arguments.iter().any(|(k, _)| k == "new_string"));
}

#[tokio::test]
async fn run_writes_patched_content() {
    let file = write_temp("alpha\nbeta\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "beta", "gamma");
    let id = uuid::Uuid::new_v4();
    let call = create_edit_call(id, args, DEFAULT_TOOL_TIMEOUT_SECS);
    Box::new(call).run().await.unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, "alpha\ngamma\n");
}

#[cfg(unix)]
#[tokio::test]
async fn run_refuses_symlink_swapped_after_approval() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("target.txt");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    std::fs::write(&victim, "alpha\nbeta\n").unwrap();
    let path_str = path.to_str().unwrap().to_string();
    let args = make_args_search_replace(&path_str, "beta", "gamma");
    let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);

    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();

    let Err(error) = Box::new(call).run().await else {
        panic!("swap must be refused");
    };
    assert!(error.to_string().contains("now resolves"), "{error}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "alpha\nbeta\n");
}

// ── EditResult ─────────────────────────────────────────────────────────

#[test]
fn edit_result_text_search_replace() {
    let result = EditResult {
        path: "src/foo.rs".to_string(),
        outcome: EditApplied::SearchReplace {
            replacements_made: 2,
        },
        diff: String::new(),
    };
    assert!(result.text().contains("src/foo.rs"));
    assert!(result.text().contains("2"));
}

#[test]
fn edit_result_text_line_range() {
    let result = EditResult {
        path: "src/foo.rs".to_string(),
        outcome: EditApplied::LineRangeReplace {
            start_line: 3,
            end_line: 5,
        },
        diff: String::new(),
    };
    assert!(result.text().contains("src/foo.rs"));
    assert!(result.text().contains("3"));
    assert!(result.text().contains("5"));
}

#[test]
fn edit_result_metadata_search_replace() {
    let result = EditResult {
        path: "src/foo.rs".to_string(),
        outcome: EditApplied::SearchReplace {
            replacements_made: 1,
        },
        diff: String::new(),
    };
    let meta = result.metadata().unwrap();
    assert_eq!(meta["replacements_made"], serde_json::json!(1));
    assert!(result.metadata_schema().is_some());
}

#[test]
fn edit_result_metadata_line_range() {
    let result = EditResult {
        path: "src/foo.rs".to_string(),
        outcome: EditApplied::LineRangeReplace {
            start_line: 2,
            end_line: 4,
        },
        diff: String::new(),
    };
    let meta = result.metadata().unwrap();
    assert_eq!(meta["start_line"], serde_json::json!(2));
    assert_eq!(meta["end_line"], serde_json::json!(4));
    assert!(meta.get("replacements_made").is_none() || meta["replacements_made"].is_null());
}

#[tokio::test]
async fn edit_result_content_sections_returns_diff_after_run() {
    let file = write_temp("hello\nworld\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
    let result = Box::new(call).run().await.unwrap();
    let sections = result.content_sections();
    assert_eq!(sections.len(), 1, "diff section must be present after run");
    assert!(
        matches!(sections[0], crate::agent::tools::Section::Diff(_)),
        "section must be a Diff"
    );
    if let crate::agent::tools::Section::Diff(ref diff) = sections[0] {
        assert!(
            diff.content.contains("-hello"),
            "diff must show removed line"
        );
        assert!(diff.content.contains("+hi"), "diff must show added line");
        assert_eq!(diff.label.as_deref(), Some(&path as &str));
    }
}

// ── EditTool ───────────────────────────────────────────────────────────

#[test]
fn edit_tool_needs_approval_missing_path() {
    let tool = EditTool::default();
    let args = serde_json::json!({});
    assert!(matches!(
        tool.needs_approval(&args),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
}

#[test]
fn edit_tool_needs_approval_empty_path() {
    let tool = EditTool::default();
    let args = serde_json::json!({ "path": "" });
    assert!(matches!(
        tool.needs_approval(&args),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
}

#[test]
fn edit_tool_needs_approval_valid_path_requires_user_approval() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "old_string": "foo",
        "new_string": "bar",
    });
    assert!(matches!(
        tool.needs_approval(&args),
        crate::sandbox::ApprovalCheck::UserApprovalRequired
    ));
}

#[test]
fn edit_tool_validate_args_invalid_mode_args_returns_error() {
    let tool = EditTool::default();
    // path present but no mode field → schema validation fails
    let args = serde_json::json!({ "path": "src/lib.rs" });
    assert!(tool.validate_args(&args).is_err());
}

#[test]
fn edit_tool_validate_args_search_replace_missing_old_string_returns_error() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "mode": "search_replace",
        "new_string": "bar",
    });
    let error = tool.validate_args(&args).unwrap_err();
    assert!(error.contains("old_string"), "error was: {error}");
}

#[test]
fn edit_tool_validate_args_line_range_replace_missing_start_line_returns_error() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "mode": "line_range_replace",
        "new_string": "x",
        "end_line": 3,
    });
    let error = tool.validate_args(&args).unwrap_err();
    assert!(error.contains("start_line"), "error was: {error}");
}

#[test]
fn edit_tool_validate_args_line_range_replace_missing_end_line_returns_error() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "mode": "line_range_replace",
        "new_string": "x",
        "start_line": 1,
    });
    let error = tool.validate_args(&args).unwrap_err();
    assert!(error.contains("end_line"), "error was: {error}");
}

#[test]
fn edit_tool_validate_args_valid_search_replace_passes() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "mode": "search_replace",
        "old_string": "foo",
        "new_string": "bar",
    });
    assert!(tool.validate_args(&args).is_ok());
}

#[test]
fn edit_tool_validate_args_valid_line_range_replace_passes() {
    let tool = EditTool::default();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "mode": "line_range_replace",
        "new_string": "x",
        "start_line": 1,
        "end_line": 3,
    });
    assert!(tool.validate_args(&args).is_ok());
}

#[test]
fn edit_tool_schema_property_order() {
    let tool = EditTool::default();
    // All fields are now top-level in `EditFileArgs`, so each receives a schema-driven rank.
    assert_eq!(tool.argument_order("path"), 0);
    assert_eq!(tool.argument_order("mode"), 1);
    assert_eq!(tool.argument_order("new_string"), 3);
    assert_eq!(tool.argument_order("old_string"), 2);
    assert_eq!(tool.argument_order("replace_all"), 4);
    assert_eq!(tool.argument_order("start_line"), 5);
    assert_eq!(tool.argument_order("end_line"), 6);
}

#[test]
fn edit_call_argument_order_matches_tool() {
    let file = write_temp("hello\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let call = create_edit_call(uuid::Uuid::new_v4(), args, DEFAULT_TOOL_TIMEOUT_SECS);
    // path must be rank 0 so it renders as the keyless primary arg in the TUI.
    assert_eq!(call.argument_order("path"), 0);
    assert!(call.argument_order("path") < call.argument_order("mode"));
    assert!(call.argument_order("mode") < call.argument_order("old_string"));
}

#[test]
fn edit_tool_with_timeout() {
    let tool = EditTool::with_timeout(
        10,
        crate::sandbox::SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        ))),
    );
    assert_eq!(tool.timeout_secs(), 10);
}

#[test]
fn edit_tool_default_timeout() {
    let tool = EditTool::default();
    assert_eq!(tool.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
}

#[test]
fn edit_tool_name_and_description() {
    let tool = EditTool::default();
    assert_eq!(tool.name(), "edit");
    assert!(!tool.description().is_empty());
}

#[test]
fn edit_tool_create_call_for_nonexistent_file_produces_error_call() {
    let tool = EditTool::default();
    let id = uuid::Uuid::new_v4();
    let args = make_args_search_replace("/no/such/file.txt", "x", "y");
    // create_call embeds the execution error into the EditCall; verify it is well-formed.
    let call = tool.create_call(id, args);
    assert_eq!(call.name(), "edit");
    assert_eq!(call.id(), id);
}

#[tokio::test]
async fn failing_edit_call_run_returns_error() {
    let tool = EditTool::default();
    let id = uuid::Uuid::new_v4();
    let args = make_args_search_replace("/no/such/file.txt", "x", "y");
    let call = tool.create_call(id, args);
    let result = call.run().await;
    assert!(result.is_err());
}

// ── EditCall trait methods ─────────────────────────────────────────────

#[test]
fn edit_call_trait_methods() {
    let file = write_temp("hello\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let id = uuid::Uuid::new_v4();
    let call = create_edit_call(id, args.clone(), DEFAULT_TOOL_TIMEOUT_SECS);
    assert_eq!(call.id(), id);
    assert_eq!(call.name(), "edit");
    assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
    let redacted = call.non_default_redacted_arguments();
    assert!(!redacted.is_empty());
}

// ── Line range edge cases ──────────────────────────────────────────────

#[test]
fn line_range_zero_start_line_returns_error() {
    let content = "line1\nline2\n";
    let result = apply_edit(content, &line_range_args("test.txt", 0, 1, "x"));
    assert!(result.is_err());
}

#[test]
fn line_range_delete_range_with_empty_new_string() {
    let content = "a\nb\nc\n";
    let (patched, _) = apply_edit(content, &line_range_args("test.txt", 2, 2, "")).unwrap();
    assert_eq!(patched, "a\nc\n");
}

#[test]
fn line_range_multi_line_replacement() {
    let content = "a\nb\nc\n";
    let (patched, _) = apply_edit(content, &line_range_args("test.txt", 1, 2, "X\nY")).unwrap();
    assert_eq!(patched, "X\nY\nc\n");
}

#[test]
fn edit_tool_non_default_redacted_arguments() {
    let tool = EditTool::default();
    let file = write_temp("hello\n");
    let path = file.path().to_str().unwrap().to_string();
    let args = make_args_search_replace(&path, "hello", "hi");
    let redacted = tool.non_default_redacted_arguments(&args);
    assert!(!redacted.is_empty());
}
