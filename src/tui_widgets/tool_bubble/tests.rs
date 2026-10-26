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

#[test]
fn tool_bubble_abbreviates_only_declared_home_path_attributes() {
    use crate::tui_reducer_state::ResultSection;

    let home = dirs::home_dir().expect("home directory");
    let home_path = home.join("ylva-render-test/file.rs").display().to_string();
    let sibling_path = format!("{}-backup/file.rs", home.display());
    for (path, expected_path) in [
        (home.display().to_string(), "~".to_string()),
        (home_path.clone(), "~/ylva-render-test/file.rs".to_string()),
        (sibling_path.clone(), sibling_path),
        (
            "relative/file.rs".to_string(),
            "relative/file.rs".to_string(),
        ),
    ] {
        for primary in [true, false] {
            for declared in [true, false] {
                let expected = if declared { &expected_path } else { &path };
                let mut title_arguments = vec![("path".into(), serde_json::json!(path))];
                if !primary {
                    title_arguments.insert(0, ("pattern".into(), serde_json::json!("needle")));
                }
                title_arguments.push(("label".into(), serde_json::json!(home_path)));
                let state = ToolCallState::Done {
                    arguments: serde_json::json!({"path": path, "label": home_path}),
                    title_arguments,
                    content_sections: vec![],
                    result_section: ResultSection::plain(""),
                    summary: vec![
                        ("path".into(), path.clone()),
                        ("label".into(), home_path.clone()),
                    ],
                    duration: std::time::Duration::ZERO,
                };
                for collapsed in [true, false] {
                    let mut lines = vec![];
                    append_tool_bubble_lines(
                        &mut lines,
                        ToolBubbleArgs {
                            id: uuid::Uuid::new_v4(),
                            name: "read",
                            state: &state,
                            collapsed,
                            spinner_frame: 0,
                            elapsed_seconds: None,
                            display_title_override: None,
                            subtype_argument: None,
                            path_like_arguments: if declared { &["path"] } else { &[] },
                        },
                        &mut vec![],
                        1024,
                    );
                    let rendered = lines
                        .iter()
                        .flat_map(|line| &line.spans)
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    if collapsed {
                        assert!(!rendered.contains("path="), "{rendered}");
                        assert!(!rendered.contains("label="), "{rendered}");
                        let invocation = if primary {
                            format!("Read {expected} {home_path}")
                        } else {
                            format!("Read needle {home_path}")
                        };
                        assert!(rendered.contains(&invocation), "{rendered}");
                        continue;
                    }
                    assert!(rendered.contains(&format!("path={expected}")), "{rendered}");
                    assert!(
                        rendered.contains(&format!("label={home_path}")),
                        "{rendered}"
                    );
                    let argument_line = if primary {
                        expected.to_string()
                    } else {
                        format!("path={expected}")
                    };
                    assert!(
                        lines.iter().any(|line| {
                            let text = line
                                .spans
                                .iter()
                                .map(|span| span.content.as_ref())
                                .collect::<String>();
                            text.trim_end().ends_with(&argument_line)
                        }),
                        "{rendered}"
                    );
                }
            }
        }
    }
}

#[test]
fn skill_source_uses_home_label_without_rewriting_instructions() {
    use crate::agent::tools::{ContentSection, Section};
    use crate::tui_reducer_state::ResultSection;

    let source = dirs::home_dir()
        .expect("home directory")
        .join(".agents/skills/example/SKILL.md")
        .display()
        .to_string();
    let instructions = format!("Keep this absolute reference: {source}");
    let state = ToolCallState::Done {
        arguments: serde_json::json!({"name": "example"}),
        title_arguments: vec![("name".into(), serde_json::json!("example"))],
        result_section: ResultSection::plain(instructions.clone()),
        summary: vec![],
        duration: std::time::Duration::ZERO,
        content_sections: vec![
            Section::Content(ContentSection {
                label: Some("Source".into()),
                content: source,
                file_extension: None,
                line_number_offset: None,
            }),
            Section::Content(ContentSection {
                label: Some("Skill instructions".into()),
                content: instructions.clone(),
                file_extension: Some("md".into()),
                line_number_offset: None,
            }),
        ],
    };
    let mut lines = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "load_skill",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut vec![],
        1024,
    );
    let rendered = lines
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("~/.agents/skills/example/SKILL.md"));
    assert!(rendered.contains(&instructions), "{rendered}");
}

