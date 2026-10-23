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
use super::result::GlobPathKind;
use super::traversal::*;
use super::*;
use crate::test_support::capture_logs;
use serde_json::{json, Value};
use tempfile::TempDir;

fn glob_args(pattern: impl Into<String>) -> GlobInputArgs {
    GlobInputArgs {
        pattern: pattern.into(),
        git_ignore: true,
        only_count: false,
        only_files: false,
        only_dirs: false,
        limit: None,
        dot_ignore: false,
    }
}

fn path_counts(files: usize, directories: usize) -> GlobMatchCounts {
    GlobMatchCounts {
        files,
        directories,
        total: files + directories,
    }
}

#[test]
fn glob_pattern_prefix_scopes_matches_to_subdirectory() {
    crate::test_support::run_locked(async {
        let search_root = TempDir::new().expect("tempdir");
        let docs_directory = search_root.path().join("docs");
        std::fs::create_dir(&docs_directory).expect("mkdir");
        std::fs::write(search_root.path().join("README.md"), "outside").expect("write");
        let markdown_path = docs_directory.join("README.md");
        std::fs::write(&markdown_path, "scoped").expect("write");

        let args = GlobInputArgs {
            pattern: format!("{}/**/*.md", docs_directory.display()),
            git_ignore: false,
            ..glob_args("")
        };
        let result = glob_files(&args).await.expect("glob ok");

        assert_eq!(
            result.matched_paths,
            vec![markdown_path.display().to_string()]
        );
    });
}

#[test]
fn glob_title_arguments_omit_unsupported_fields() {
    let tool = GlobTool::default();
    let call = tool.create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({
            "pattern": "docs/**/*.md",
            "path": "docs",
        }),
    );

    assert_eq!(
        call.title_arguments(),
        vec![("pattern".to_string(), serde_json::json!("docs/**/*.md"))]
    );
}

#[test]
fn glob_tool_orders_pattern_first_and_marks_defaults() {
    let tool = GlobTool::default();
    assert_eq!(tool.argument_order("pattern"), 0);
    assert!(tool.is_default_argument("git_ignore", &Value::Bool(true)));
    assert!(tool.is_default_argument("only_count", &Value::Bool(false)));
    assert!(tool.is_default_argument("only_files", &Value::Bool(false)));
    assert!(tool.is_default_argument("only_dirs", &Value::Bool(false)));
    assert!(tool.is_default_argument("limit", &Value::Null));
    assert!(!tool.is_default_argument("git_ignore", &Value::Bool(false)));
}

#[test]
fn glob_tool_with_timeout_constructor() {
    let tool = GlobTool::with_timeout(
        30,
        SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        ))),
    );
    assert_eq!(tool.timeout_secs(), 30);
}

#[test]
fn glob_tool_missing_pattern_is_auto_denied() {
    let tool = GlobTool::default();
    assert!(matches!(
        tool.needs_approval(&serde_json::json!({})),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
    assert!(matches!(
        tool.needs_approval(&serde_json::json!({ "pattern": "" })),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
}

#[test]
fn glob_with_git_ignore_false_matches_files() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(dir.path().join(name), name).expect("write");
        }
        std::fs::write(sub.join("c.txt"), "c").expect("write");

        let matches = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            ..glob_args("")
        })
        .await
        .expect("glob ok");
        let text = matches.text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().any(|l: &&str| l.ends_with("a.txt")));
        assert!(lines.iter().any(|l: &&str| l.ends_with("b.txt")));
        assert!(!lines.iter().any(|l: &&str| l.ends_with("c.txt")));
    });
}

#[test]
fn glob_with_git_ignore_true_matches_files() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(dir.path().join("ignored.txt"), "ignored").expect("write");
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");

        let result = glob_files(&glob_args(format!("{}/*.txt", dir.path().display())))
            .await
            .expect("glob ok");
        assert!(result
            .matched_paths
            .iter()
            .any(|path| path.ends_with("a.txt")));
        assert!(!result
            .matched_paths
            .iter()
            .any(|path| path.ends_with("ignored.txt")));
    });
}

