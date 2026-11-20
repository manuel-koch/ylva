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

mod common;

use common::sha256_hex;
use std::process::{Command, Stdio};
use time::format_description::well_known::Rfc3339;
use ylva::{project_key, session_path, Store};

#[tokio::test]
async fn resumed_old_sessions_sort_by_last_use_and_are_not_selected_by_before() {
    let directory = tempfile::tempdir().unwrap();
    write_mock_profile(directory.path());
    let chats_root = directory.path().join("chats");
    let resumed_identity = uuid::Uuid::from_u128(10);
    let inactive_identity = uuid::Uuid::from_u128(11);
    for (identity, created_at, title) in [
        (resumed_identity, "2020-01-01T00:00:00Z", "resumed"),
        (inactive_identity, "2021-01-01T00:00:00Z", "inactive"),
    ] {
        let store = Store::create(
            &chats_root,
            directory.path(),
            identity,
            "mock",
            "mock",
            None,
        )
        .unwrap();
        store.flush().await;
        let path = store.path().to_path_buf();
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = created_at.into();
        metadata.title = title.into();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap()
            ),
        )
        .unwrap();
    }
    let output = command(directory.path())
        .args(["session", "compress", &resumed_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let resumed_at = time::OffsetDateTime::now_utc();
    let output = command(directory.path())
        .args([
            "--no-tui",
            "--conversation-id",
            &resumed_identity.to_string(),
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let output = command(directory.path())
        .args(["session", "compress", &resumed_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let resumed_path = session_path(
        &chats_root,
        &project_key(directory.path()),
        resumed_identity,
    );
    let archive_path = resumed_path.with_extension("jsonl.gz");
    let archive_bytes = std::fs::read(&archive_path).unwrap();
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let listing = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = listing.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains(&inactive_identity.to_string()));
    assert!(lines[1].contains(&resumed_identity.to_string()));
    let last_used_at =
        time::OffsetDateTime::parse(lines[1].split_whitespace().next().unwrap(), &Rfc3339).unwrap();
    assert!(
        last_used_at.unix_timestamp_nanos().div_euclid(1_000_000)
            >= resumed_at.unix_timestamp_nanos().div_euclid(1_000_000)
    );
    assert_eq!(
        Store::read_session_meta(&resumed_path)
            .unwrap()
            .unwrap()
            .created_at,
        "2020-01-01T00:00:00Z"
    );
    let before = (resumed_at - time::Duration::hours(1))
        .format(&Rfc3339)
        .unwrap();
    for operation in ["compress", "decompress", "delete"] {
        let preview = command(directory.path())
            .args(["session", operation, "--before", &before, "--dry-run"])
            .output()
            .unwrap();
        assert!(preview.status.success(), "{:?}", preview);
        let preview = String::from_utf8(preview.stdout).unwrap();
        assert!(preview.contains(&inactive_identity.to_string()));
        assert!(!preview.contains(&resumed_identity.to_string()));
        let output = command(directory.path())
            .args(["session", operation, "--before", &before])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains(&inactive_identity.to_string()));
        assert!(!output.contains(&resumed_identity.to_string()));
        assert_eq!(std::fs::read(&archive_path).unwrap(), archive_bytes);
    }
    assert!(!session_path(
        &chats_root,
        &project_key(directory.path()),
        inactive_identity
    )
    .exists());
}

#[tokio::test]
async fn leap_second_cutoffs_do_not_delete_sessions_at_the_same_clamped_instant() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let store = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    store.flush().await;
    let path = store.path().to_path_buf();
    let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
    metadata.created_at = "2016-12-31T23:59:60.5Z".into();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let original = format!(
        "{}\n",
        serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap()
    );
    std::fs::write(&path, &original).unwrap();

    let output = command(directory.path())
        .args([
            "session",
            "delete",
            "--before",
            "2016-12-31T23:59:60.9Z",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[tokio::test]
async fn listing_groups_archived_and_nested_subagents_with_their_parents() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    for (identity, parent, day, title) in [
        (1, None, 1, "root one"),
        (2, None, 2, "root two"),
        (3, Some(1), 4, "subagent\none"),
        (4, Some(3), 5, "nested"),
        (5, Some(2), 3, "subagent two"),
        (6, Some(1), 4, "sibling"),
        (7, Some(99), 6, "orphan"),
    ] {
        let store = Store::create(
            &chats_root,
            directory.path(),
            uuid::Uuid::from_u128(identity),
            "mock",
            "mock",
            None,
        )
        .unwrap();
        store.flush().await;
        let path = store.path().to_path_buf();
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = "2020-01-01T00:00:00Z".into();
        metadata.title = title.into();
        metadata.parent_session = parent.map(uuid::Uuid::from_u128);
        let activity = ylva::StoreEvent::ModelChanged {
            version: metadata.version,
            changed_at: format!("2026-01-{day:02}T00:00:00Z"),
            provider_connection: "mock".into(),
            model: "mock".into(),
            parameters: Default::default(),
        };
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap(),
                serde_json::to_string(&activity).unwrap()
            ),
        )
        .unwrap();
    }
    let subagent_identity = uuid::Uuid::from_u128(3);
    let output = command(directory.path())
        .args(["session", "compress", &subagent_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "2026-01-01T00:00:00Z {} root one\n2026-01-02T00:00:00Z {} root two\n",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2)
        )
    );
    let output = command(directory.path())
        .args(["session", "list", "--include-subagents"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let expected: String = [
        (1, 1, "", "root one"),
        (3, 4, "+ ", "subagent one"),
        (4, 5, "  + ", "nested"),
        (6, 4, "+ ", "sibling"),
        (2, 2, "", "root two"),
        (5, 3, "+ ", "subagent two"),
    ]
    .iter()
    .map(|(identity, day, marker, title)| {
        format!(
            "2026-01-{day:02}T00:00:00Z {marker}{} {title}\n",
            uuid::Uuid::from_u128(*identity)
        )
    })
    .collect();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{expected}2026-01-06T00:00:00Z + {} orphan [parent unavailable: {}]\n",
            uuid::Uuid::from_u128(7),
            uuid::Uuid::from_u128(99)
        )
    );
    assert!(output.stderr.is_empty());
    let subagent_path = session_path(
        &chats_root,
        &project_key(directory.path()),
        subagent_identity,
    );
    assert!(!subagent_path.exists());
    assert!(subagent_path.with_extension("jsonl.gz").exists());
}

/// Conversations need a configured provider; give the default profile an
/// explicit mock engine.
fn write_mock_profile(directory: &std::path::Path) {
    let profile_directory = directory.join("profiles").join("default");
    std::fs::create_dir_all(&profile_directory).unwrap();
    std::fs::write(
        profile_directory.join("config.toml"),
        "[engines.default]\nprovider_connection = \"mock\"\nmodel = \"test-model\"\n",
    )
    .unwrap();
}

fn command(directory: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("YLVA_HOME", directory)
        .current_dir(directory);
    command
}

fn write_export_source(
    directory: &std::path::Path,
    identity: uuid::Uuid,
    bytes: &[u8],
) -> std::path::PathBuf {
    let path = session_path(&directory.join("chats"), &project_key(directory), identity);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn export_help_lists_output_path_placeholders() {
    let directory = tempfile::tempdir().unwrap();
    for flag in ["-h", "--help"] {
        let output = command(directory.path())
            .args(["session", "export", flag])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        for placeholder in [
            "{project_key}",
            "{session_id}",
            "{session_title}",
            "{format}",
        ] {
            let bullet = format!("- {placeholder}:");
            assert!(
                help.lines()
                    .any(|line| line.trim_start().starts_with(&bullet)),
                "{flag} should list {placeholder} on its own line"
            );
        }
    }
}

#[test]
fn export_help_explains_default_redaction() {
    let directory = tempfile::tempdir().unwrap();
    for flag in ["-h", "--help"] {
        let output = command(directory.path())
            .args(["session", "export", flag])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("best-effort secret redaction"));
        assert!(help.contains("by default"));
    }
}

#[test]
fn export_help_explains_manifest_format_scope_and_literal_path() {
    let directory = tempfile::tempdir().unwrap();
    for flag in ["-h", "--help"] {
        let output = command(directory.path())
            .args(["session", "export", flag])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("one metadata-only JSON manifest per invocation"));
        assert!(help.contains("always JSON, regardless of --format"));
        assert!(help.contains("no template placeholders"));
    }
}