#[test]
fn load_skill_bubble_keeps_name_primary_and_source_in_expanded_body() {
    use crate::agent::tools::{load_skill::LoadSkillTool, Tool};
    use crate::skills::policy::{loader_context, CallableSkills};
    use std::sync::{Arc, RwLock};

    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let source_directory = directory.path().join(".ylva/skills/unslop");
        std::fs::create_dir_all(&source_directory).unwrap();
        let source = source_directory.join("SKILL.md");
        std::fs::write(
            &source,
            "---\nname: unslop\ndescription: Improve prose\n---\nSkill body",
        )
        .unwrap();
        let snapshot =
            crate::skills::SkillCatalog::discover(None, directory.path(), None, None).unwrap();
        let profile = crate::ProfileConfig::default();
        let sandbox = Arc::new(RwLock::new(crate::Sandbox::with_root(
            directory.path().to_owned(),
        )));
        let callable = CallableSkills::resolve(
            &snapshot,
            &profile,
            "default",
            &[LoadSkillTool::NAME.into()],
            false,
            sandbox.clone(),
        )
        .unwrap();
        let tool = LoadSkillTool::new(
            Arc::new(callable),
            loader_context(sandbox, &profile, "default").expect("valid sandbox fixture"),
        );
        let arguments = serde_json::json!({"name": "unslop"});
        let result = tool
            .create_call(uuid::Uuid::new_v4(), arguments.clone())
            .run()
            .await
            .unwrap();
        let canonical_source = source
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(result.metadata().unwrap()["name"], "unslop");
        assert!(result
            .metadata_schema()
            .unwrap()
            .schema
            .object
            .unwrap()
            .properties
            .contains_key("name"));
        let legacy_sections = result
            .content_sections()
            .into_iter()
            .filter(|section| {
                !matches!(section, crate::agent::tools::Section::Content(content)
                    if content.label.as_deref() == Some("Source"))
            })
            .collect::<Vec<_>>();
        let legacy_summary = vec![
            ("name".to_string(), "unslop".to_string()),
            ("source".to_string(), canonical_source.clone()),
        ];
        for (summary, content_sections, metadata) in [
            (
                result.display_summary(),
                result.content_sections(),
                result.metadata(),
            ),
            (
                legacy_summary.clone(),
                legacy_sections.clone(),
                result.metadata(),
            ),
            (legacy_summary, legacy_sections, None),
        ] {
            let call_id = uuid::Uuid::new_v4();
            let messages = vec![
                crate::agent::Message::Assistant {
                    content: String::new(),
                    status: crate::agent::AssistantStatus::Complete,
                    tool_calls: vec![crate::agent::ToolCallRequest {
                        id: call_id,
                        name: LoadSkillTool::NAME.into(),
                        arguments: arguments.clone(),
                        persisted_title_arguments: tool.non_default_redacted_arguments(&arguments),
                    }],
                },
                crate::agent::Message::ToolResult {
                    value: crate::agent::ToolResult {
                        id: call_id,
                        name: LoadSkillTool::NAME.into(),
                        result: result.text(),
                        attachments: vec![],
                        result_section_id: None,
                        result_metadata: metadata,
                        result_metadata_schema: None,
                        summary,
                        content_sections,
                        duration: std::time::Duration::ZERO,
                    },
                },
            ];
            let persisted = serde_json::to_value(&messages).unwrap();
            let entries = crate::tui_reducer_state::project_messages(&messages);
            assert_eq!(serde_json::to_value(&messages).unwrap(), persisted);
            let crate::tui_reducer_state::ChatEntry::ToolBubble { state, .. } = &entries[0] else {
                panic!("expected replayed skill bubble");
            };
            for collapsed in [true, false] {
                let mut lines = vec![];
                append_tool_bubble_lines(
                    &mut lines,
                    ToolBubbleArgs {
                        id: uuid::Uuid::new_v4(),
                        name: LoadSkillTool::NAME,
                        state,
                        collapsed,
                        spinner_frame: 0,
                        elapsed_seconds: None,
                        display_title_override: None,
                        subtype_argument: None,
                        path_like_arguments: &[],
                    },
                    &mut vec![],
                    512,
                );
                let title = lines[0]
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                let expected_title = if collapsed {
                    "▶ Load Skill unslop"
                } else {
                    "▼ Load Skill"
                };
                assert!(title.contains(expected_title), "{title}");
                assert!(title.ends_with("0.000s"), "{title}");
                assert_eq!(title.chars().count(), 1 + 2 + 512, "{title}");
                assert!(!title.contains(&canonical_source), "{title}");
                assert!(!title.contains("name="), "{title}");
                let body = lines[1..]
                    .iter()
                    .flat_map(|line| &line.spans)
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                if collapsed {
                    assert!(body.is_empty(), "{body}");
                } else {
                    assert_eq!(body.matches("Source:").count(), 1, "{body}");
                    assert!(body.contains(&canonical_source), "{body}");
                    assert!(body.contains("Skill instructions:"), "{body}");
                    assert!(body.contains("Skill body"), "{body}");
                }
            }
        }
    });
}