#[test]
fn glob_with_git_ignore_true_keeps_single_star_within_directory() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let nested_directory = dir.path().join("nested");
        std::fs::create_dir_all(&nested_directory).expect("mkdir");
        let direct_path = dir.path().join("direct.md");
        let nested_path = nested_directory.join("nested.md");
        std::fs::write(&direct_path, "direct").expect("write");
        std::fs::write(&nested_path, "nested").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.md", dir.path().display()),
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert_eq!(
            result.matched_paths,
            vec![direct_path.display().to_string()]
        );
    });
}

#[test]
fn glob_only_files_excludes_dirs() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let subdirectory = dir.path().join("sub");
        std::fs::create_dir_all(&subdirectory).expect("mkdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/**", dir.path().display()),
            only_files: true,
            git_ignore: false,
            ..glob_args("")
        })
        .await
        .expect("glob ok");
        assert!(result
            .matched_paths
            .iter()
            .all(|path| std::path::Path::new(path).is_file()));
    });
}

#[test]
fn glob_only_dirs_excludes_files() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let subdirectory = dir.path().join("sub");
        std::fs::create_dir_all(&subdirectory).expect("mkdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(subdirectory.join("b.txt"), "b").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/**", dir.path().display()),
            only_dirs: true,
            git_ignore: false,
            ..glob_args("")
        })
        .await
        .expect("glob ok");
        assert!(!result.matched_paths.is_empty());
        assert!(result
            .matched_paths
            .iter()
            .all(|path| std::path::Path::new(path).is_dir()));
    });
}

#[test]
fn glob_structured_metadata_counts_files_directories_and_total() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        std::fs::write(dir.path().join("b.txt"), "b").expect("write");
        std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
        let pattern = format!("{}/*", dir.path().display());

        let result = glob_files(&GlobInputArgs {
            pattern: pattern.clone(),
            git_ignore: false,
            ..glob_args("")
        })
        .await
        .expect("glob ok");
        assert_eq!(result.matches, path_counts(2, 1));
        assert!(!result.truncated);
        assert_eq!(
            result.metadata(),
            Some(json!({
                "files": 2,
                "directories": 1,
                "total": 3,
                "truncated": false,
            }))
        );

        let files_only = glob_files(&GlobInputArgs {
            pattern: pattern.clone(),
            git_ignore: false,
            only_files: true,
            ..glob_args("")
        })
        .await
        .expect("glob files ok");
        assert_eq!(files_only.matches, path_counts(2, 0));
        assert!(!files_only.truncated);

        let directories_only = glob_files(&GlobInputArgs {
            pattern,
            git_ignore: false,
            only_dirs: true,
            ..glob_args("")
        })
        .await
        .expect("glob directories ok");
        assert_eq!(directories_only.matches, path_counts(0, 1));
        assert!(!directories_only.truncated);
    });
}

#[test]
fn glob_display_summary_omits_default_attributes() {
    let empty_result = GlobResult::new(vec![], vec![], path_counts(0, 0), false);
    assert_eq!(
        empty_result.display_summary(),
        vec![("total".to_string(), "0".to_string())]
    );
    let empty_metadata = empty_result.metadata().expect("glob metadata");
    assert_eq!(empty_metadata["files"], json!(0));
    assert_eq!(empty_metadata["directories"], json!(0));
    assert_eq!(empty_metadata["truncated"], json!(false));

    let files_result = GlobResult::new(vec![], vec![], path_counts(2, 0), false);
    assert_eq!(
        files_result.display_summary(),
        vec![("files".to_string(), "2".to_string())]
    );

    let directories_result = GlobResult::new(vec![], vec![], path_counts(0, 3), true);
    assert_eq!(
        directories_result.display_summary(),
        vec![
            ("directories".to_string(), "3".to_string()),
            ("truncated".to_string(), "true".to_string()),
        ]
    );

    let mixed_result = GlobResult::new(vec![], vec![], path_counts(2, 3), false);
    assert_eq!(
        mixed_result.display_summary(),
        vec![
            ("files".to_string(), "2".to_string()),
            ("directories".to_string(), "3".to_string()),
            ("total".to_string(), "5".to_string()),
        ]
    );
}