#[test]
fn raw_export_ignores_profiles_and_open_stdin_and_copies_exact_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"unknown\"}\r\nnot valid JSON\npartial trailing data";
    let path = write_export_source(directory.path(), identity, bytes);
    let mut child = command(directory.path())
        .args([
            "--profile",
            "missing",
            "session",
            "export",
            "--no-subagents",
            "-f",
            "jsonl",
            "--raw",
            &identity.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, bytes);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("raw"));
    assert!(
        diagnostics.contains("warning: exports of active sessions or subagents may be partial"),
        "{diagnostics}"
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn raw_export_copies_archived_bytes_to_a_file_without_restoring_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let mut meta = serde_json::json!({
        "type": "meta",
        "version": 1, "created_at": "2021-01-01T00:00:00Z", "schema_version": 2,
        "app_version": "test", "project_key": project_key(directory.path()),
        "provider_connection": "test", "model": "test", "session_id": identity,
        "title": "test", "future_header": {"unsorted": [true, null], "earlier": {"value": 7}}
    });
    let prefix = "malformed complete event\r\n{\"type\":\"unknown\"}";
    let original = format!("{meta}\r\n{prefix}");
    let path = write_export_source(directory.path(), identity, original.as_bytes());
    let stdout_snapshot = |force: bool| {
        let mut export = command(directory.path());
        export.args([
            "session",
            "export",
            "--no-subagents",
            "-f",
            "jsonl",
            "--raw",
            "--manifest",
            "snapshot.json",
        ]);
        if force {
            export.arg("--force");
        }
        let result = export.arg(identity.to_string()).output().unwrap();
        assert!(result.status.success(), "{result:?}");
        (
            result.stdout,
            read_export_manifest(directory.path(), "snapshot.json"),
        )
    };
    let (stdout, plain) = stdout_snapshot(false);
    assert_eq!(stdout, original.as_bytes());
    meta["title"] = "Longer renamed title".into();
    let bytes = format!("{meta}\r\n{prefix}");
    std::fs::write(&path, &bytes).unwrap();
    let (stdout, renamed) = stdout_snapshot(true);
    assert_eq!(stdout, bytes.as_bytes());
    let archived = command(directory.path())
        .args(["session", "compress", &identity.to_string()])
        .output()
        .unwrap();
    assert!(archived.status.success(), "{archived:?}");
    let arguments = [
        "session",
        "export",
        "--no-subagents",
        "-f",
        "jsonl",
        "--raw",
        "--manifest",
        "archive-report.json",
        "-o",
        "destination.jsonl",
    ];
    let export = || {
        command(directory.path())
            .args(arguments)
            .arg(identity.to_string())
            .output()
            .unwrap()
    };
    let output = export();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(directory.path().join("destination.jsonl")).unwrap(),
        bytes.as_bytes()
    );
    assert!(!path.exists());
    assert!(path.with_extension("jsonl.gz").is_file());
    let archived = read_export_manifest(directory.path(), "archive-report.json");
    let plain = &plain["sessions"][0];
    let renamed = &renamed["sessions"][0];
    let archive_source = &archived["sessions"][0];
    assert_eq!(plain["representation"], "plain");
    assert_eq!(archive_source["representation"], "gzip");
    assert!(archive_source["source_paths"]["plain"].is_null());
    assert!(archive_source["source_paths"]["gzip"]
        .as_str()
        .unwrap()
        .ends_with(&format!("{identity}.jsonl.gz")));
    assert!(archive_source["source_paths"]["sidecar"]
        .as_str()
        .unwrap()
        .ends_with(&format!("{identity}.meta.json")));
    let before = &plain["boundary"];
    let after = &renamed["boundary"];
    assert!(!before["header_sha256_without_title"].is_null());
    assert_eq!(
        before["header_sha256_without_title"],
        after["header_sha256_without_title"]
    );
    assert_ne!(before["logical_sha256"], after["logical_sha256"]);
    assert_ne!(before["logical_bytes"], after["logical_bytes"]);
    assert_eq!(before["event_prefix_bytes"], prefix.len());
    assert_eq!(before["event_prefix_sha256"], sha256_hex(prefix.as_bytes()));
    assert_eq!(before["event_prefix_sha256"], after["event_prefix_sha256"]);
    assert_eq!(after, &archive_source["boundary"]);
    assert_eq!(
        archived["outputs"][0]["output_sha256"],
        sha256_hex(bytes.as_bytes())
    );
    assert_eq!(export().status.code(), Some(2));
}

fn json_transcript(events: &[serde_json::Value]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|event| format!("{event}\n").into_bytes())
        .collect()
}

#[test]
fn markdown_export_matches_the_documented_parent_and_subagent_examples() {
    let directory = tempfile::tempdir().unwrap();
    let key = project_key(directory.path());
    let mut fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/session-export/transcripts.json")).unwrap();
    let session = fixture["session"].as_array_mut().unwrap();
    let root = uuid::Uuid::parse_str(session[0]["session_id"].as_str().unwrap()).unwrap();
    session[0]["project_key"] = key.clone().into();
    write_export_source(directory.path(), root, &json_transcript(session));
    for events in fixture["subagents"].as_array_mut().unwrap() {
        let events = events.as_array_mut().unwrap();
        let identity = uuid::Uuid::parse_str(events[0]["session_id"].as_str().unwrap()).unwrap();
        events[0]["project_key"] = key.clone().into();
        write_export_source(directory.path(), identity, &json_transcript(events));
    }

    let scanner = scanner_directory(directory.path(), "/bin/cat >/dev/null");
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-o", "bundle"])
        .arg(root.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let bundle = directory.path().join("bundle");
    assert_eq!(
        std::fs::read_to_string(bundle.join("session.md")).unwrap(),
        include_str!("fixtures/session-export/session.md.txt")
    );
    assert_eq!(
        std::fs::read_to_string(bundle.join("subagent-1.md")).unwrap(),
        include_str!("fixtures/session-export/subagent-1.md.txt")
    );
}

#[test]
fn raw_markdown_closes_unfinished_blocks_and_escapes_terminal_controls() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        &json_transcript(&[
            json!({"type":"meta","title":"Title\r\n# extra\u{1b}"}),
            json!({"type":"message","message":{"role":"user","content":"```text\r\nunclosed"}}),
            json!({"type":"message","message":{"role":"assistant","content":"<!-- unclosed","status":"interrupted"}}),
            json!({"type":"message","message":{"role":"system","content":"visible"}}),
        ]),
    );
    let output = command(directory.path())
        .args(["session", "export", "--raw"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let markdown = std::str::from_utf8(&output.stdout).unwrap();
    assert!(markdown.starts_with("# Title \\# extra"));
    assert!(!markdown.contains('\x1b'));
    let headings = pulldown_cmark::Parser::new(markdown)
        .filter(|event| {
            matches!(
                event,
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading { .. })
            )
        })
        .count();
    assert_eq!(headings, 4, "{markdown}");
    assert!(markdown.contains("> Assistant response interrupted."));
}

#[test]
fn markdown_pairs_unique_results_and_keeps_ambiguous_results_visible_in_source_order() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let unique = uuid::Uuid::from_u128(10);
    let duplicate = uuid::Uuid::from_u128(11);
    let missing = uuid::Uuid::from_u128(12);
    let events = json_transcript(&[
        json!({"type":"message","message":{"role":"assistant","content":"first",
            "tool_calls":[
                {"name":"write","id":unique,"arguments":{"encoding":"base64","content":"ZW5jb2RlZA=="}},
                {"name":"inspect","id":duplicate,"arguments":{"object":{"":null},"array":[[],{},false,{"x":"x\ny"}]}},
                {"name":"inspect","id":missing,"arguments":42}
        ]}}),
        json!({"type":"message","message":{"role":"user","content":format!("Keep {unique} as authored.")}}),
        json!({"type":"message","message":{"role":"tool_result","name":"inspect","id":duplicate,"result":"unpaired one"}}),
        json!({"type":"message","message":{"role":"tool_error","name":"write","id":unique,"error":"failed with ```"}}),
        json!({"type":"message","message":{"role":"tool_result","name":"inspect","id":duplicate,"result":"unpaired two"}}),
    ]);
    write_export_source(directory.path(), identity, &events);
    let output = command(directory.path())
        .args(["session", "export", "--raw"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let markdown = std::str::from_utf8(&output.stdout).unwrap();
    assert!(markdown.contains("### Tool: `inspect` #1"));
    assert!(markdown.contains("### Tool: `inspect` #2"));
    assert!(markdown.contains("> Result not paired."));
    assert!(markdown.contains("> Result not recorded."));
    assert!(markdown.contains("````text\nfailed with ```\n````"));
    assert!(markdown.contains("- **\"\":** `null`"));
    assert!(markdown.contains("4.\n     - **x:** x\\\n       y"));
    assert!(markdown.contains("```text\n  ZW5jb2RlZA==\n  ```"));
    assert!(markdown.find("### Tool: `write`").unwrap() < markdown.find("> Tool error:").unwrap());
    assert!(markdown.find("> Tool error:").unwrap() < markdown.find("## User").unwrap());
    assert_eq!(markdown.matches(&unique.to_string()).count(), 1);
    assert_eq!(markdown.matches("## Tool result: `inspect`").count(), 2);
}

#[test]
fn markdown_retains_parent_side_results_when_child_is_unrequested() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let root = uuid::Uuid::new_v4();
    let child = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        root,
        &json_transcript(&[
            json!({"type":"subagent_start","id":child,"parent_session":root,"created_at":"example",
               "agent_type":"default","task":"Check this."}),
            json!({"type":"subagent_result","id":child,"parent_session":root,"completed_at":"example",
               "status":"failure","result":"still visible\nacross lines"}),
        ]),
    );
    let output = command(directory.path())
        .args(["session", "export", "--raw", "--no-subagents"])
        .arg(root.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let markdown = std::str::from_utf8(&output.stdout).unwrap();
    assert!(markdown.contains("## Subagent #1 completed"));
    assert!(markdown.contains("- Status: `failure`\n- Result: still visible\n  across lines"));
    assert!(!markdown.contains("subagent-1.md"));
    assert!(!markdown.contains(&child.to_string()));
}