#[test]
fn split_tool_layout_preserves_chrome_body_and_hit_ranges() {
    use crate::agent::tools::{ContentSection, Section};
    use crate::tui_reducer_state::ResultSection;

    let content_sections = vec![Section::Content(ContentSection {
        label: Some("Command".to_string()),
        content: "first command\nsecond command".to_string(),
        file_extension: None,
        line_number_offset: None,
    })];
    let title_arguments = vec![
        (
            "command".to_string(),
            serde_json::json!("first command\nsecond command"),
        ),
        (
            "label".to_string(),
            serde_json::json!("first label\nsecond label"),
        ),
    ];
    let states = [
        ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
        },
        ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            progress_arguments: vec![],
        },
        ToolCallState::Done {
            arguments: serde_json::json!({}),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            result_section: ResultSection::plain("plain output"),
            summary: vec![],
            duration: std::time::Duration::from_millis(50),
        },
        ToolCallState::Error {
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            error: "tool execution failed".to_string(),
            duration: std::time::Duration::from_millis(50),
        },
        ToolCallState::Denied {
            arguments: serde_json::json!({}),
            content_sections,
            title_arguments,
            error: "approval denied".to_string(),
        },
        ToolCallState::Killed,
    ];
    for state in &states {
        for collapsed in [true, false] {
            for width in [20, 80] {
                let bubble = ToolBubbleArgs {
                    id: uuid::Uuid::new_v4(),
                    name: "bash",
                    state,
                    collapsed,
                    spinner_frame: 0,
                    elapsed_seconds: None,
                    display_title_override: None,
                    subtype_argument: None,
                    path_like_arguments: &[],
                };
                let mut complete = vec![Line::from("preceding entry")];
                let mut hit_ranges = vec![];
                let body_start =
                    append_tool_layout(&mut complete, bubble, &mut hit_ranges, width, false);
                assert_eq!(hit_ranges, vec![(1, complete.len() - 1, bubble.id)]);

                let mut chrome = vec![Line::from("preceding entry")];
                let mut chrome_hit_ranges = vec![];
                let chrome_end =
                    append_tool_layout(&mut chrome, bubble, &mut chrome_hit_ranges, width, true);
                assert_eq!(body_start, chrome_end);
                assert_eq!(&complete[..body_start], &chrome);
                assert!(chrome_hit_ranges.is_empty());
                if matches!(
                    state,
                    ToolCallState::Error { .. } | ToolCallState::Denied { .. }
                ) {
                    let diagnostic = if matches!(state, ToolCallState::Error { .. }) {
                        "tool execution failed"
                    } else {
                        "approval denied"
                    };
                    let chrome_text = chrome
                        .iter()
                        .flat_map(|line| &line.spans)
                        .filter(|span| span.style.fg == Some(crate::theme::error_color()))
                        .map(|span| span.content.trim())
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert!(chrome_text.contains(diagnostic));
                }

                let style = BubbleStyle::new(tool_bubble_border_color());
                let mut parts = vec![Line::from("preceding entry")];
                let extra_sections = title::append_tool_chrome(&mut parts, bubble, &style, width);
                body::append_tool_body(&mut parts, bubble, &style, &extra_sections, width);
                assert_eq!(complete, parts);
                if collapsed || matches!(state, ToolCallState::Killed) {
                    assert_eq!(complete.len(), body_start);
                } else {
                    let body_text = complete[body_start..]
                        .iter()
                        .flat_map(|line| &line.spans)
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    assert_eq!(body_text.matches("first command").count(), 1);
                    assert_eq!(body_text.matches("first label").count(), 1);
                    let merged = bubble.content_sections(&extra_sections);
                    let labels = merged
                        .iter()
                        .map(|section| match section {
                            Section::Content(content) => content.label.as_deref().unwrap(),
                            _ => panic!("expected content sections"),
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(labels, ["Command", "label"]);
                }
            }
        }
    }
}

#[test]
fn tool_bubble_renders_markdown_content_section_as_prose_not_code() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::agent::tools::{ContentSection, Section};
    use crate::tui_reducer_state::{ResultSection, ToolCallState};

    let state = ToolCallState::Done {
        arguments: serde_json::json!({"task": "# Investigate the bug"}),
        result_section: ResultSection::plain("ok"),
        content_sections: vec![Section::Content(ContentSection {
            label: Some("Task".to_string()),
            content: "# Investigate the bug\n\n- check logs\n- reproduce it".to_string(),
            file_extension: Some("md".to_string()),
            line_number_offset: None,
        })],
        summary: vec![],
        duration: std::time::Duration::ZERO,
        title_arguments: vec![],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();

    // Markdown list rendering turns `- item` into a bullet-prefixed line
    // (`• item`); the syntect code-highlighting path would never do
    // this — it passes raw text through verbatim with a line-number-style
    // gutter instead. Bullet rendering is therefore proof the markdown
    // path (not the code path) handled this section.
    assert!(
        rendered.contains("Investigate the bug"),
        "expected heading text in rendered bubble, got: {rendered:?}"
    );
    assert!(
        rendered.contains('•'),
        "expected markdown bullet rendering, got: {rendered:?}"
    );
    assert!(
        rendered.contains("check logs") && rendered.contains("reproduce it"),
        "expected bullet items in rendered bubble, got: {rendered:?}"
    );
}

#[test]
fn tool_bubble_renders_execution_error_content_literally() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::agent::tools::{ContentSection, Section};
    use crate::tui_reducer_state::{ResultSection, ToolCallState};

    let state = ToolCallState::Done {
        arguments: serde_json::json!({}),
        result_section: ResultSection::plain(""),
        content_sections: vec![Section::Content(ContentSection {
            label: Some("Error".to_string()),
            content: "unknown engine: local-discovery_".to_string(),
            file_extension: None,
            line_number_offset: None,
        })],
        summary: vec![],
        duration: std::time::Duration::ZERO,
        title_arguments: vec![],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains("unknown engine: local-discovery_"),
        "execution diagnostic should remain literal, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("local-discovery _"),
        "execution diagnostic should not be reformatted as Markdown, got: {rendered:?}"
    );
}