#[test]
fn glob_bare_double_star_requires_leaf_pattern_for_files() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
        std::fs::write(dir.path().join("file.txt"), "content").expect("write");

        let directories_pattern = format!("{}/**", dir.path().display());
        let directories_result = glob_files(&GlobInputArgs {
            pattern: directories_pattern,
            git_ignore: false,
            only_files: true,
            ..glob_args("")
        })
        .await
        .expect("glob directories pattern");
        assert!(directories_result.matched_paths.is_empty());
        assert_eq!(directories_result.matches, path_counts(0, 0));

        let files_pattern = format!("{}/**/*", dir.path().display());
        let files_result = glob_files(&GlobInputArgs {
            pattern: files_pattern,
            git_ignore: false,
            only_files: true,
            ..glob_args("")
        })
        .await
        .expect("glob files pattern");
        assert_eq!(files_result.matches, path_counts(1, 0));
        assert!(files_result
            .matched_paths
            .iter()
            .any(|path| path.ends_with("file.txt")));
    });
}

#[test]
fn glob_never_includes_search_root_in_paths_or_counts() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
        let pattern = format!("{}/**", dir.path().display());
        let root_path = dir.path().display().to_string();

        for git_ignore in [false, true] {
            let result = glob_files(&GlobInputArgs {
                pattern: pattern.clone(),
                git_ignore,
                ..glob_args("")
            })
            .await
            .expect("glob ok");

            assert!(
                !result.matched_paths.iter().any(|path| path == &root_path),
                "search root must never be returned"
            );
            assert_eq!(result.matches, path_counts(0, 1), "git_ignore={git_ignore}");
            assert!(!result.truncated, "git_ignore={git_ignore}");
        }
    });
}

#[test]
fn glob_contradictory_only_files_and_dirs_is_error() {
    crate::test_support::run_locked(async {
        let result = glob_files(&GlobInputArgs {
            pattern: "*.txt".to_string(),
            only_files: true,
            only_dirs: true,
            ..glob_args("")
        })
        .await;
        assert!(result.is_err());
    });
}

#[test]
fn glob_limit_caps_results_and_sets_truncated_metadata() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        for index in 0..5 {
            std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                .expect("write");
        }

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            limit: Some(3),
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert_eq!(result.matched_paths.len(), 3);
        assert_eq!(result.matches, path_counts(3, 0));
        assert!(result.truncated);
        assert_eq!(
            result.metadata(),
            Some(json!({
                "files": 3,
                "directories": 0,
                "total": 3,
                "truncated": true,
            }))
        );
    });
}

#[test]
fn glob_only_count_returns_empty_content_with_total_in_metadata() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        for index in 0..5 {
            std::fs::write(dir.path().join(format!("{index}.txt")), format!("{index}"))
                .expect("write");
        }

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            only_count: true,
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert_eq!(result.text(), "5");
        assert!(result.matched_paths.is_empty());
        assert_eq!(result.matches, path_counts(5, 0));
        assert!(!result.truncated);
        assert_eq!(
            result.metadata(),
            Some(json!({
                "files": 5,
                "directories": 0,
                "total": 5,
                "truncated": false,
            }))
        );
    });
}

#[test]
fn glob_needs_approval_auto_approves_inside_workspace() {
    let tool = GlobTool::default();
    let args = serde_json::json!({ "pattern": "src/**/*.rs" });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::AutoApproved
    ));
}

#[test]
fn glob_needs_approval_allows_dot_paths_inside_workspace() {
    let tool = GlobTool::default();
    let args = serde_json::json!({ "pattern": format!("{}/**/*", crate::GIT_ENTRY_NAME) });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::AutoApproved
    ));
}

#[cfg(unix)]
#[test]
fn glob_tool_symlink_parent_traversal_requires_user_approval() {
    let outside = TempDir::new().expect("outside");
    let workspace = TempDir::new().expect("workspace");
    let nested = outside.path().join("nested");
    std::fs::create_dir(&nested).expect("mkdir");
    std::os::unix::fs::symlink(&nested, workspace.path().join("link")).expect("symlink");
    let context = SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
        crate::sandbox::Sandbox::with_root(workspace.path().to_path_buf()),
    )));
    let tool = GlobTool::new(context);

    assert_eq!(
        tool.needs_approval(&serde_json::json!({ "pattern": format!("{}/link/../**/*", workspace.path().display()) })),
        ApprovalCheck::UserApprovalRequired,
        "`link/..` leaves the workspace and must not be judged lexically"
    );
}