#[test]
fn sanitized_jsonl_preserves_source_lines_and_an_oversized_unterminated_record() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let content = "x".repeat(512 * 1024);
    let source = format!(
        "{{\"type\":\"unknown\"}}\r\ninvalid line\n{}",
        json!({"type":"future","content":content})
    );
    write_export_source(directory.path(), identity, source.as_bytes());
    let scanner = scanner_directory(directory.path(), "/bin/cat >/dev/null");
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "--no-subagents", "-f", "jsonl"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let records: Vec<serde_json::Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    assert!(records[1]["event"].is_null());
    assert_eq!(records[2]["source"]["line"], 3);
    assert_eq!(records[2]["event"]["content"], content);
}

#[test]
fn markdown_uses_bundle_local_anchors_even_when_starts_are_withheld_or_repeated() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let root = uuid::Uuid::from_u128(1);
    let clone = uuid::Uuid::from_u128(2);
    let missing = uuid::Uuid::from_u128(3);
    let shared = uuid::Uuid::from_u128(4);
    let other = uuid::Uuid::from_u128(5);
    let start = |id, parent_session, task: &str| {
        json!({"type":"subagent_start","id":id,"parent_session":parent_session,
            "created_at":"example","agent_type":"default","task":task})
    };
    for (identity, events) in [
        (
            root,
            vec![
                json!({"type":"meta","title":["unsafe"]}),
                start(missing, root, "Unavailable"),
                json!({"type":"message","message":{"role":"user","content":"## Subagent #2 started"}}),
                start(shared, root, "Started"),
                start(shared, root, "Started again"),
                start(other, root, "Also delegated"),
            ],
        ),
        (clone, vec![start(shared, clone, "withhold-task")]),
        (other, vec![start(shared, other, "Another parent")]),
        (shared, vec![start(root, shared, "Cycle back")]),
    ] {
        write_export_source(directory.path(), identity, &json_transcript(&events));
    }
    let scanner = scanner_directory(
        directory.path(),
        r#"
if /usr/bin/grep -q withhold-task; then
    printf '%s\n' '{"Raw":"withhold-task","DecoderName":"BASE64"}'
fi
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-o", "exports"])
        .args([root.to_string(), clone.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let read =
        |file: &str| std::fs::read_to_string(directory.path().join("exports").join(file)).unwrap();
    let parent = read("session-1/session.md");
    assert!(parent
        .starts_with("# Session\n\nParent: [Subagent #2](subagent-2.md#ylva-start:subagent-0)"));
    assert!(!parent.contains("unsafe"));
    assert_eq!(parent.matches("name=\"ylva-start:subagent-2\"").count(), 1);
    assert!(parent.contains("## Subagent #2 started\n\n<a name=\"ylva-start:subagent-2\"></a>\n\n## [Subagent #2](subagent-2.md) started"));
    assert!(parent.contains("## Subagent #1 started"));
    assert!(read("session-1/subagent-2.md").contains(
        "Parents: [Session](session.md#ylva-start:subagent-2), [Subagent #3](subagent-3.md#ylva-start:subagent-2)"
    ));
    assert!(read("session-2/subagent-1.md").contains(
        "Parents: [Session](session.md), [Subagent #2](subagent-2.md#ylva-start:subagent-1), [Subagent #4](subagent-4.md#ylva-start:subagent-1)"
    ));
    assert!(read("session-2/session.md").contains("> Content withheld."));
}

#[test]
fn raw_markdown_omits_typed_media_but_preserves_encoded_text() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        &json_transcript(&[
            json!({"type":"message","message":{"role":"tool_result","name":"read","result":"private-image",
            "attachments":[{"type":"image","media_type":"image/png"}]}}),
            json!({"type":"message","message":{"role":"tool_result","name":"read","result":"ZW5jb2RlZA==",
            "result_metadata":{"encoding":"base64"}}}),
            json!({"type":"message","message":{"role":"tool_result","name":"read",
            "result":"[Encoded content omitted.]"}}),
        ]),
    );
    let output = command(directory.path())
        .args(["session", "export", "--raw"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let markdown = std::str::from_utf8(&output.stdout).unwrap();
    assert!(markdown.contains("> Media omitted: image/png."));
    assert!(!markdown.contains("private-image"));
    assert!(markdown.contains("```text\nZW5jb2RlZA==\n```"));
    assert!(markdown.contains("```text\n[Encoded content omitted.]\n```"));
}

fn scanner_directory(directory: &std::path::Path, scan_body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = directory.join("scanner");
    std::fs::create_dir_all(&path).unwrap();
    let scanner = path.join("trufflehog");
    std::fs::write(&scanner, format!(
        r#"#!/bin/sh
case "$1 $2" in
    '--version ') printf 'trufflehog 3.99.7\n'; exit 0 ;;
    'stdin --help') printf 'stdin --json --no-verification --no-update --no-ignore-tag --fail-on-scan-errors\n'; exit 0 ;;
esac
test "$*" = "stdin --json --no-verification --no-update --no-ignore-tag --fail-on-scan-errors" || exit 9
{scan_body}
"#
    )).unwrap();
    std::fs::set_permissions(scanner, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}

#[test]
fn sanitized_jsonl_redacts_all_mappable_finding_parts_and_credential_assignments() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        &json_transcript(&[
            json!({"type":"future","marker-one":"marker-two","part":"marker-three",
            "text":"Authorization: Bearer synthetic-value", "API_KEY": "assignment-value"}),
        ]),
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"
/bin/cat >/dev/null
printf '%s\n' '{"Raw":"marker-one","RawV2":"marker-two","DecoderName":"PLAIN","SecretParts":{"part":"marker-three"}}'
printf '%s\n' 'private scanner diagnostics' >&2
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-f", "jsonl"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let row: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(row["source"]["line"], 1);
    assert_eq!(row["event"]["[REDACTED]"], "[REDACTED]");
    assert_eq!(row["event"]["part"], "[REDACTED]");
    assert_eq!(row["event"]["API_KEY"], "[REDACTED]");
    assert_eq!(row["event"]["text"], "Authorization: Bearer [REDACTED]");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private scanner diagnostics"));
}

#[test]
fn sanitized_jsonl_withholds_transformed_findings_even_with_an_unrelated_literal() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        br#"{"type":"future","decoded":"synthetic-secret","encoded":"c3ludGhldGljLXNlY3JldA=="}"#,
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"
/bin/cat >/dev/null
printf '%s\n' '{"Raw":"synthetic-secret","DecoderName":"BASE64"}'
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-f", "jsonl"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let row: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(row["event"].is_null());
    assert_eq!(row["omissions"][0]["reason"], "unmapped_finding");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-secret"));
}

#[test]
fn scanner_protocol_failure_discards_only_its_source_and_does_not_echo_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let broken = uuid::Uuid::new_v4();
    let available = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        broken,
        br#"{"type":"future","text":"malformed-protocol"}"#,
    );
    write_export_source(
        directory.path(),
        available,
        br#"{"type":"future","text":"safe"}"#,
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"
if /usr/bin/grep -q malformed-protocol; then
    printf '%s\n' 'private invalid stdout'
    printf '%s\n' 'private invalid stderr' >&2
fi
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args([
            "session",
            "export",
            "--no-subagents",
            "-f",
            "jsonl",
            "-o",
            "exports",
        ])
        .args([broken.to_string(), available.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!directory.path().join("exports/session-1").exists());
    assert!(directory
        .path()
        .join("exports/session-2/session.jsonl")
        .is_file());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private invalid"));
}

#[test]
fn export_manifest_survives_when_no_transcript_stages() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");
    let scanner = scanner_directory(directory.path(), "exit 5");
    let existing = directory.path().join("unstaged.jsonl");
    std::fs::write(&existing, b"not an actual destination").unwrap();
    let output = command(directory.path())
        .env("PATH", scanner)
        .args([
            "session",
            "export",
            "-f",
            "jsonl",
            "-o",
            "unstaged.jsonl",
            "--manifest",
            "report.json",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(existing).unwrap(),
        b"not an actual destination"
    );
    let manifest = read_export_manifest(directory.path(), "report.json");
    assert_eq!(manifest["status"], "failed");
    assert_eq!(manifest["sessions"][0]["availability"], "available");
    let output = &manifest["outputs"][0];
    assert_eq!(output["publication_status"], "unattempted");
    assert_eq!(output["scanner_version"], "3.99.7");
    assert!(output["content_completeness"].is_null());
    assert!(output["destination"].is_null());
    assert_eq!(manifest["bundles"][0]["status"], "failed");
}

#[test]
fn sanitized_export_requires_a_compatible_absolute_scanner_but_raw_does_not() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{}");
    let scanner = scanner_directory(directory.path(), "exit 0");
    let run = |path: &std::path::Path, raw: bool| {
        let mut command = command(directory.path());
        command
            .env("PATH", path)
            .args(["session", "export", "-f", "jsonl"]);
        if raw {
            command.arg("--raw");
        }
        command.arg(identity.to_string()).output().unwrap()
    };
    assert_eq!(
        run(std::path::Path::new("scanner"), false).status.code(),
        Some(2)
    );
    let incompatible = scanner.join("trufflehog");
    std::fs::write(
        &incompatible,
        b"#!/bin/sh\nprintf '%s\\n' 'untrusted-probe-output'\n",
    )
    .unwrap();
    let output = run(&scanner, false);
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("untrusted-probe-output"));
    let output = run(&scanner, true);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}");
}