#[test]
fn failed_subagent_status_is_red_in_root_and_header_bubble_titles() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::theme::error_color;
    use crate::tui_reducer_state::{ResultSection, ToolCallState};
    use ratatui::style::Modifier;

    let state = ToolCallState::Done {
        arguments: serde_json::json!({"label": "Inspect change"}),
        result_section: ResultSection::plain(""),
        content_sections: vec![],
        summary: vec![("status".to_string(), "failure".to_string())],
        duration: std::time::Duration::from_millis(123),
        title_arguments: vec![("label".to_string(), serde_json::json!("Inspect change"))],
    };

    for display_title_override in [None, Some("Subagent")] {
        for collapsed in [true, false] {
            let mut lines: Vec<Line<'static>> = vec![];
            let mut bubble_title_rows = vec![];
            append_tool_bubble_lines(
                &mut lines,
                ToolBubbleArgs {
                    id: uuid::Uuid::new_v4(),
                    name: "spawn_subagent",
                    state: &state,
                    collapsed,
                    spinner_frame: 0,
                    elapsed_seconds: None,
                    display_title_override,
                    subtype_argument: None,
                    path_like_arguments: &[],
                },
                &mut bubble_title_rows,
                80,
            );

            let failure_status_span = lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .find(|span| span.content.as_ref() == "status=failure");
            if collapsed {
                assert!(failure_status_span.is_none(), "status belongs to the body");
                continue;
            }
            let failure_status_span =
                failure_status_span.expect("failed subagent status should be rendered");
            assert_eq!(failure_status_span.style.fg, Some(error_color()));
            assert!(
                failure_status_span
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD),
                "failed subagent status should be bold"
            );
        }
    }
}