#[test]
fn glob_needs_approval_outside_workspace_requires_user_approval() {
    let tool = GlobTool::default();
    let args = serde_json::json!({ "pattern": "/tmp/**" });
    assert!(matches!(
        tool.needs_approval(&args),
        ApprovalCheck::UserApprovalRequired
    ));
}

#[test]
fn glob_invalid_pattern_is_error() {
    crate::test_support::run_locked(async {
        let result = glob_files(&glob_args("[")).await;
        assert!(result.is_err());
    });
}

#[test]
fn validate_glob_pattern_rejects_negation_outside_brackets() {
    assert!(validate_glob_pattern("docs/**/!*.md").is_err());
    assert!(validate_glob_pattern("!*.rs").is_err());
    assert!(validate_glob_pattern("src/**/!foo").is_err());
    // Quoted-style artifact the LLM produced
    assert!(validate_glob_pattern("docs/**/!*\"").is_err());
}

#[test]
fn validate_glob_pattern_allows_negation_inside_brackets() {
    // [!abc] is valid glob syntax (character class negation)
    assert!(validate_glob_pattern("src/[!_]*.rs").is_ok());
    assert!(validate_glob_pattern("**/*.[!d]").is_ok());
}

#[test]
fn validate_glob_pattern_allows_normal_patterns() {
    assert!(validate_glob_pattern("**/*.rs").is_ok());
    assert!(validate_glob_pattern("docs/**/*.md").is_ok());
    assert!(validate_glob_pattern("src/**/mod.rs").is_ok());
    assert!(validate_glob_pattern("").is_ok());
}

#[test]
fn glob_files_returns_error_for_negation_pattern() {
    let args = GlobInputArgs {
        pattern: "docs/**/!*.md".to_string(),
        git_ignore: false,
        ..glob_args("")
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(glob_files(&args));
    assert!(result.is_err());
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("'!'"),
        "error should mention the offending character: {message}"
    );
    assert!(
        message.contains("not supported"),
        "error should say negation is not supported: {message}"
    );
}

#[test]
fn glob_files_logs_pattern_and_match_count() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "a").expect("seed file");
    std::fs::write(dir.path().join("b.txt"), "b").expect("seed file");
    let args = GlobInputArgs {
        pattern: format!("{}/*.txt", dir.path().display()),
        git_ignore: false,
        ..glob_args("")
    };
    let log = capture_logs(async move {
        let result = glob_files(&args).await;
        assert!(result.is_ok());
    });
    assert!(
        log.contains("glob searched"),
        "glob_files must log its result: {log}"
    );
    assert!(
        log.contains("matches=2"),
        "glob_files must log the match count: {log}"
    );
}

#[test]
fn glob_result_content_sections_empty_and_nonempty() {
    let empty_result = GlobResult::new(vec![], vec![], path_counts(0, 0), false);
    assert!(empty_result.content_sections().is_empty());

    let result = GlobResult::new(
        vec!["src/main.rs".to_string()],
        vec![GlobPathKind::File],
        path_counts(1, 0),
        false,
    );
    let sections = result.content_sections();
    assert_eq!(sections.len(), 1);
    assert!(matches!(sections[0], Section::Content(_)));
}

#[test]
fn runtime_token_budget_caps_glob_results_and_marks_them_truncated() {
    crate::test_support::run_locked(async {
        let directory = TempDir::new().expect("tempdir");
        for name in [
            "alpha-long-name.txt",
            "beta-long-name.txt",
            "gamma-long-name.txt",
        ] {
            std::fs::write(directory.path().join(name), "fixture").expect("write fixture");
        }
        let mut call = GlobTool::default().create_call(
            uuid::Uuid::new_v4(),
            json!({
                "pattern": format!("{}/*.txt", directory.path().display()),
                "git_ignore": false
            }),
        );
        call.set_runtime_output_token_limit(Some(3));

        let result = call.run().await.expect("glob result");

        assert!(crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&result.text()) <= 3);
        assert_eq!(result.metadata().expect("metadata")["truncated"], true);
    });
}