#[test]
fn sanitized_jsonl_omits_typed_and_encoded_content_before_scanner_input() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        &json_transcript(&[
            json!({"type":"message","message":{"role":"assistant","content":"","tool_calls":[
                {"name":"write","arguments":{"content":"private-payload","encoding":"base64"}},
                {"name":"write","arguments":{"content":"[Encoded content omitted.]","encoding":"text"}}
            ]}}),
            json!({"type":"message","message":{"role":"tool_result","name":"read","result":"private-payload",
            "result_metadata":{"encoding":"base64"},"content_sections":[{"text":"private-payload"}]}}),
            json!({"type":"message","message":{"role":"tool_result","name":"read","result":"private-payload",
            "result_metadata":{"encoding":"base64"},"attachments":[{"type":"image","media_type":"image/png"}]}}),
        ]),
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"
if /usr/bin/grep -q private-payload; then exit 8; fi
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-f", "jsonl"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<serde_json::Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["omissions"].as_array().unwrap().len(), 1);
    assert_eq!(lines[1]["omissions"].as_array().unwrap().len(), 2);
    assert_eq!(lines[2]["omissions"][0]["reason"], "media");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-payload"));
}

#[test]
fn sanitized_jsonl_withholds_a_batch_when_redacting_a_key_would_merge_fields() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        b"{\"type\":\"unknown\",\"old-key\":1,\"[REDACTED]\":2}\n{\"type\":\"other\"}",
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"
/bin/cat >/dev/null
printf '%s\n' '{"Raw":"old-key","DecoderName":"PLAIN"}'
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-f", "jsonl"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let lines: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(record["event"].is_null());
        assert_eq!(record["omissions"][0]["reason"], "key_collision");
    }
}

#[test]
fn raw_export_numbers_each_root_bundle_by_its_own_edges_and_reserves_unavailable_numbers() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let root = uuid::Uuid::from_u128(1);
    let clone = uuid::Uuid::from_u128(2);
    let missing = uuid::Uuid::from_u128(3);
    let shared = uuid::Uuid::from_u128(4);
    let nested = uuid::Uuid::from_u128(5);
    let start = |id, parent_session| {
        json!({
            "type":"subagent_start", "id":id, "parent_session":parent_session,
            "created_at":"2026-01-01T00:00:00Z", "agent_type":"default", "task":"test"
        })
    };
    let shared_source = json_transcript(&[start(nested, shared)]);
    let nested_source = json_transcript(&[start(root, nested)]);
    for (identity, bytes) in [
        (
            root,
            json_transcript(&[
                json!({"type":"message","message":{"role":"tool_result","name":"spawn_subagent",
                "result":json!({"event":"subagent_accepted","run_id":missing,"status":"queued"}).to_string()}}),
                start(shared, root),
                start(shared, root),
            ]),
        ),
        (clone, json_transcript(&[start(shared, root)])),
        (shared, shared_source.clone()),
        (nested, nested_source),
    ] {
        write_export_source(directory.path(), identity, &bytes);
    }

    let output = command(directory.path())
        .args(["session", "export", "--raw", "-f", "jsonl", "-o", "exports"])
        .args([root.to_string(), clone.to_string(), root.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let exports = directory.path().join("exports");
    assert!(!exports.join("session-1/subagent-1.jsonl").exists());
    assert_eq!(
        std::fs::read(exports.join("session-1/subagent-2.jsonl")).unwrap(),
        shared_source
    );
    assert_eq!(
        std::fs::read(exports.join("session-2/subagent-1.jsonl")).unwrap(),
        shared_source
    );
    assert!(exports.join("session-1/subagent-3.jsonl").is_file());
    assert!(exports.join("session-2/subagent-3.jsonl").is_file());
    assert!(!exports.join("session-3").exists());
}

#[test]
fn raw_export_with_no_subagents_copies_ambiguous_and_legacy_events_as_warnings() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"unknown\",\"nested\":{\"key\":1,\"key\":2}}\n{\"type\":\"child_result\"}\n{}\npartial";
    write_export_source(directory.path(), identity, bytes);
    for (arguments, expected_code) in [(&[][..], 1), (&["--no-subagents"][..], 0)] {
        let output = command(directory.path())
            .args(["session", "export", "--raw", "-f", "jsonl"])
            .args(arguments)
            .arg(identity.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected_code), "{output:?}");
        assert_eq!(output.stdout, bytes);
        assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate_keys"));
    }
}

#[test]
fn export_templates_always_make_bundles_and_reject_malformed_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"example\"}\n";
    write_export_source(directory.path(), identity, bytes);
    let export = |template: &str| {
        command(directory.path())
            .args(["session", "export", "--raw", "-f", "jsonl", "-o", template])
            .arg(identity.to_string())
            .output()
            .unwrap()
    };
    let output = export("exports/{project_key}/{session_id}-{format}");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(directory.path().join(format!(
            "exports/{}/{identity}-jsonl/session.jsonl",
            project_key(directory.path())
        )))
        .unwrap(),
        bytes
    );
    for template in [
        "bad/{unknown}",
        "bad/{session_id",
        "bad/x}",
        "bad/{{format}}",
    ] {
        assert_eq!(export(template).status.code(), Some(2), "{template}");
    }
    assert!(!directory.path().join("bad").exists());
}

#[test]
fn export_resolves_quoted_home_relative_paths_without_expanding_home_as_a_template() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home-{session_title}");
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"meta\",\"title\":\"Ship It\"}\n";
    write_export_source(directory.path(), identity, bytes);
    for (argument, relative_path) in [
        ("~/tmp/transcript.jsonl", "tmp/transcript.jsonl".into()),
        (
            "~/tmp/{session_id}-{session_title}",
            format!("tmp/{identity}-ship-it/session.jsonl"),
        ),
    ] {
        let output = command(directory.path())
            .env("HOME", &home)
            .args(["session", "export", "--raw", "-f", "jsonl", "-o", argument])
            .arg(identity.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(std::fs::read(home.join(relative_path)).unwrap(), bytes);
    }
    let output = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "-o",
            "~/tmp/transcript.jsonl",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!directory.path().join("~").exists());
}

#[test]
fn export_title_template_uses_path_safe_slugs_and_falls_back_to_untitled() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let titled = json_transcript(&[json!({"type":"meta","title":"../../ Ship\\IT\u{001b}!"})]);
    let untitled = json_transcript(&[json!({"type":"meta","title":"../..."})]);
    write_export_source(directory.path(), first, &titled);
    write_export_source(directory.path(), second, &untitled);
    let output = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "-o",
            "exports/{session_title}",
        ])
        .args([first.to_string(), second.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    for (slug, bytes) in [("ship-it", titled), ("untitled", untitled)] {
        assert_eq!(
            std::fs::read(
                directory
                    .path()
                    .join(format!("exports/{slug}/session.jsonl"))
            )
            .unwrap(),
            bytes
        );
    }
}

#[test]
fn sanitized_export_title_template_uses_the_scanned_title() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        identity,
        &json_transcript(&[json!({"type":"meta","title":"Deploy synthetic-marker"})]),
    );
    let scanner = scanner_directory(
        directory.path(),
        r#"/bin/cat >/dev/null
printf '%s\n' '{"Raw":"synthetic-marker","DecoderName":"PLAIN"}'"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args([
            "session",
            "export",
            "-f",
            "jsonl",
            "-o",
            "exports/{session_title}",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(directory
        .path()
        .join("exports/deploy-redacted/session.jsonl")
        .is_file());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-marker"));
    assert!(!directory
        .path()
        .join("exports/deploy-synthetic-marker")
        .exists());
}