#[test]
fn queued_subagent_bubble_shows_static_glyph_not_spinner() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::tui_reducer_state::ToolCallState;

    let state = ToolCallState::Running {
        arguments: serde_json::json!({"task": "investigate"}),
        content_sections: vec![],
        title_arguments: vec![],
        progress_arguments: vec![("status".to_string(), serde_json::json!("queued"))],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: false,
            spinner_frame: 3,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains('≡'),
        "expected static queued glyph, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("status=queued"),
        "queued state should be communicated by the glyph, not the title: {rendered:?}"
    );
    for spinner_char in crate::tui_widgets::SPINNER_FRAMES {
        assert!(
            !rendered.contains(*spinner_char),
            "queued bubble must not show the animated spinner, got: {rendered:?}"
        );
    }
}

#[test]
fn running_subagent_bubble_shows_spinner_not_queued_glyph() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::tui_reducer_state::ToolCallState;

    let state = ToolCallState::Running {
        arguments: serde_json::json!({"task": "investigate"}),
        content_sections: vec![],
        title_arguments: vec![],
        progress_arguments: vec![("status".to_string(), serde_json::json!("running"))],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains(crate::tui_widgets::SPINNER_FRAMES[0]),
        "expected animated spinner glyph, got: {rendered:?}"
    );
    assert!(
        !rendered.contains('≡'),
        "running bubble must not show the static queued glyph, got: {rendered:?}"
    );
}

#[test]
fn running_bash_bubble_shows_elapsed_seconds() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::tui_reducer_state::ToolCallState;

    let state = ToolCallState::Running {
        arguments: serde_json::json!({"command": "sleep 10"}),
        content_sections: vec![],
        title_arguments: vec![("command".to_string(), serde_json::json!("sleep 10"))],
        progress_arguments: vec![],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "bash",
            state: &state,
            collapsed: true,
            spinner_frame: 0,
            elapsed_seconds: Some(7),
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(rendered.contains("7s"), "got: {rendered:?}");
}

#[test]
fn subagent_bubble_hides_run_id_from_title() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::tui_reducer_state::ToolCallState;

    let run_id = uuid::Uuid::new_v4();
    let state = ToolCallState::Running {
        arguments: serde_json::json!({"label": "Inspect change"}),
        content_sections: vec![],
        title_arguments: vec![("label".to_string(), serde_json::json!("Inspect change"))],
        progress_arguments: vec![
            ("status".to_string(), serde_json::json!("running")),
            ("run_id".to_string(), serde_json::json!(run_id.to_string())),
        ],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: true,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains("Inspect change"),
        "the subagent label should remain visible: {rendered:?}"
    );
    assert!(
        !rendered.contains("status=running"),
        "the live subagent status should be communicated by the indicator, not the title: {rendered:?}"
    );
    assert!(
        !rendered.contains("run_id") && !rendered.contains(&run_id.to_string()),
        "the internal run identifier must not appear in the title: {rendered:?}"
    );
}

/// The subagent-view header shortens the `spawn_subagent` bubble's title to
/// `"Subagent"` via `ToolBubbleArgs::display_title_override`, dropping
/// the "Spawn" prefix that's only meaningful from the root
/// conversation's point of view (regression test for the header always
/// reading "Spawn Subagent ..." even though it sits directly above the
/// subagent's own transcript).
#[test]
fn display_title_override_replaces_title_cased_tool_name() {
    use super::{append_tool_bubble_lines, ToolBubbleArgs};
    use crate::tui_reducer_state::ToolCallState;

    let state = ToolCallState::Running {
        arguments: serde_json::json!({"task": "investigate"}),
        content_sections: vec![],
        title_arguments: vec![],
        progress_arguments: vec![],
    };
    let mut lines: Vec<Line<'static>> = vec![];
    let mut bubble_title_rows = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: Some("Subagent"),
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut bubble_title_rows,
        80,
    );

    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains("Subagent"),
        "expected the overridden title, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("Spawn Subagent"),
        "the plain title-cased tool name must not also appear, got: {rendered:?}"
    );
}

