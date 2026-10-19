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

use super::super::ToolCallResult;
use super::engine::{search_blocking, search_files};
use super::*;
use serde_json::json;
use tempfile::TempDir;

// ── approval ──────────────────────────────────────────────────────────────

#[test]
fn search_tool_no_path_is_auto_approved() {
    let tool = SearchTool::default();
    let args = serde_json::json!({ "pattern": "foo" });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::AutoApproved
    ));
}

#[test]
fn search_tool_workspace_path_is_auto_approved() {
    let tool = SearchTool::default();
    // cwd in tests is the workspace root; src/ is inside the workspace.
    let args = serde_json::json!({ "pattern": "fn main", "path": "src" });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::AutoApproved
    ));
}

#[test]
fn search_tool_outside_workspace_path_requires_user_approval() {
    let tool = SearchTool::default();
    let args = serde_json::json!({ "pattern": "secret", "path": "/etc" });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::UserApprovalRequired
    ));
}

// ── core search ───────────────────────────────────────────────────────────

#[test]
fn search_finds_match_in_file() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("hello.txt"), "hello world\nfoo bar\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "hello".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1);
        assert!(result.text().contains(":1: hello world"));
        assert!(!result.truncated);
    });
}

#[test]
fn search_no_match_returns_empty_sections() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "apple banana\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "cherry".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert!(result.sections.is_empty());
    });
}

#[test]
fn search_invalid_regex_returns_error() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let result = search_files(SearchInputArgs {
            pattern: "[unclosed".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await;
        assert!(result.is_err());
    });
}

#[test]
fn search_case_insensitive_matches_regardless_of_case() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("b.txt"), "Hello World\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "hello".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1, "case-insensitive should match");

        let case_sensitive_result = search_files(SearchInputArgs {
            pattern: "hello".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert!(
            case_sensitive_result.sections.is_empty(),
            "case-sensitive should not match 'Hello'"
        );
    });
}

#[test]
fn search_context_lines_included_in_output() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("c.txt"),
            "line1\nline2\nMATCH\nline4\nline5\n",
        )
        .expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "MATCH".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 1,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1);
        // Match line uses ':' separator; context lines use '-' separator.
        assert!(result.text().contains(":3: MATCH"), "match line present");
        assert!(
            result.text().contains("-2- line2"),
            "before-context present"
        );
        assert!(result.text().contains("-4- line4"), "after-context present");
    });
}

#[test]
fn search_binary_file_is_skipped() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        // Write a file with a null byte — triggers binary detection.
        let mut binary_content = b"text before\x00binary after".to_vec();
        binary_content.extend_from_slice(b"text");
        std::fs::write(dir.path().join("binary.bin"), &binary_content).expect("write");
        // Also write a plain text file so the walker has something to visit.
        std::fs::write(dir.path().join("text.txt"), "text before binary after\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "text before".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        // Only the text file should match; the binary file must be skipped.
        assert_eq!(result.sections.len(), 1);
        assert!(result.sections[0].file_path.ends_with("text.txt"));
    });
}

#[test]
fn search_output_truncated_at_1000_line_cap() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        // Create enough matching lines to exceed the 1000-line cap, each
        // short enough that the combined output stays under the byte cap
        // so only the line cap is exercised here.
        let line = "NEEDLE\n";
        let content = line.repeat(1500);
        std::fs::write(dir.path().join("big.txt"), &content).expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "NEEDLE".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert!(result.truncated, "output must be truncated");
        let total_emitted_lines: usize = result.sections.iter().map(|s| s.lines.len()).sum();
        assert_eq!(
            total_emitted_lines, SEARCH_OUTPUT_LIMIT_LINES,
            "must cap at whole lines, never split mid-line"
        );
        // The metadata() must advertise the truncation.
        let meta = result
            .metadata()
            .expect("truncated result must have metadata");
        let obj = meta.as_object().expect("metadata is a JSON object");
        assert!(obj.contains_key("truncated"), "truncated key expected");
        assert_eq!(obj["truncated"], json!(true));
    });
}

#[test]
fn search_output_truncated_at_byte_cap_with_few_huge_lines() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        // A handful of matching lines, each far larger than the byte cap
        // divided by the line cap, so the byte cap (not the line cap)
        // must trigger truncation.
        let huge_line = "a".repeat(SEARCH_OUTPUT_LIMIT_BYTES) + " NEEDLE\n";
        let content = huge_line.repeat(3);
        std::fs::write(dir.path().join("huge.txt"), &content).expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "NEEDLE".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert!(result.truncated, "output must be truncated");
        let total_emitted_lines: usize = result.sections.iter().map(|s| s.lines.len()).sum();
        assert!(
            total_emitted_lines < SEARCH_OUTPUT_LIMIT_LINES,
            "byte cap must trigger before the line cap: got {total_emitted_lines} lines"
        );
        let total_emitted_bytes: usize = result
            .sections
            .iter()
            .flat_map(|s| s.lines.iter())
            .map(|line| line.content.len())
            .sum();
        assert!(
            total_emitted_bytes <= SEARCH_OUTPUT_LIMIT_BYTES,
            "output must never exceed the byte cap: got {total_emitted_bytes} bytes"
        );
    });
}

#[test]
fn search_section_label_is_file_path() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let file_path = dir.path().join("labeled.txt");
        std::fs::write(&file_path, "target line\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "target".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1);
        assert_eq!(
            result.sections[0].file_path,
            file_path.display().to_string()
        );
    });
}