#[test]
fn export_refuses_the_whole_set_if_one_actual_bundle_is_nonempty() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let missing = uuid::Uuid::new_v4();
    write_export_source(directory.path(), first, b"{\"type\":\"example\"}\n");
    write_export_source(
        directory.path(),
        second,
        &json_transcript(&[
            json!({"type":"subagent_start","id":missing,"parent_session":second,
            "created_at":"example","agent_type":"default","task":"Unavailable"}),
        ]),
    );
    let occupied = directory.path().join("exports/session-2");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("subagent-1.jsonl"), b"keep me").unwrap();
    let output = command(directory.path())
        .args([
            "session", "export", "--raw", "-f", "jsonl", "--force", "-o", "exports",
        ])
        .args([first.to_string(), second.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!directory.path().join("exports/session-1").exists());
    assert_eq!(
        std::fs::read(occupied.join("subagent-1.jsonl")).unwrap(),
        b"keep me"
    );
    assert!(!occupied.join("session.jsonl").exists());
}

#[test]
fn export_keeps_a_child_when_its_root_does_not_stage() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let root = uuid::Uuid::new_v4();
    let child = uuid::Uuid::new_v4();
    write_export_source(
        directory.path(),
        root,
        &json_transcript(&[
            json!({"type":"subagent_start","id":child,"parent_session":root,
            "created_at":"example","agent_type":"default","task":"fail-this-source"}),
        ]),
    );
    write_export_source(directory.path(), child, b"{\"type\":\"example\"}\n");
    let scanner = scanner_directory(
        directory.path(),
        r#"
if /usr/bin/grep -q fail-this-source; then exit 7; fi
"#,
    );
    let output = command(directory.path())
        .env("PATH", scanner)
        .args(["session", "export", "-f", "jsonl", "-o", "bundle"])
        .arg(root.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(directory.path().join("bundle/subagent-1.jsonl").is_file());
    assert!(!directory.path().join("bundle/session.jsonl").exists());
}

#[test]
fn export_checks_resolved_parent_paths_and_refuses_final_symlinks() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"example\"}\n";
    let source = write_export_source(directory.path(), identity, bytes);
    std::fs::create_dir(directory.path().join("outside")).unwrap();
    symlink("outside", directory.path().join("alias")).unwrap();
    symlink("chats", directory.path().join("store-alias")).unwrap();
    symlink(source, directory.path().join("outside/final-link")).unwrap();
    let export = |path: &str| {
        command(directory.path())
            .args([
                "session", "export", "--raw", "-f", "jsonl", "--force", "-o", path,
            ])
            .arg(identity.to_string())
            .output()
            .unwrap()
    };
    let written = export("alias/transcript");
    assert!(written.status.success(), "{written:?}");
    assert_eq!(
        std::fs::read(directory.path().join("outside/transcript")).unwrap(),
        bytes
    );
    for path in ["store-alias/new/destination", "alias/final-link"] {
        let output = export(path);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }
    assert!(!directory.path().join("chats/new").exists());
    assert!(directory.path().join("outside/final-link").is_symlink());
}

#[test]
fn export_detects_all_destination_aliases_before_publishing() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    for identity in [first, second] {
        write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");
    }
    let root = directory.path();
    std::fs::create_dir(root.join("actual")).unwrap();
    symlink("actual", root.join(first.to_string())).unwrap();
    symlink("actual", root.join(second.to_string())).unwrap();
    let output = command(root)
        .args(["session", "export", "--raw", "-o", "{session_id}/bundle"])
        .args([first.to_string(), second.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("bundle destinations overlap"));
    assert!(!root.join("actual/bundle").exists());
    for (transcript, manifest) in [
        ("OUT/transcript.json", "out/transcript.txt"),
        ("OUT/caf\u{e9}.json", "OUT/cafe\u{301}.json"),
        ("OUT", "OUT/subdir/report"),
    ] {
        let output = command(root)
            .args([
                "session",
                "export",
                "--raw",
                "-o",
                transcript,
                "--manifest",
                manifest,
            ])
            .arg(first.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("destinations overlap"));
        assert!(!root.join("OUT").exists());
    }
    let output = command(root)
        .args([
            "session",
            "export",
            "--raw",
            "-o",
            "bundle-{format}",
            "--manifest",
            "bundle-md/nested/report",
        ])
        .arg(first.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside bundle"));
    assert!(!root.join("bundle-md").exists());
}

#[test]
fn export_manifest_adds_or_replaces_the_json_extension() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");
    for (argument, expected) in [
        ("report", "report.json"),
        ("report.v2.txt", "report.v2.json"),
    ] {
        let output = command(directory.path())
            .args(["session", "export", "--raw", "--manifest", argument])
            .arg(identity.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let report = read_export_manifest(directory.path(), expected);
        assert_eq!(report["version"], 1);
        assert!(!directory.path().join(argument).exists());
    }
}

#[test]
fn export_abbreviates_console_paths_without_changing_manifest_paths() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");

    let output = command(directory.path())
        .env("HOME", &home)
        .args([
            "session",
            "export",
            "--raw",
            "-o",
            "~/exports/session.md",
            "--manifest",
            "~/exports/report.json",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostics.contains("\"~/exports/session.md\""));
    assert!(diagnostics.contains("created export directory \"~/exports\""));
    assert!(diagnostics.contains("published export manifest \"~/exports/report.json\""));

    let report = read_export_manifest(&home, "exports/report.json");
    assert_eq!(
        report["outputs"][0]["destination"],
        home.join("exports/session.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
}

#[test]
fn export_manifest_resolves_quoted_home_relative_path() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");

    let export = || {
        command(directory.path())
            .env("HOME", &home)
            .args([
                "session",
                "export",
                "--raw",
                "--manifest",
                "~/reports/{format}.txt",
            ])
            .arg(identity.to_string())
            .output()
            .unwrap()
    };
    let output = export();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        read_export_manifest(&home, "reports/{format}.json")["version"],
        1
    );
    assert!(!directory.path().join("~").exists());

    let conflict = export();
    assert_eq!(conflict.status.code(), Some(2), "{conflict:?}");
    assert!(conflict.stdout.is_empty());
}

#[test]
fn export_preflights_a_stdout_manifest_before_streaming() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");
    let manifest = directory.path().join("manifest.json");
    std::fs::write(&manifest, b"old report").unwrap();
    let output = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "--manifest",
            "manifest.txt",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination already exists"));
    assert_eq!(std::fs::read(manifest).unwrap(), b"old report");
}