#[test]
fn glob_call_tool_call_methods() {
    let id = uuid::Uuid::new_v4();
    let args = serde_json::json!({ "pattern": "**/*.rs" });
    let call = GlobTool::default().create_call(id, args.clone());
    assert_eq!(call.id(), id);
    assert_eq!(call.name(), "glob");
    assert_eq!(call.arguments(), &args);
    assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
    assert_eq!(call.argument_order("pattern"), 0);
    let redacted = call.non_default_redacted_arguments();
    assert!(redacted.iter().any(|(key, _)| key == "pattern"));
}

#[test]
fn glob_default_includes_hidden_files() {
    // dot_ignore defaults to false — hidden files must appear in results.
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
        std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert!(
            result
                .matched_paths
                .iter()
                .any(|p| p.ends_with("visible.txt")),
            "visible file must be present"
        );
        assert!(
            result
                .matched_paths
                .iter()
                .any(|p| p.ends_with(".hidden.txt")),
            "hidden file must be present when dot_ignore=false"
        );
    });
}

#[test]
fn glob_dot_ignore_excludes_hidden_files() {
    // dot_ignore=true must exclude any path component starting with '.'.
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
        std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: false,
            dot_ignore: true,
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert!(
            result
                .matched_paths
                .iter()
                .any(|p| p.ends_with("visible.txt")),
            "visible file must be present"
        );
        assert!(
            !result
                .matched_paths
                .iter()
                .any(|p| p.ends_with(".hidden.txt")),
            "hidden file must be absent when dot_ignore=true"
        );
    });
}

#[test]
fn glob_dot_ignore_excludes_hidden_files_git_ignore_mode() {
    // dot_ignore=true must also exclude hidden files in the git_ignore=true path.
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("visible.txt"), "v").expect("write");
        std::fs::write(dir.path().join(".hidden.txt"), "h").expect("write");

        let result = glob_files(&GlobInputArgs {
            pattern: format!("{}/*.txt", dir.path().display()),
            git_ignore: true,
            dot_ignore: true,
            ..glob_args("")
        })
        .await
        .expect("glob ok");

        assert!(
            result
                .matched_paths
                .iter()
                .any(|p| p.ends_with("visible.txt")),
            "visible file must be present"
        );
        assert!(
            !result
                .matched_paths
                .iter()
                .any(|p| p.ends_with(".hidden.txt")),
            "hidden file must be absent when dot_ignore=true"
        );
    });
}

#[test]
fn glob_input_args_all_properties_have_descriptions() {
    let schema = GlobTool::default().input_schema();
    crate::test_support::assert_all_properties_have_descriptions(&schema, "GlobInputArgs");
}

#[test]
fn glob_description_defines_search_root_exclusion() {
    let tool = GlobTool::default();
    let description = tool.description();
    assert!(description.contains(
        "The search root itself is never a match and is always excluded from returned paths \
         and all counts"
    ));
    assert!(description.contains("Only descendants of the search root are eligible"));
}

#[test]
fn glob_descriptions_distinguish_git_ignore_from_dot_ignore() {
    let tool = GlobTool::default();
    let clarification = "does not exclude the `.git` directory or other dot-prefixed paths";
    assert!(tool.description().contains(clarification));

    let schema = serde_json::to_value(tool.input_schema()).expect("serialize glob schema");
    let git_ignore_description = schema["properties"]["git_ignore"]["description"]
        .as_str()
        .expect("git_ignore description");
    assert!(git_ignore_description.contains(clarification));
}

#[test]
fn glob_limit_descriptions_define_omission_as_unlimited() {
    let tool = GlobTool::default();
    let clarification = "Omit";
    assert!(tool.description().contains(clarification));
    assert!(tool.description().contains("no result-count limit"));
    assert!(tool.description().contains("arbitrary large value"));

    let schema = serde_json::to_value(tool.input_schema()).expect("serialize glob schema");
    let limit_description = schema["properties"]["limit"]["description"]
        .as_str()
        .expect("limit description");
    assert!(limit_description.contains(clarification));
    assert!(limit_description.contains("no result-count limit"));
    assert!(limit_description.contains("arbitrary large value"));
}