#[test]
fn runtime_token_budget_caps_search_results_and_marks_them_truncated() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("results.txt"),
            "needle alpha\nneedle beta\n",
        )
        .expect("write fixture");
        let mut call = SearchTool::default().create_call(
            uuid::Uuid::new_v4(),
            json!({"pattern":"needle", "path":dir.path().display().to_string()}),
        );
        call.set_runtime_output_token_limit(Some(3));

        let result = call.run().await.expect("search result");

        assert!(crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&result.text()) <= 3);
        assert_eq!(result.metadata().expect("metadata")["truncated"], true);
    });
}

#[test]
fn search_relative_path_returns_relative_result_paths() {
    crate::test_support::run_locked(async {
        // Call search_blocking directly with a relative root so we can
        // assert path form without mutating the process-global cwd.
        // `src/` is guaranteed to exist and contain Rust files with `fn `.
        let regex = regex::Regex::new(r"\bfn \b").expect("regex");
        let result = search_blocking(&regex, std::path::Path::new("src"), 0, None, false, false);
        assert!(
            !result.sections.is_empty(),
            "expected at least one match in src/"
        );
        for section in &result.sections {
            assert!(
                !std::path::Path::new(&section.file_path).is_absolute(),
                "result path must be relative, got: {}",
                section.file_path
            );
            assert!(
                section.file_path.starts_with("src"),
                "result path must be rooted at 'src', got: {}",
                section.file_path
            );
        }
    });
}

#[test]
fn search_glob_filters_to_matching_files_only() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("code.rs"), "needle in rust\n").expect("write");
        std::fs::write(dir.path().join("docs.txt"), "needle in text\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "needle".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: Some("*.rs".to_string()),
            files_with_matches: false,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1, "only .rs file should be searched");
        assert!(
            result.sections[0].file_path.ends_with(".rs"),
            "matched file must be .rs"
        );
    });
}

#[test]
fn search_invalid_glob_returns_error() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let result = search_files(SearchInputArgs {
            pattern: "needle".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: false,
            context_lines: 0,
            glob: Some("[bad".to_string()),
            files_with_matches: false,
            only_matches: false,
        })
        .await;
        assert!(result.is_err(), "invalid glob must return ToolError");
    });
}

#[test]
fn search_glob_with_dotdot_segment_returns_error() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        for glob_str in ["../*.rs", "src/../*.rs", "../**/*.rs"] {
            let result = search_files(SearchInputArgs {
                pattern: "needle".to_string(),
                path: Some(dir.path().display().to_string()),
                case_sensitive: false,
                context_lines: 0,
                glob: Some(glob_str.to_string()),
                files_with_matches: false,
                only_matches: false,
            })
            .await;
            assert!(
                result.is_err(),
                "glob '{glob_str}' with '..' must return ToolError"
            );
        }
    });
}

#[test]
fn search_files_with_matches_returns_only_paths() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n").expect("write");
        std::fs::write(dir.path().join("b.rs"), "no match here\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "fn foo".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: true,
            only_matches: false,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1, "one matching file");
        // In files_with_matches mode the section has no content lines.
        assert!(
            result.sections[0].lines.is_empty(),
            "files_with_matches sections must have no content lines"
        );
        // text() output must contain only file paths.
        let text = result.text();
        assert!(
            text.contains("a.rs"),
            "path of matching file must appear in text"
        );
        assert!(
            !text.contains("fn foo"),
            "line content must not appear in text"
        );
    });
}

#[test]
fn search_only_matches_returns_match_substrings() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("code.rs"), "foo bar foo\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "foo".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: false,
            only_matches: true,
        })
        .await
        .expect("search ok");
        assert_eq!(result.sections.len(), 1, "one section for matching file");
        // "foo bar foo" has two occurrences of "foo".
        assert_eq!(
            result.sections[0].lines.len(),
            2,
            "two match occurrences on the line"
        );
        for line in &result.sections[0].lines {
            assert_eq!(line.content, "foo", "only the matched substring is stored");
        }
    });
}

#[test]
fn search_only_matches_ignores_context_lines() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("code.rs"),
            "context before\nfoo match\ncontext after\n",
        )
        .expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "foo".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 2,
            glob: None,
            files_with_matches: false,
            only_matches: true,
        })
        .await
        .expect("search ok");
        // context_lines must be ignored in only_matches mode.
        assert_eq!(
            result.sections[0].lines.len(),
            1,
            "only the matched substring"
        );
    });
}

#[test]
fn search_files_with_matches_and_only_matches_returns_error() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("code.rs"), "foo\n").expect("write");
        let result = search_files(SearchInputArgs {
            pattern: "foo".to_string(),
            path: Some(dir.path().display().to_string()),
            case_sensitive: true,
            context_lines: 0,
            glob: None,
            files_with_matches: true,
            only_matches: true,
        })
        .await;
        assert!(
            result.is_err(),
            "files_with_matches and only_matches are mutually exclusive"
        );
    });
}

#[test]
fn search_input_args_all_properties_have_descriptions() {
    let schema = SearchTool::default().input_schema();
    crate::test_support::assert_all_properties_have_descriptions(&schema, "SearchInputArgs");
}

#[test]
fn description_uses_the_output_byte_limit_constant() {
    let tool = SearchTool::default();
    let description = tool.description();
    assert!(description.contains(&format!("{} bytes", SEARCH_OUTPUT_LIMIT_BYTES)));
}