#[test]
fn collapsed_title_shows_primary_and_label_with_right_aligned_time() {
    use crate::tui_reducer_state::ResultSection;

    let state = ToolCallState::Done {
        arguments: serde_json::json!({}),
        title_arguments: vec![
            ("command".into(), serde_json::json!("make test")),
            ("label".into(), serde_json::json!("run tests")),
            ("timeout".into(), serde_json::json!(30)),
        ],
        content_sections: vec![],
        result_section: ResultSection::plain(""),
        summary: vec![("exit".into(), "0".into())],
        duration: std::time::Duration::from_millis(1500),
    };
    let render = |collapsed: bool, width: usize| -> Vec<String> {
        let mut lines = vec![];
        append_tool_bubble_lines(
            &mut lines,
            ToolBubbleArgs {
                id: uuid::Uuid::new_v4(),
                name: "bash",
                state: &state,
                collapsed,
                spinner_frame: 0,
                elapsed_seconds: None,
                display_title_override: None,
                subtype_argument: None,
                path_like_arguments: &[],
            },
            &mut vec![],
            width,
        );
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    };

    let collapsed = render(true, 40);
    assert_eq!(collapsed.len(), 1);
    assert_eq!(
        collapsed[0],
        format!(
            "▌  ▶ Bash make test run tests{}1.500s",
            " ".repeat(40 - 26 - 6)
        )
    );

    let truncated = render(true, 24);
    assert!(
        truncated[0].ends_with("…  1.500s") || truncated[0].ends_with("… 1.500s"),
        "{}",
        truncated[0]
    );
    assert_eq!(truncated[0].chars().count(), 3 + 24);

    let expanded = render(false, 40);
    assert!(expanded[0].starts_with("▌  ▼ Bash") && expanded[0].ends_with("1.500s"));
    assert!(expanded.iter().any(|line| line.contains("timeout=30")));
    assert!(expanded.iter().any(|line| line.contains("exit=0")));
}

#[test]
fn expanded_body_lists_result_pairs_first_and_output_last() {
    use crate::agent::tools::{ContentSection, Section};
    use crate::tui_reducer_state::ResultSection;

    let section = |label: &str, content: &str| {
        Section::Content(ContentSection {
            label: Some(label.into()),
            content: content.into(),
            file_extension: None,
            line_number_offset: None,
        })
    };
    let state = ToolCallState::Done {
        arguments: serde_json::json!({}),
        title_arguments: vec![],
        content_sections: vec![
            section("Output", "printed-text"),
            section("Label", "Greet"),
            section("Command", "echo hello"),
        ],
        result_section: ResultSection::plain(""),
        summary: vec![("output_bytes".into(), "6".into())],
        duration: std::time::Duration::from_millis(38),
    };
    let mut lines = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "bash",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut vec![],
        60,
    );
    let rendered: String = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect();
    let position = |needle: &str| rendered.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(position("Result:") < position("output_bytes=6"));
    assert!(position("output_bytes=6") < position("Label:"));
    assert!(position("Label:") < position("Command:"));
    assert!(position("Command:") < position("Output:"));
    assert!(position("Output:") < position("printed-text"));
}

#[test]
fn expanded_arguments_render_under_arguments_section() {
    use crate::tui_reducer_state::ResultSection;

    let state = ToolCallState::Done {
        arguments: serde_json::json!({}),
        title_arguments: vec![
            ("path".into(), serde_json::json!("docs/configuration.md")),
            ("mode".into(), serde_json::json!("lines")),
            ("offset".into(), serde_json::json!(926)),
        ],
        result_section: ResultSection::plain(String::new()),
        summary: vec![],
        duration: std::time::Duration::ZERO,
        content_sections: vec![],
    };
    let mut lines = vec![];
    append_tool_bubble_lines(
        &mut lines,
        ToolBubbleArgs {
            id: uuid::Uuid::new_v4(),
            name: "read",
            state: &state,
            collapsed: false,
            spinner_frame: 0,
            elapsed_seconds: None,
            display_title_override: None,
            subtype_argument: None,
            path_like_arguments: &[],
        },
        &mut vec![],
        80,
    );
    let rows: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let tail: Vec<&str> = rows
        .iter()
        .map(|row| row.trim_start_matches(|c| c != ' ').trim())
        .collect();
    assert!(tail[1].ends_with("docs/configuration.md"), "{rows:?}");
    assert_eq!(tail[2], "", "{rows:?}");
    assert!(rows[3].ends_with("Arguments:"), "{rows:?}");
    assert!(rows[4].ends_with("mode=lines"), "{rows:?}");
    assert!(rows[5].ends_with("offset=926"), "{rows:?}");
}