#[test]
fn export_manifest_records_the_canonical_launch_directory_separately_from_source_home() {
    let directory = tempfile::tempdir().unwrap();
    let launch = directory.path().join("project");
    let alias = directory.path().join("project-alias");
    let home = directory.path().join("ylva-home");
    std::fs::create_dir(&launch).unwrap();
    std::os::unix::fs::symlink(&launch, &alias).unwrap();
    let identity = uuid::Uuid::new_v4();
    let key = project_key(&launch);
    let source = session_path(&home.join("chats"), &key, identity);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(source, b"{\"type\":\"example\"}\n").unwrap();

    let output = command(&alias)
        .env("YLVA_HOME", &home)
        .args(["session", "export", "--raw", "--manifest", "report.json"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report = read_export_manifest(&launch, "report.json");
    assert_eq!(
        report["launch_directory"],
        launch.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        report["source_home"],
        home.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(report["project_key"], key);
}

#[test]
fn export_manifest_records_confirmed_stdout_without_transcript_content() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"unknown\",\"custom-private-key\":\"private-text\"}\r\npartial";
    let path = write_export_source(directory.path(), identity, bytes);
    let result = command(directory.path())
        .args([
            "session",
            "export",
            "--no-subagents",
            "--raw",
            "-f",
            "jsonl",
            "--manifest",
            "report.json",
        ])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, bytes);

    let report_path = directory.path().join("report.json");
    let report_text = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&report_text).unwrap();
    assert_eq!(report["version"], 1);
    assert_eq!(
        report["source_home"],
        directory.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(report["project_key"], project_key(directory.path()));
    assert_eq!(report["selected_ids"], serde_json::json!([identity]));
    assert_eq!(report["status"], "success");
    let session = &report["sessions"][0];
    assert_eq!(session["availability"], "available");
    assert_eq!(session["representation"], "plain");
    assert_eq!(
        session["source_paths"]["plain"],
        path.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(session["discovery"], "not_requested");
    assert_eq!(session["boundary"]["logical_bytes"], bytes.len());
    assert_eq!(session["boundary"]["logical_sha256"], sha256_hex(bytes));
    assert_eq!(
        session["issues"][0],
        serde_json::json!({"line": 2, "reason": "incomplete_tail"})
    );
    assert_eq!(report["bundles"][0]["status"], "published");
    let output = &report["outputs"][0];
    assert_eq!(output["stdout"], true);
    assert!(output["destination"].is_null());
    assert_eq!(output["publication_status"], "published");
    assert_eq!(output["content_completeness"], "complete");
    assert_eq!(output["output_sha256"], sha256_hex(&result.stdout));
    assert!(output["scanner_version"].is_null());
    assert_eq!(output["omission_count"], 0);
    assert_eq!(
        std::fs::metadata(report_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!report_text.contains("private-text"));
    assert!(!report_text.contains("custom-private-key"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("published export manifest"));
}

fn read_export_manifest(directory: &std::path::Path, name: &str) -> serde_json::Value {
    let bytes = std::fs::read(directory.join(name)).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn export_manifest_relates_shared_snapshots_and_unavailable_members_without_leaking_content() {
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let [root, clone, shared, missing] = [1, 2, 3, 4].map(uuid::Uuid::from_u128);
    let start = |parent| {
        json!({"type":"subagent_start","id":shared,"parent_session":parent,
            "created_at":"example","agent_type":"default","task":"private-task"})
    };
    let admission = json!({"event":"subagent_accepted","run_id":missing,"status":"queued"});
    let fixtures = [
        (
            root,
            vec![
                json!({"type":"meta","title":"private-title"}),
                json!({"type":"message","message":{"role":"tool_result","name":"spawn_subagent",
                "result":admission.to_string()}}),
                start(root),
                json!({"type":"subagent_result","id":shared,"parent_session":root,
                "completed_at":"example","status":"success","result":"private-result"}),
            ],
        ),
        (clone, vec![start(clone)]),
        (
            shared,
            vec![
                json!({"type":"meta","title":{"private-unsafe-key":"private-unsafe-title"}}),
                json!({"type":"message","message":{"role":"assistant","content":"private-message",
                "tool_calls":[{"name":"write","arguments":{"encoding":"base64","content":"cHJpdmF0ZQ=="}}]}}),
                json!({"type":"future","custom-private-key":"synthetic-finding"}),
            ],
        ),
    ];
    for (identity, events) in fixtures {
        write_export_source(directory.path(), identity, &json_transcript(&events));
    }
    let scanner = scanner_directory(
        directory.path(),
        r#"
if /usr/bin/grep -q synthetic-finding; then
    printf '%s\n' '{"Raw":"synthetic-finding","DecoderName":"PLAIN"}'
    printf '%s\n' 'private-scanner-diagnostic' >&2
fi
"#,
    );
    let result = command(directory.path())
        .env("PATH", scanner)
        .args([
            "session",
            "export",
            "-f",
            "jsonl",
            "-o",
            "exports",
            "--manifest",
            "report.json",
        ])
        .args([root.to_string(), clone.to_string(), root.to_string()])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty());
    let report_bytes = std::fs::read(directory.path().join("report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    let report_text = std::str::from_utf8(&report_bytes).unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    for private in [
        "private-title",
        "private-task",
        "private-result",
        "private-unsafe-key",
        "private-unsafe-title",
        "private-message",
        "custom-private-key",
        "synthetic-finding",
        "private-scanner-diagnostic",
        "cHJpdmF0ZQ==",
    ] {
        assert!(
            !report_text.contains(private) && !stderr.contains(private),
            "{private}"
        );
    }
    assert_eq!(report["selected_ids"], json!([root, clone]));
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 4);
    assert_eq!(sessions[0]["discovery"], "incomplete");
    assert_eq!(sessions[1]["discovery"], "complete");
    assert_eq!(sessions[2]["session_id"], json!(missing));
    assert_eq!(sessions[2]["availability"], "missing");
    assert!(sessions[2]["boundary"].is_null());
    assert_eq!(report["bundles"][0]["status"], "partial");
    assert_eq!(report["bundles"][1]["status"], "published");
    assert_eq!(
        report["edges"],
        json!([
            {"parent_session_id":root,"child_session_id":missing,"line":2,"kind":"subagent_accepted"},
            {"parent_session_id":root,"child_session_id":shared,"line":3,"kind":"subagent_start"},
            {"parent_session_id":root,"child_session_id":shared,"line":4,"kind":"subagent_result"},
            {"parent_session_id":clone,"child_session_id":shared,"line":1,"kind":"subagent_start"},
        ]),
    );
    let outputs = report["outputs"].as_array().unwrap();
    let absent = &outputs[1];
    assert_eq!(absent["session_id"], json!(missing));
    assert_eq!(absent["number"], 1);
    assert!(absent["destination"].is_null());
    assert_eq!(absent["publication_status"], "unattempted");
    for (copy, parent, number) in [(&outputs[2], root, 2), (&outputs[4], clone, 1)] {
        assert_eq!(copy["root_session_id"], json!(parent));
        assert_eq!(copy["session_id"], json!(shared));
        assert_eq!(copy["number"], number);
        assert_eq!(copy["publication_status"], "published");
        assert_eq!(copy["content_completeness"], "incomplete");
        assert_eq!(copy["scanner_version"], "3.99.7");
        let bytes = std::fs::read(copy["destination"].as_str().unwrap()).unwrap();
        assert_eq!(copy["output_sha256"], sha256_hex(&bytes));
        assert_eq!(copy["omission_count"], 2);
        assert_eq!(
            copy["omissions"],
            json!([
                {"line":1,"reason":"unsafe_title","field":"/title"},
                {"line":2,"reason":"encoded_content","field":"/message/tool_calls/0/arguments/content"},
            ])
        );
    }
    assert_eq!(outputs[2]["output_sha256"], outputs[4]["output_sha256"]);
}

#[test]
fn export_creates_private_paths_and_force_replaces_only_the_standalone_file() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"example\"}\n";
    write_export_source(directory.path(), identity, bytes);
    let existing = directory.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
    let arguments = [
        "session",
        "export",
        "--raw",
        "-f",
        "jsonl",
        "-o",
        "existing/new/export",
    ];
    let output = command(directory.path())
        .args(arguments)
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("created export directory"));
    let mode = |path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let destination = existing.join("new/export");
    assert_eq!(mode(existing.clone()), 0o755);
    assert_eq!(mode(existing.join("new")), 0o700);
    assert_eq!(mode(destination.clone()), 0o600);
    std::fs::write(&destination, b"previous contents").unwrap();
    let output = command(directory.path())
        .args(arguments)
        .arg("--force")
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
}

#[test]
fn export_serializes_competing_writers_without_leaving_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"example\"}\n";
    write_export_source(directory.path(), identity, bytes);
    std::fs::create_dir(directory.path().join("out")).unwrap();
    let spawn = || {
        command(directory.path())
            .args([
                "session",
                "export",
                "--raw",
                "-f",
                "jsonl",
                "-o",
                "out/export",
            ])
            .arg(identity.to_string())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn();
    let second = spawn();
    let results = [
        first.wait_with_output().unwrap().status.code().unwrap(),
        second.wait_with_output().unwrap().status.code().unwrap(),
    ];
    assert_eq!(results.iter().filter(|&&code| code == 0).count(), 1);
    assert!(results.iter().all(|&code| matches!(code, 0..=2)));
    assert_eq!(
        std::fs::read(directory.path().join("out/export")).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read_dir(directory.path().join("out"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn export_keeps_published_files_and_continues_after_a_late_bundle_conflict() {
    use nix::fcntl::{Flock, FlockArg};
    use std::fs::{self, File};
    let directory = tempfile::tempdir().unwrap();
    let roots = [
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    ];
    let bytes = b"{\"type\":\"example\"}\n";
    for root in roots {
        write_export_source(directory.path(), root, bytes);
    }
    let outputs = directory.path().join("exports");
    let blocked_bundle = outputs.join("session-2");
    fs::create_dir_all(&blocked_bundle).unwrap();
    let lock = Flock::lock(
        File::open(&blocked_bundle).unwrap(),
        FlockArg::LockExclusive,
    )
    .unwrap();

    let mut child = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "-o",
            "exports",
            "--manifest",
            "report.json",
        ])
        .args(roots.map(|root| root.to_string()))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first = outputs.join("session-1/session.jsonl");
    let preflight_succeeded = wait_for_export_file(&mut child, &first);
    if preflight_succeeded {
        fs::write(blocked_bundle.join(".keep"), b"late arrival").unwrap();
    } else {
        let _ = child.kill();
    }
    drop(lock);
    let result = child.wait_with_output().unwrap();

    assert!(preflight_succeeded, "{result:?}");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(fs::read(first).unwrap(), bytes);
    assert_eq!(
        fs::read(blocked_bundle.join(".keep")).unwrap(),
        b"late arrival"
    );
    assert_eq!(fs::read_dir(blocked_bundle).unwrap().count(), 1);
    assert_eq!(
        fs::read(outputs.join("session-3/session.jsonl")).unwrap(),
        bytes
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains(&format!(
        "session {} for root {}: failed",
        roots[1], roots[1]
    )));
    assert!(stderr.contains("no longer empty"));
    assert!(stderr.lines().any(|line| {
        line.contains("created export directory") && line.contains("exports/session-3")
    }));
    let manifest = read_export_manifest(directory.path(), "report.json");
    let outputs = manifest["outputs"].as_array().unwrap();
    let digest = sha256_hex(bytes);
    assert_eq!(manifest["status"], "failed");
    assert_eq!(outputs[0]["output_sha256"], digest);
    assert_eq!(outputs[1]["publication_status"], "failed");
    assert!(outputs[1]["output_sha256"].is_null());
    assert_eq!(outputs[2]["output_sha256"], digest);
}

fn wait_for_export_file(child: &mut std::process::Child, path: &std::path::Path) -> bool {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if path.is_file() {
            return true;
        }
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn export_publishes_manifest_after_interruption_follows_preflight() {
    use nix::fcntl::{Flock, FlockArg};
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use std::fs::{self, File};
    let directory = tempfile::tempdir().unwrap();
    let roots = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
    let bytes = b"{\"type\":\"example\"}\n";
    for root in roots {
        write_export_source(directory.path(), root, bytes);
    }
    let exports = directory.path().join("exports");
    let blocked = exports.join("session-2");
    let scratch = directory.path().join("scratch");
    fs::create_dir_all(&blocked).unwrap();
    fs::create_dir(&scratch).unwrap();
    let _lock = Flock::lock(File::open(&blocked).unwrap(), FlockArg::LockExclusive).unwrap();

    let mut child = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "-o",
            "exports",
            "--manifest",
            "reports/report.json",
        ])
        .args(roots.map(|root| root.to_string()))
        .env("TMPDIR", &scratch)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first = exports.join("session-1/session.jsonl");
    let preflight_succeeded = wait_for_export_file(&mut child, &first);
    if preflight_succeeded {
        kill(
            Pid::from_raw(i32::try_from(child.id()).unwrap()),
            Signal::SIGTERM,
        )
        .unwrap();
    } else {
        let _ = child.kill();
    }
    let result = child.wait_with_output().unwrap();

    assert!(preflight_succeeded, "{result:?}");
    assert_eq!(result.status.code(), Some(130), "{result:?}");
    assert_eq!(fs::read(first).unwrap(), bytes);
    assert_eq!(fs::read_dir(blocked).unwrap().count(), 0);
    assert_eq!(fs::read_dir(scratch).unwrap().count(), 0);
    let report = read_export_manifest(directory.path(), "reports/report.json");
    assert_eq!(report["status"], "interrupted");
    assert_eq!(report["outputs"][0]["publication_status"], "published");
    assert_eq!(report["outputs"][0]["output_sha256"], sha256_hex(bytes));
    assert_eq!(report["outputs"][1]["publication_status"], "unattempted");
    assert!(report["outputs"][1]["output_sha256"].is_null());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("unattempted"));
    assert!(stderr.contains("published export manifest"));
}

#[test]
fn export_manifest_does_not_claim_a_digest_for_partial_stdout() {
    use std::io::Read;
    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let bytes = format!(
        "{{\"type\":\"example\",\"content\":\"{}\"}}\n",
        "x".repeat(4 * 1024 * 1024)
    );
    write_export_source(directory.path(), identity, bytes.as_bytes());
    let mut child = command(directory.path())
        .args([
            "session",
            "export",
            "--raw",
            "-f",
            "jsonl",
            "--manifest",
            "report.json",
        ])
        .arg(identity.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut prefix = [0; 128];
    let received = stdout.read_exact(&mut prefix);
    drop(stdout);
    let result = child.wait_with_output().unwrap();

    received.unwrap();
    assert_eq!(prefix, bytes.as_bytes()[..prefix.len()]);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("stdout failed (may contain a partial payload)"));
    let report = read_export_manifest(directory.path(), "report.json");
    assert_eq!(report["status"], "failed");
    assert_eq!(report["outputs"][0]["publication_status"], "failed");
    assert!(report["outputs"][0]["output_sha256"].is_null());
}

#[cfg(target_os = "linux")]
#[test]
fn export_copies_from_scratch_on_another_filesystem() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let Ok(scratch) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let device = |path| std::fs::metadata(path).unwrap().dev();
    if device(scratch.path()) == device(directory.path()) {
        return;
    }

    let identity = uuid::Uuid::new_v4();
    let bytes = b"{\"type\":\"example\"}\n";
    write_export_source(directory.path(), identity, bytes);
    let scanner = scanner_directory(
        directory.path(),
        r#"
pwd -P > "$(/usr/bin/dirname "$0")/working-directory"
/bin/cat >/dev/null
"#,
    );
    let output = command(directory.path())
        .env("TMPDIR", scratch.path())
        .env("PATH", &scanner)
        .args(["session", "export", "-f", "jsonl", "-o", "out/export"])
        .arg(identity.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let working_directory = std::fs::read_to_string(scanner.join("working-directory")).unwrap();
    assert!(std::path::Path::new(working_directory.trim())
        .starts_with(scratch.path().canonicalize().unwrap()));
    let published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("out/export")).unwrap())
            .unwrap();
    assert_eq!(published["event"]["type"], "example");
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn export_cleans_scratch_and_reaps_the_scanner_on_sigint_and_sigterm() {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use std::time::{Duration, Instant};
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        let directory = tempfile::tempdir().unwrap();
        let identity = uuid::Uuid::new_v4();
        write_export_source(directory.path(), identity, b"{\"type\":\"example\"}\n");
        let scratch = directory.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let scanner = scanner_directory(
            directory.path(),
            r#"
printf '%s\n' "$$" > "$(/usr/bin/dirname "$0")/scanner-running"
exec /bin/sleep 30
"#,
        );
        let child = command(directory.path())
            .env("PATH", scanner)
            .env("TMPDIR", &scratch)
            .args([
                "session",
                "export",
                "-o",
                "out/export",
                "--manifest",
                "report.json",
            ])
            .arg(identity.to_string())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let scanner_file = directory.path().join("scanner/scanner-running");
        let deadline = Instant::now() + Duration::from_secs(8);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&scanner_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break pid;
            }
            if Instant::now() >= deadline {
                let _ = kill(
                    Pid::from_raw(i32::try_from(child.id()).unwrap()),
                    Signal::SIGINT,
                );
                let output = child.wait_with_output().unwrap();
                panic!("scanner did not start: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        kill(Pid::from_raw(i32::try_from(child.id()).unwrap()), signal).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(130), "{output:?}");
        assert_eq!(
            kill(Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
        assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
        assert!(!directory.path().join("out").exists());
        assert!(!directory.path().join("report.json").exists());
    }
}

#[test]
fn empty_listing_ignores_providers_profiles_and_open_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = command(directory.path())
        .args(["--profile", "missing", "session", "list"])
        .env("OLLAMA_BASE_URL", "http://127.0.0.1:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stdout.is_empty());
    assert!(!directory.path().join("chats").exists());
}
#[cfg(unix)]
#[test]
fn session_list_marks_a_held_writer_lock_but_not_an_unlocked_file() {
    use nix::fcntl::{Flock, FlockArg};

    let directory = tempfile::tempdir().unwrap();
    let identity = uuid::Uuid::new_v4();
    let project = project_key(directory.path());
    let path = session_path(&directory.path().join("chats"), &project, identity);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let header = serde_json::json!({
        "type": "meta",
        "created_at": "2026-01-01T00:00:00Z",
        "schema_version": 2,
        "app_version": "test",
        "project_key": project,
        "provider_connection": "mock",
        "model": "mock",
        "session_id": identity,
        "title": "Session title",
    });
    std::fs::write(&path, format!("{header}\n")).unwrap();
    let lock_path = path.with_extension("lock");
    let lock = Flock::lock(
        std::fs::File::create(&lock_path).unwrap(),
        FlockArg::LockExclusive,
    )
    .unwrap();
    let list = || {
        let output = command(directory.path())
            .args(["session", "list"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };

    let live_listing = list();
    assert!(
        live_listing
            .trim_end()
            .ends_with(&format!("{identity} Session title [live]")),
        "{live_listing}"
    );
    drop(lock);
    assert!(list()
        .trim_end()
        .ends_with(&format!("{identity} Session title")));
    assert!(lock_path.is_file());
}

#[tokio::test]
async fn listing_is_sorted_filtered_safe_and_diagnoses_bad_headers() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let store = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let subagent = store
        .start_new_session("mock", "mock", Some(store.session_id()), None)
        .unwrap();
    store.set_title("Title".to_string()).await.unwrap();
    let bad_path = session_path(
        &chats_root,
        &project_key(directory.path()),
        uuid::Uuid::new_v4(),
    );
    std::fs::write(&bad_path, "invalid\n").unwrap();
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(&format!("{} Title", store.session_id())));
    assert!(!text.contains(&subagent.session_id().to_string()));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("no readable meta header"));
    let output = command(directory.path())
        .args(["session", "list", "--include-subagents"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 2);
}

#[test]
fn delete_untitled_previews_and_removes_only_untitled_roots_and_unshared_descendants() {
    use serde_json::json;
    use uuid::Uuid;

    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let key = project_key(directory.path());
    let path = |id| session_path(&chats_root, &key, Uuid::from_u128(id));
    std::fs::create_dir_all(path(1).parent().unwrap()).unwrap();

    for (id, parent, title) in [
        (1, None, Some("")),
        (2, None, Some(" \t\u{2003}")),
        (3, None, None),
        (4, Some(1), Some("Unshared descendant")),
        (5, None, Some("Keep this session")),
        (6, Some(5), Some("")),
    ] {
        let mut header = json!({
            "type": "meta",
            "created_at": "2026-01-01T00:00:00Z",
            "schema_version": 2,
            "app_version": "test",
            "project_key": key,
            "provider_connection": "mock",
            "model": "mock",
            "session_id": Uuid::from_u128(id),
            "parent_session": parent.map(Uuid::from_u128),
        });
        if let Some(title) = title {
            header["title"] = title.into();
        }
        std::fs::write(path(id), format!("{header}\n")).unwrap();
    }

    let archive_id = Uuid::from_u128(2).to_string();
    let archived = command(directory.path())
        .args(["session", "compress", &archive_id])
        .output()
        .unwrap();
    assert!(archived.status.success(), "{archived:?}");
    let selected_paths = [
        path(1),
        path(2).with_extension("jsonl.gz"),
        path(2).with_extension("meta.json"),
        path(3),
        path(4),
    ];
    let existing = selected_paths
        .each_ref()
        .map(|path| std::fs::read(path).unwrap());
    let delete = |dry_run| {
        let mut command = command(directory.path());
        command.args(["session", "delete", "--untitled"]);
        if dry_run {
            command.arg("--dry-run");
        }
        command.output().unwrap()
    };
    let assert_reports = |output: std::process::Output, prefix: &str| {
        assert!(output.status.success(), "{output:?}");
        let notices = String::from_utf8(output.stdout).unwrap();
        let mut actual: Vec<_> = notices.lines().collect();
        actual.sort_unstable();
        assert_eq!(
            actual,
            [1, 2, 3, 4].map(|id| format!("{prefix} {}", Uuid::from_u128(id)))
        );
    };
    assert_reports(delete(true), "would delete");
    for (path, original) in selected_paths.iter().zip(existing) {
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    assert_reports(delete(false), "deleted");
    assert!(selected_paths.iter().all(|path| !path.exists()));
    assert!(path(5).is_file() && path(6).is_file());
}

#[test]
fn maintenance_requires_one_valid_selector() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["session", "delete"],
        vec!["session", "delete", "--dry-run"],
        vec!["session", "delete", "--untitled", "--all"],
        vec!["session", "delete", "--untitled", "--before", "2026-01-01"],
        vec![
            "session",
            "delete",
            "--untitled",
            "00000000-0000-0000-0000-000000000001",
        ],
        vec!["session", "compress", "--untitled"],
        vec!["session", "decompress", "--untitled"],
        vec!["session", "compress", "--all", "--before", "2026-01-01"],
        vec!["session", "decompress", "bad"],
        vec!["session", "delete", "--before", "2026-01-01T00:00:00"],
    ] {
        let output = command(directory.path()).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!directory.path().join("chats").exists());
    }
}

#[test]
fn session_list_logs_build_info_and_resolved_store_at_debug_level() {
    let directory = tempfile::tempdir().unwrap();
    let log_path = directory.path().join("session-list.log");
    let output = command(directory.path())
        .args(["--log-level", "debug", "--log-file"])
        .arg(&log_path)
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let logs = std::fs::read_to_string(&log_path).unwrap();
    assert!(logs.contains("build info"), "{logs}");
    assert!(logs.contains("resolved session store"), "{logs}");
    assert!(logs.contains(&project_key(directory.path())), "{logs}");
}

#[test]
fn log_file_with_leading_tilde_is_written_under_home() {
    let directory = tempfile::tempdir().unwrap();
    let home_directory = tempfile::tempdir().unwrap();
    let output = command(directory.path())
        .env("HOME", home_directory.path())
        .args([
            "--log-level",
            "debug",
            "--log-file",
            "~/session-list.log",
            "session",
            "list",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(home_directory.path().join("session-list.log").exists());
    assert!(!directory.path().join("~").exists());
}

#[test]
fn compiled_cli_logs_session_chains_in_text_and_json_without_prompt_content() {
    for format in ["text", "json"] {
        let directory = tempfile::tempdir().unwrap();
        write_mock_profile(directory.path());
        let log_path = directory.path().join("session.log");
        let output = command(directory.path())
            .args([
                "--no-tui",
                "--prompt",
                "PRIVATE LOGGING PROMPT",
                "--log-level",
                "debug",
                "--log-format",
                format,
            ])
            .arg("--log-file")
            .arg(&log_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let identity = stderr
            .lines()
            .find_map(|line| line.strip_prefix("Session: "))
            .unwrap();
        let logs = std::fs::read_to_string(log_path).unwrap();
        assert!(!logs.contains("PRIVATE LOGGING PROMPT"));
        let command_line = logs
            .lines()
            .find(|line| line.contains("agent command received"))
            .unwrap();
        let writer_line = logs
            .lines()
            .find(|line| line.contains("session event persisted"))
            .unwrap();
        for line in [command_line, writer_line] {
            assert!(line.contains(identity), "{line}");
            if format == "json" {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(event["spans"][0]["session_id"], identity);
            }
        }
    }
}
#[tokio::test]
async fn listing_exact_output_uses_timestamp_instants_then_uuid_and_launch_scope() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    for identity in [uuid::Uuid::from_u128(2), uuid::Uuid::from_u128(1)] {
        let store = Store::create(
            &chats_root,
            directory.path(),
            identity,
            "mock",
            "mock",
            None,
        )
        .unwrap();
        store.flush().await;
        let path = store.path().to_path_buf();
        let mut metadata = Store::read_session_meta(&path).unwrap().unwrap();
        metadata.created_at = "2026-01-01T00:00:00Z".into();
        metadata.title = "one\nsafe\u{1b}line".into();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string(&ylva::StoreEvent::Meta(metadata)).unwrap()
            ),
        )
        .unwrap();
    }
    let output = command(directory.path())
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "2026-01-01T00:00:00Z {} one safe line\n2026-01-01T00:00:00Z {} one safe line\n",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2)
        )
    );
    let another_directory = directory.path().join("another");
    std::fs::create_dir(&another_directory).unwrap();
    let output = command(directory.path())
        .current_dir(another_directory)
        .args(["session", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn command_batches_preview_transform_report_noops_and_delete_without_yes() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let store = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let identity = store.session_id();
    let path = store.path().to_path_buf();
    store.flush().await;
    let bytes = std::fs::read(&path).unwrap();
    drop(store);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let preview = command(directory.path())
        .args(["session", "delete", &identity.to_string(), "--dry-run"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert_eq!(
        String::from_utf8(preview.stdout).unwrap(),
        format!("would delete {identity}\n")
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    for operation in ["compress", "compress", "decompress", "decompress"] {
        let output = command(directory.path())
            .args(["session", operation, "--all"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains(&identity.to_string()));
    }
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let output = command(directory.path())
        .args(["session", "delete", &identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("deleted {identity}\n")
    );
    assert!(!path.exists());
    assert!(path.with_extension("lock").exists());
}

#[tokio::test]
async fn deletion_and_previews_report_sessions_including_archived_descendants() {
    let directory = tempfile::tempdir().unwrap();
    let chats_root = directory.path().join("chats");
    let root = Store::create(
        &chats_root,
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "mock",
        None,
    )
    .unwrap();
    let subagent = root
        .start_new_session("mock", "mock", Some(root.session_id()), None)
        .unwrap();
    let root_identity = root.session_id();
    let subagent_identity = subagent.session_id();
    let root_path = root.path().to_path_buf();
    let subagent_path = subagent.path().to_path_buf();
    root.flush().await;
    subagent.flush().await;
    drop(root);
    drop(subagent);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let output = command(directory.path())
        .args(["session", "compress", &subagent_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let archive_path = subagent_path.with_extension("jsonl.gz");
    let metadata_path = subagent_path.with_extension("meta.json");
    let output = command(directory.path())
        .args(["session", "delete", &root_identity.to_string(), "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let paths = [root_path, archive_path, metadata_path];
    let mut identities = [root_identity, subagent_identity];
    identities.sort();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        identities
            .iter()
            .map(|identity| format!("would delete {identity}\n"))
            .collect::<String>()
    );
    assert!(paths.iter().all(|path| path.exists()));
    let output = command(directory.path())
        .args(["session", "delete", &root_identity.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        identities
            .iter()
            .map(|identity| format!("deleted {identity}\n"))
            .collect::<String>()
    );
    assert!(paths.iter().all(|path| !path.exists()));
}
