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

use super::super::STANDARD_BASE64;
use super::*;
use crate::test_support::capture_logs;
use base64::Engine as _;
use serde_json::Value;
use tempfile::TempDir;

/// Open `args.path` directly (bypassing the approval binding) and read it.
async fn read_file(args: ReadFileArgs) -> Result<ReadResult, ToolError> {
    read_file_streaming(args, STREAM_CHUNK_BYTES).await
}

/// As [`read_file`] with an explicit chunk size for chunk-boundary tests.
async fn read_file_streaming(
    args: ReadFileArgs,
    chunk_size: usize,
) -> Result<ReadResult, ToolError> {
    if args.encoding == FileEncoding::Base64 && args.mode != Some(ReadMode::Bytes) {
        return Err(ToolError::failed(
            "read",
            "`encoding: \"base64\"` requires explicit `mode: \"bytes\"`",
        ));
    }
    let file = tokio::fs::File::open(&args.path)
        .await
        .map_err(|error| read_io_error(&args.path, error))?;
    read_opened_file(args, file, chunk_size).await
}

#[test]
fn read_tool_with_timeout_constructor() {
    let tool = ReadTool::with_timeout(
        10,
        SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::sandbox::Sandbox::new(),
        ))),
    );
    assert_eq!(tool.timeout_secs(), 10);
}

#[test]
fn read_tool_auto_approves_workspace_path_and_has_default_timeout() {
    let tool = ReadTool::default();
    let args = serde_json::json!({ "path": "src/main.rs" });
    // Phase 6.0: reads inside the workspace are auto-approved by the sandbox;
    // the cwd is the workspace root in tests, so "src/main.rs" is in-workspace.
    assert!(matches!(
        tool.needs_approval(&args),
        crate::sandbox::ApprovalCheck::AutoApproved
    ));
    assert_eq!(tool.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
}

#[test]
fn read_tool_missing_path_is_auto_denied() {
    let tool = ReadTool::default();
    assert!(matches!(
        tool.needs_approval(&serde_json::json!({})),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
    assert!(matches!(
        tool.needs_approval(&serde_json::json!({ "path": "" })),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
    assert!(matches!(
        tool.needs_approval(&serde_json::json!({ "path": 42 })),
        crate::sandbox::ApprovalCheck::AutoDenied { .. }
    ));
}

#[cfg(unix)]
#[test]
fn read_tool_cross_filesystem_rule_requires_user_approval_for_cross_device_path() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule, SandboxRuleConfig};

    let workspace = TempDir::new().expect("tempdir");
    let context = SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
        crate::sandbox::Sandbox::with_root(workspace.path().to_path_buf()),
    )))
    .with_configured_rules(vec![SandboxRuleConfig::Named {
        rule: NamedSandboxRule::CrossFilesystem,
        verdict: ApprovalVerdictConfig::UserApprovalRequired,
        reason: None,
        target: None,
    }])
    .expect("valid sandbox fixture");
    let tool = ReadTool::new(context);

    assert_eq!(
        tool.needs_approval(&serde_json::json!({ "path": "/dev/null" })),
        ApprovalCheck::UserApprovalRequired,
        "a cross-device path under a configured cross_filesystem rule must \
         require user approval"
    );

    let same_device_path = workspace.path().join("notes.txt");
    assert_eq!(
        tool.needs_approval(&serde_json::json!({
            "path": same_device_path.to_string_lossy()
        })),
        ApprovalCheck::AutoApproved,
        "a same-device path does not match, so read auto-approves its \
         sandbox abstention"
    );
}

#[test]
fn undeclared_tool_timeout_uses_default() {
    assert_eq!(
        ReadTool::default().timeout_secs(),
        DEFAULT_TOOL_TIMEOUT_SECS
    );
}

#[test]
fn read_tool_schema_includes_mode_selector() {
    let schema = ReadTool::default().input_schema();
    let value = serde_json::to_value(&schema).expect("schema serializes");
    let required = value["required"].as_array().expect("required is an array");
    assert_eq!(required.len(), 1, "only `path` is required");
    assert_eq!(required[0], serde_json::json!("path"));
    let mode_any_of = value["properties"]["mode"]["anyOf"]
        .as_array()
        .expect("optional mode is a nullable anyOf");
    assert!(
        mode_any_of
            .iter()
            .any(|branch| branch["$ref"] == serde_json::json!("#/definitions/ReadMode")),
        "mode must reference the ReadMode definition: {mode_any_of:?}"
    );
    assert!(
        mode_any_of
            .iter()
            .any(|branch| branch["type"] == serde_json::json!("null")),
        "mode must accept null for autodetect: {mode_any_of:?}"
    );
    let variants = value["definitions"]["ReadMode"]["enum"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .expect("mode enum present");
    assert_eq!(variants, vec!["lines", "bytes", "chars"]);
}

#[test]
fn read_schema_accepts_explicit_null_mode_as_autodetect() {
    let tool = ReadTool::default();
    let arguments = serde_json::json!({ "path": "source.bin", "mode": null });
    assert!(tool.validate_args(&arguments).is_ok());
    let parsed: ReadFileArgs = serde_json::from_value(arguments).expect("arguments deserialize");
    assert_eq!(parsed.mode, None);
}

#[test]
fn read_schema_documents_offset_as_zero_based() {
    let schema = ReadTool::default().input_schema();
    let value = serde_json::to_value(&schema).expect("schema serializes");
    let offset_description = value["properties"]["offset"]["description"]
        .as_str()
        .expect("offset property description");
    assert!(
        offset_description.contains("0-based"),
        "offset schema must state its 0-based convention, got: {offset_description}"
    );
}

#[test]
fn read_schema_explains_autodetect_and_explicit_modes() {
    let tool = ReadTool::default();
    assert!(tool.description().contains("Omit `mode` to autodetect"));
    assert!(tool.description().contains("`bytes` for byte ranges"));
    assert!(tool.description().contains("`encoding` defaults to `utf8`"));
    assert!(tool
        .description()
        .contains(&format_kibibytes(READ_OUTPUT_LIMIT_BYTES)));
    assert!(tool
        .description()
        .contains(&format_mebibytes(READ_MEDIA_MAX_BYTES)));

    let schema = serde_json::to_value(tool.input_schema()).expect("schema serializes");
    let mode_description = schema["definitions"]["ReadMode"]["description"]
        .as_str()
        .expect("mode schema description");
    assert!(mode_description.contains("Omit `mode` to autodetect"));
    let encoding_description = schema["properties"]["encoding"]["description"]
        .as_str()
        .expect("encoding schema description");
    assert!(encoding_description.contains("`base64` requires `mode: bytes`"));
    assert!(encoding_description.contains("Autodetect"));
    assert_eq!(schema["properties"]["encoding"]["default"], "utf8");
    let variants = schema["definitions"]["FileEncoding"]["oneOf"]
        .as_array()
        .expect("encoding variants")
        .iter()
        .filter_map(|variant| variant["enum"][0].as_str())
        .collect::<Vec<_>>();
    assert_eq!(variants, vec!["utf8", "base64"]);
}

#[test]
fn runtime_token_budget_cannot_expand_the_model_supplied_read_limit() {
    crate::test_support::run_locked(async {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let path = temporary_directory.path().join("read.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\nepsilon\n").expect("write fixture");
        let call = ReadTool::default().create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": path.display().to_string(),
                "mode": "lines",
                "limit": 3
            }),
        );
        let mut call = call;
        call.set_runtime_output_token_limit(Some(100));

        let result = call.run().await.expect("read result");

        assert_eq!(result.text(), "alpha\nbeta\ngamma");
    });
}

#[test]
fn runtime_token_budget_caps_read_content_and_marks_it_truncated() {
    crate::test_support::run_locked(async {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let path = temporary_directory.path().join("read.txt");
        std::fs::write(&path, "alpha beta gamma delta epsilon").expect("write fixture");
        let mut call = ReadTool::default().create_call(
            uuid::Uuid::new_v4(),
            serde_json::json!({"path": path.display().to_string()}),
        );
        call.set_runtime_output_token_limit(Some(2));

        let result = call.run().await.expect("read result");

        assert!(crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&result.text()) <= 2);
        assert_eq!(result.metadata().expect("metadata")["truncated"], true);
    });
}

#[test]
fn read_args_omit_mode_to_select_autodetect() {
    let tool = ReadTool::default();
    let arguments = serde_json::json!({
        "path": "source.txt",
        "offset": 32,
        "limit": 2,
    });
    assert!(tool.validate_args(&arguments).is_ok());

    let parsed: ReadFileArgs = serde_json::from_value(arguments).expect("arguments deserialize");
    assert_eq!(parsed.mode, None, "omitted mode must select autodetect");
    assert_eq!(parsed.encoding, FileEncoding::Utf8);
}

#[test]
fn read_rejects_base64_without_explicit_bytes_mode() {
    let tool = ReadTool::default();
    let missing_mode = serde_json::json!({
        "path": "source.bin",
        "encoding": "base64",
    });
    let lines_mode = serde_json::json!({
        "path": "source.bin",
        "mode": "lines",
        "encoding": "base64",
    });
    let bytes_mode = serde_json::json!({
        "path": "source.bin",
        "mode": "bytes",
        "encoding": "base64",
    });

    let missing_mode_error = tool.validate_args(&missing_mode).expect_err("must reject");
    assert!(missing_mode_error.contains("requires explicit `mode: \"bytes\"`"));
    let lines_mode_error = tool.validate_args(&lines_mode).expect_err("must reject");
    assert!(lines_mode_error.contains("requires explicit `mode: \"bytes\"`"));
    assert!(tool.validate_args(&bytes_mode).is_ok());
}

#[cfg(unix)]
#[test]
fn read_tool_symlink_parent_traversal_requires_user_approval() {
    let outside = TempDir::new().expect("outside");
    let workspace = TempDir::new().expect("workspace");
    let nested = outside.path().join("nested");
    std::fs::create_dir(&nested).expect("mkdir");
    std::os::unix::fs::symlink(&nested, workspace.path().join("link")).expect("symlink");
    let context = SandboxContext::new(std::sync::Arc::new(std::sync::RwLock::new(
        crate::sandbox::Sandbox::with_root(workspace.path().to_path_buf()),
    )));
    let tool = ReadTool::new(context);

    assert_eq!(
        tool.needs_approval(&serde_json::json!({ "path": workspace.path().join("link/../secret.txt").display().to_string() })),
        ApprovalCheck::UserApprovalRequired,
        "`link/..` leaves the workspace and must not be judged lexically"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_refuses_symlink_swapped_after_approval() {
    let dir = TempDir::new().expect("tempdir");
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "secret").expect("seed secret");
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "harmless").expect("seed note");
    let call = ReadTool::default().create_call(
        uuid::Uuid::new_v4(),
        serde_json::json!({ "path": path.display().to_string() }),
    );

    std::fs::remove_file(&path).expect("remove note");
    std::os::unix::fs::symlink(&secret, &path).expect("plant symlink");

    let Err(error) = call.run().await else {
        panic!("swap must be refused");
    };
    assert!(error.to_string().contains("now resolves"), "{error}");
}

#[test]
fn read_omitted_mode_autodetects_text_as_lines_for_ranges() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
        let arguments = serde_json::json!({
            "path": path.display().to_string(),
            "offset": 1,
            "limit": 2,
        });
        let result = read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
            .await
            .expect("read ok");
        assert_eq!(result.text(), "two\nthree");
        assert_eq!(result.encoding, FileEncoding::Utf8);
    });
}

#[test]
fn read_omitted_mode_returns_base64_attachment_for_whole_media_file() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("image.png");
        let bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];
        std::fs::write(&path, &bytes).expect("write fixture");
        let arguments = serde_json::json!({ "path": path.display().to_string() });
        let result = read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
            .await
            .expect("autodetect media read succeeds");
        assert_eq!(result.encoding, FileEncoding::Base64);
        assert_eq!(result.media_type.as_deref(), Some(MEDIA_TYPE_IMAGE_PNG));
        assert_eq!(result.text(), STANDARD_BASE64.encode(&bytes));
        assert_eq!(result.attachments().len(), 1);
        assert_eq!(
            result.metadata(),
            Some(serde_json::json!({
                "read_bytes": bytes.len(),
                "encoding": "base64",
                "media_type": MEDIA_TYPE_IMAGE_PNG,
            }))
        );
    });
}

#[test]
fn read_omitted_mode_rejects_binary_without_recognized_media_type() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, [0x00, 0xff, 0x80, 0x01, 0xfe, b'Y']).expect("write fixture");
        let arguments = serde_json::json!({ "path": path.display().to_string() });
        let error = read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
            .await
            .expect_err("unrecognized binary must fail autodetect");
        let message = error.to_string();
        assert!(
            message.contains(r#"retry with explicit mode: "bytes""#),
            "error must carry the bytes-mode remedy: {message}"
        );
        assert!(
            message.contains(r#"encoding: "base64""#),
            "error must mention base64 for binary data: {message}"
        );
    });
}

#[test]
fn read_omitted_mode_rejects_ranged_reads_of_binary_files() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("image.png");
        std::fs::write(&path, [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0])
            .expect("write fixture");
        let arguments = serde_json::json!({
            "path": path.display().to_string(),
            "limit": 4,
        });
        let error = read_file(serde_json::from_value(arguments).expect("arguments deserialize"))
            .await
            .expect_err("ranged binary read must fail autodetect");
        let message = error.to_string();
        assert!(
            message.contains(r#"retry with explicit mode: "bytes""#),
            "error must carry the bytes-mode remedy: {message}"
        );
    });
}

#[test]
fn read_text_modes_reject_invalid_utf8() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("image.png");
        std::fs::write(&path, [137, 80, 78, 71, 0, 255]).expect("write");

        for mode in [Some(ReadMode::Lines), Some(ReadMode::Chars)] {
            let error = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            })
            .await
            .expect_err("invalid UTF-8 text reads must fail");
            assert!(
                error.to_string().contains("invalid UTF-8"),
                "unexpected error: {error}"
            );
            assert!(
                error.to_string().contains(r#"retry with mode: "bytes""#),
                "error must point at the bytes-mode remedy: {error}"
            );
            assert!(
                error.to_string().contains(r#"encoding: "base64""#),
                "error must mention base64 for binary data: {error}"
            );
        }
    });
}

#[test]
fn read_returns_whole_file_content() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello world").expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        })
        .await
        .expect("read ok");
        assert_eq!(result.text(), "hello world");
    });
}

#[test]
fn read_missing_file_is_error() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let result = read_file(ReadFileArgs {
            path: dir.path().join("nope.txt").display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        })
        .await;
        assert!(result.is_err());
    });
}

#[test]
fn read_bytes_honors_offset_and_limit() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "0123456789").expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset: Some(2),
            limit: Some(4),
        })
        .await
        .expect("read ok");
        assert_eq!(result.text(), "2345");
    });
}

#[test]
fn read_base64_preserves_raw_bytes_and_range_metadata() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("binary.dat");
        let bytes = [0x00, 0xff, 0x80, b'A', 0x00, 0xfe];
        std::fs::write(&path, bytes).expect("write");

        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: Some(1),
            limit: Some(4),
        })
        .await
        .expect("read ok");

        assert_eq!(result.text(), STANDARD_BASE64.encode(&bytes[1..5]));
        assert_eq!(
            result.metadata(),
            Some(serde_json::json!({
                "read_bytes": 4,
                "encoding": "base64",
            }))
        );
        let metadata_schema =
            serde_json::to_value(result.metadata_schema().expect("metadata schema"))
                .expect("schema serializes");
        assert!(metadata_schema["properties"].get("read_bytes").is_some());
        assert!(metadata_schema["properties"].get("read_lines").is_none());
        assert!(metadata_schema["properties"].get("encoding").is_some());
        assert!(metadata_schema["properties"].get("media_type").is_none());
        assert!(result.content_sections().is_empty());
    });
}

#[test]
fn read_base64_text_uses_text_metadata_and_preview() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("source.rs");
        let text = "fn main() {}\nfn helper() {}\n";
        std::fs::write(&path, text).expect("write");

        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: None,
            limit: None,
        })
        .await
        .expect("read ok");

        assert_eq!(result.text(), STANDARD_BASE64.encode(text));
        assert_eq!(
            result.metadata(),
            Some(serde_json::json!({
                "read_bytes": text.len(),
                "read_lines": 2,
                "encoding": "base64",
            }))
        );
        let metadata_schema =
            serde_json::to_value(result.metadata_schema().expect("metadata schema"))
                .expect("schema serializes");
        assert!(metadata_schema["properties"].get("read_lines").is_some());
        assert!(metadata_schema["properties"].get("encoding").is_some());
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1);
        let Section::Content(section) = &sections[0] else {
            panic!("expected a text content section");
        };
        assert_eq!(section.content, text);
        assert_eq!(section.file_extension.as_deref(), Some("rs"));
        assert_eq!(section.line_number_offset, Some(0));
    });
}

#[test]
fn read_base64_supports_invalid_utf8_without_logging_payload() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("binary.dat");
    let bytes = [
        0x00, 0xff, 0x80, b'R', b'E', b'A', b'D', b'-', b'B', b'I', b'N', b'A', b'R', b'Y', b'-',
        b'S', b'E', b'N', b'T', b'I', b'N', b'E', b'L',
    ];
    std::fs::write(&path, bytes).expect("write");
    let encoded = STANDARD_BASE64.encode(bytes);
    let log = capture_logs(async {
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: None,
            limit: None,
        })
        .await
        .expect("read ok");
        assert_eq!(result.text(), encoded);
    });
    assert!(
        log.contains("raw_selected_bytes=23"),
        "log must contain safe size: {log}"
    );
    assert!(
        !log.contains(&encoded),
        "Base64 content leaked into log: {log}"
    );
    assert!(
        !log.contains("READ-BINARY-SENTINEL"),
        "decoded content leaked into log: {log}"
    );
}

#[test]
fn read_base64_recognizes_supported_complete_media_signatures() {
    crate::test_support::run_locked(async {
        let directory = TempDir::new().expect("temporary directory");
        let cases = [
            (
                "image.png",
                vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0],
                MEDIA_TYPE_IMAGE_PNG,
            ),
            (
                "image.jpg",
                vec![255, 216, 255, 224, 0, 16],
                MEDIA_TYPE_IMAGE_JPEG,
            ),
            (
                "image.webp",
                vec![b'R', b'I', b'F', b'F', 0, 0, 0, 0, b'W', b'E', b'B', b'P'],
                MEDIA_TYPE_IMAGE_WEBP,
            ),
            (
                "image.gif",
                b"GIF89a\x00\x00".to_vec(),
                MEDIA_TYPE_IMAGE_GIF,
            ),
            (
                "report.pdf",
                b"%PDF-1.7\n".to_vec(),
                MEDIA_TYPE_APPLICATION_PDF,
            ),
        ];

        for (filename, bytes, media_type) in cases {
            let path = directory.path().join(filename);
            std::fs::write(&path, &bytes).expect("write fixture");
            let result = read_file(ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            })
            .await
            .expect("read fixture");

            assert_eq!(result.media_type.as_deref(), Some(media_type));
            assert_eq!(
                result.metadata().expect("metadata")["media_type"],
                media_type
            );
            assert_eq!(result.attachments().len(), 1);
            match result.attachments().first().expect("attachment") {
                ToolAttachment::Image { media_type: actual } => {
                    assert_eq!(actual, media_type)
                }
                ToolAttachment::File {
                    media_type: actual,
                    filename: actual_filename,
                } => {
                    assert_eq!(actual, media_type);
                    assert_eq!(actual_filename, filename);
                }
            }
            if result.text_preview.is_none() {
                assert!(result.content_sections().is_empty());
            }
        }
    });
}

#[test]
fn read_base64_does_not_attach_partial_or_false_positive_signatures() {
    crate::test_support::run_locked(async {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("looks-like.png");
        let bytes = [137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
        std::fs::write(&path, bytes).expect("write fixture");

        let partial = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: Some(0),
            limit: Some(8),
        })
        .await
        .expect("partial read");
        assert_eq!(partial.media_type, None);
        assert!(partial.attachments().is_empty());
        assert_eq!(partial.metadata().expect("metadata")["encoding"], "base64");

        let false_positive_path = directory.path().join("not-an-image.png");
        std::fs::write(&false_positive_path, b"not an image").expect("write fixture");
        let false_positive = read_file(ReadFileArgs {
            path: false_positive_path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: None,
            limit: None,
        })
        .await
        .expect("false-positive read");
        assert_eq!(false_positive.media_type, None);
        assert!(false_positive.attachments().is_empty());
    });
}

#[test]
fn read_pdf_attachment_uses_a_safe_fallback_filename() {
    let result = ReadResult {
        content: STANDARD_BASE64.encode(b"%PDF-1.7"),
        path: "/".to_string(),
        starting_line: None,
        encoding: FileEncoding::Base64,
        read_bytes: 8,
        source_size_bytes: 8,
        source_lines: None,
        text_preview: None,
        media_type: Some(MEDIA_TYPE_APPLICATION_PDF.to_string()),
        truncated: false,
    };
    assert_eq!(
        result.attachments(),
        vec![ToolAttachment::File {
            media_type: MEDIA_TYPE_APPLICATION_PDF.to_string(),
            filename: "attachment.pdf".to_string(),
        }]
    );
}

#[test]
fn read_call_rejects_incompatible_encoding_before_file_io() {
    crate::test_support::run_locked(async {
        let call = ReadTool::default().create_call(
            uuid::Uuid::nil(),
            serde_json::json!({
                "path": "does-not-exist.bin",
                "encoding": "base64",
            }),
        );
        let error = call.run().await.expect_err("incompatible call must fail");
        assert!(
            error
                .to_string()
                .contains("requires explicit `mode: \"bytes\"`"),
            "unexpected error: {error}"
        );
    });
}

#[test]
fn read_lines_honors_offset_and_limit() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Lines),
            encoding: FileEncoding::Utf8,
            offset: Some(1),
            limit: Some(2),
        })
        .await
        .expect("read ok");
        assert_eq!(result.text(), "two\nthree");
    });
}

#[test]
fn read_chars_counts_utf8_code_points() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        // `é` is 2 bytes but 1 UTF-8 code point.
        std::fs::write(&path, "héllo").expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Chars),
            encoding: FileEncoding::Utf8,
            offset: Some(1),
            limit: Some(3),
        })
        .await
        .expect("read ok");
        assert_eq!(result.text(), "éll");
    });
}

#[test]
fn read_tool_orders_path_first_and_marks_its_defaults() {
    let tool = ReadTool::default();
    assert_eq!(tool.argument_order("path"), 0);
    assert!(tool.argument_order("mode") < tool.argument_order("offset"));
    assert!(tool.is_default_argument("mode", &Value::Null));
    assert!(!tool.is_default_argument("mode", &Value::from("lines")));
    assert!(!tool.is_default_argument("mode", &Value::from("bytes")));
    assert!(tool.is_default_argument("offset", &Value::Null));
    assert!(tool.is_default_argument("offset", &Value::from(0)));
    assert!(!tool.is_default_argument("offset", &Value::from(3)));
    assert!(tool.is_default_argument("limit", &Value::Null));
    assert!(!tool.is_default_argument("limit", &Value::from(5)));
}

#[test]
fn read_tool_non_default_arguments_drop_defaults() {
    let tool = ReadTool::default();
    let args = serde_json::json!({
        "path": "a.txt",
        "mode": null,
        "offset": 0,
        "limit": 3,
    });
    let keys: Vec<&str> = tool
        .non_default_arguments(&args)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(keys, vec!["path", "limit"]);
}

/// A `tracing` log target that appends to an in-memory buffer
/// synchronously in the calling thread, plus a `capture_logs` helper that
/// runs an async body under it; both live in shared test-support
/// (`src/test_support.rs`), SSOT.
#[test]
fn read_file_logs_start_and_result_size() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "hello world").expect("seed file");
    let log = capture_logs(async {
        let call = ReadTool::default().create_call(
            uuid::Uuid::nil(),
            serde_json::json!({ "path": path.display().to_string() }),
        );
        let result = call.run().await;
        assert!(result.is_ok());
    });
    assert!(
        log.contains("reading file"),
        "read_file must log its start: {log}"
    );
    assert!(log.contains("read file"), "read_file must log: {log}");
    assert!(
        log.contains("source_size_bytes=11") && log.contains("raw_selected_bytes=11"),
        "read_file must distinguish source and returned sizes: {log}"
    );
    assert!(
        log.contains("source_lines=Some(1)") && log.contains("raw_selected_lines=Some(1)"),
        "read_file must distinguish source and returned line counts: {log}"
    );
}

#[test]
fn read_log_distinguishes_full_source_from_capped_returned_text() {
    let directory = TempDir::new().expect("tempdir");
    let path = directory.path().join("large.txt");
    let source_text = format!("{}\n{}", "a".repeat(READ_OUTPUT_LIMIT_BYTES), "tail");
    std::fs::write(&path, &source_text).expect("write fixture");
    let log = capture_logs(async {
        let call = ReadTool::default().create_call(
            uuid::Uuid::nil(),
            serde_json::json!({ "path": path.display().to_string() }),
        );
        let result = call.run().await.expect("read succeeds");
        assert_eq!(result.metadata().expect("metadata")["truncated"], true);
    });

    assert!(
        log.contains(&format!("source_size_bytes={}", source_text.len())),
        "log includes the full source byte size: {log}"
    );
    assert!(
        log.contains("source_lines=Some(2)"),
        "log includes the full source line count: {log}"
    );
    assert!(
        log.contains(&format!("raw_selected_bytes={READ_OUTPUT_LIMIT_BYTES}")),
        "log includes the capped returned byte size: {log}"
    );
    assert!(
        log.contains("raw_selected_lines=Some(1)") && log.contains("truncated=true"),
        "log includes returned lines and the truncation flag: {log}"
    );
}

#[test]
fn read_tool_run_records_non_default_arguments_in_span() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "hello world").expect("seed file");
    let log = capture_logs(async {
        let call = ReadTool::default().create_call(
            uuid::Uuid::nil(),
            serde_json::json!({
                "path": path.display().to_string(),
                "mode": "bytes",
                "offset": 0,
            }),
        );
        let result = call.run().await;
        assert!(result.is_ok());
    });
    assert!(
        log.contains("tool_args"),
        "run must open an args span: {log}"
    );
    assert!(
        log.contains("\"path\""),
        "the non-default path argument must be recorded: {log}"
    );
}

#[test]
fn read_non_default_redacted_arguments_drop_defaults_and_keep_path() {
    let tool = ReadTool::default();
    let arguments = serde_json::json!({
        "path": "a.txt",
        "mode": null,
        "offset": 0,
        "limit": serde_json::Value::Null,
    });
    let redacted = tool.non_default_redacted_arguments(&arguments);
    let keys: Vec<&str> = redacted.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, vec!["path"]);
}

#[test]
fn read_tool_done_content_sections_have_line_numbers() {
    crate::test_support::run_locked(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, "fn main() {}\nfn foo() {}\n").unwrap();
        let result = read_file(ReadFileArgs {
            path: path.to_str().unwrap().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        })
        .await
        .unwrap();
        let sections = result.content_sections();
        assert_eq!(sections.len(), 1, "expected one body section");
        let section = &sections[0];
        let content_section = match section {
            super::Section::Content(cs) => cs,
            other => panic!("expected Section::Content, got {other:?}"),
        };
        assert_eq!(
            content_section.line_number_offset,
            Some(0),
            "bytes mode must carry line_number_offset Some(0)"
        );
        assert_eq!(content_section.file_extension.as_deref(), Some("rs"));
    });
}

#[test]
fn read_result_empty_content_has_no_sections() {
    let result = ReadResult {
        content: String::new(),
        path: "empty.txt".to_string(),
        starting_line: None,
        encoding: FileEncoding::Utf8,
        read_bytes: 0,
        source_size_bytes: 0,
        source_lines: Some(0),
        text_preview: None,
        media_type: None,
        truncated: false,
    };
    assert!(result.content_sections().is_empty());
}

#[test]
fn read_call_tool_call_methods() {
    let id = uuid::Uuid::new_v4();
    let args = serde_json::json!({ "path": "src/main.rs" });
    let call = ReadTool::default().create_call(id, args.clone());
    assert_eq!(call.id(), id);
    assert_eq!(call.name(), "read");
    assert_eq!(call.arguments(), &args);
    assert_eq!(call.timeout_secs(), DEFAULT_TOOL_TIMEOUT_SECS);
    assert_eq!(call.argument_order("path"), 0);
    let redacted = call.non_default_redacted_arguments();
    assert!(redacted.iter().any(|(key, _)| key == "path"));
}

#[test]
fn read_input_args_all_properties_have_descriptions() {
    let schema = ReadTool::default().input_schema();
    crate::test_support::assert_all_properties_have_descriptions(&schema, "ReadFileArgs");
}

// ── Range-boundary characterization tests ─────────────────────────────────
//
// These pin the *current* selection semantics at the boundaries of
// `offset`/`limit`: past EOF, `limit: 0`, and ranges that cross EOF.

const BOUNDARY_TEXT: &str = "one\ntwo\nthree";

#[test]
fn read_bytes_utf8_range_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, BOUNDARY_TEXT).expect("write");

        let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset,
            limit,
        };

        let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
        assert_eq!(offset_past_eof.text(), "");
        assert_eq!(offset_past_eof.read_bytes, 0);
        assert_eq!(offset_past_eof.starting_line, Some(0));

        let offset_past_eof_no_limit = read_file(read(Some(100), None)).await.expect("read");
        assert_eq!(offset_past_eof_no_limit.text(), "");
        assert_eq!(offset_past_eof_no_limit.read_bytes, 0);

        let limit_zero = read_file(read(None, Some(0))).await.expect("read");
        assert_eq!(limit_zero.text(), "");
        assert_eq!(limit_zero.read_bytes, 0);

        let crossing_eof = read_file(read(Some(11), Some(5))).await.expect("read");
        assert_eq!(crossing_eof.text(), "ee");
        assert_eq!(crossing_eof.read_bytes, 2);
        assert_eq!(crossing_eof.starting_line, Some(0));
    });
}

#[test]
fn read_bytes_base64_range_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, BOUNDARY_TEXT).expect("write");

        let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset,
            limit,
        };

        let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
        assert_eq!(offset_past_eof.text(), "");
        assert_eq!(offset_past_eof.read_bytes, 0);
        assert_eq!(offset_past_eof.starting_line, Some(0));
        assert_eq!(
            offset_past_eof.metadata(),
            Some(serde_json::json!({
                "read_bytes": 0,
                "read_lines": 0,
                "encoding": "base64",
            }))
        );

        let limit_zero = read_file(read(None, Some(0))).await.expect("read");
        assert_eq!(limit_zero.text(), "");
        assert_eq!(limit_zero.read_bytes, 0);
        assert!(limit_zero.content_sections().is_empty());

        let crossing_eof = read_file(read(Some(11), Some(5))).await.expect("read");
        assert_eq!(
            crossing_eof.text(),
            STANDARD_BASE64.encode(&BOUNDARY_TEXT.as_bytes()[11..])
        );
        assert_eq!(crossing_eof.read_bytes, 2);
        assert_eq!(crossing_eof.starting_line, Some(0));
    });
}

#[test]
fn read_lines_range_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, BOUNDARY_TEXT).expect("write");

        let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Lines),
            encoding: FileEncoding::Utf8,
            offset,
            limit,
        };

        let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
        assert_eq!(offset_past_eof.text(), "");
        assert_eq!(offset_past_eof.read_bytes, 0);
        assert_eq!(offset_past_eof.starting_line, Some(3));

        let limit_zero = read_file(read(None, Some(0))).await.expect("read");
        assert_eq!(limit_zero.text(), "");
        assert_eq!(limit_zero.read_bytes, 0);
        assert_eq!(limit_zero.starting_line, Some(0));

        let limit_zero_after_offset = read_file(read(Some(2), Some(0))).await.expect("read");
        assert_eq!(limit_zero_after_offset.text(), "");
        assert_eq!(limit_zero_after_offset.read_bytes, 0);
        assert_eq!(limit_zero_after_offset.starting_line, Some(2));

        let crossing_eof = read_file(read(Some(1), Some(10))).await.expect("read");
        assert_eq!(crossing_eof.text(), "two\nthree");
        assert_eq!(crossing_eof.read_bytes, 9);
        assert_eq!(crossing_eof.starting_line, Some(1));
    });
}

#[test]
fn read_chars_range_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "héllo").expect("write");

        let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Chars),
            encoding: FileEncoding::Utf8,
            offset,
            limit,
        };

        let offset_past_eof = read_file(read(Some(10), None)).await.expect("read");
        assert_eq!(offset_past_eof.text(), "");
        assert_eq!(offset_past_eof.read_bytes, 0);
        assert_eq!(offset_past_eof.starting_line, Some(0));

        let limit_zero = read_file(read(None, Some(0))).await.expect("read");
        assert_eq!(limit_zero.text(), "");
        assert_eq!(limit_zero.read_bytes, 0);
        assert_eq!(limit_zero.starting_line, Some(0));

        let crossing_eof = read_file(read(Some(3), Some(10))).await.expect("read");
        assert_eq!(crossing_eof.text(), "lo");
        assert_eq!(crossing_eof.read_bytes, 2);
        assert_eq!(crossing_eof.starting_line, Some(0));
    });
}

#[test]
fn read_autodetect_text_range_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, BOUNDARY_TEXT).expect("write");

        let read = |offset: Option<usize>, limit: Option<usize>| ReadFileArgs {
            path: path.display().to_string(),
            mode: None,
            encoding: FileEncoding::Utf8,
            offset,
            limit,
        };

        let offset_past_eof = read_file(read(Some(20), None)).await.expect("read");
        assert_eq!(offset_past_eof.text(), "");
        assert_eq!(offset_past_eof.encoding, FileEncoding::Utf8);
        assert_eq!(offset_past_eof.read_bytes, 0);
        assert_eq!(offset_past_eof.starting_line, Some(3));

        let limit_zero = read_file(read(None, Some(0))).await.expect("read");
        assert_eq!(limit_zero.text(), "");
        assert_eq!(limit_zero.read_bytes, 0);
        assert_eq!(limit_zero.starting_line, Some(0));

        let crossing_eof = read_file(read(Some(1), Some(10))).await.expect("read");
        assert_eq!(crossing_eof.text(), "two\nthree");
        assert_eq!(crossing_eof.read_bytes, 9);
        assert_eq!(crossing_eof.starting_line, Some(1));
    });
}

// ── Chunk-boundary tests ──────────────────────────────────────────────────
//
// `read_file_streaming` takes the chunk size as a parameter so these stay
// fast and deterministic: no large fixtures, no timing assertions.

/// A line that starts in one chunk and ends in another is reassembled.
#[test]
fn read_lines_reassembles_lines_split_across_chunks() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "aaa\nbbb\nccc").expect("write");
        let arguments = ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Lines),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        };

        let whole = read_file_streaming(arguments.clone(), 6)
            .await
            .expect("read");
        assert_eq!(whole.text(), "aaa\nbbb\nccc");
        assert_eq!(whole.starting_line, Some(0));

        let only_second = read_file_streaming(
            ReadFileArgs {
                offset: Some(1),
                limit: Some(1),
                ..arguments.clone()
            },
            6,
        )
        .await
        .expect("read");
        assert_eq!(only_second.text(), "bbb");
        assert_eq!(only_second.starting_line, Some(1));

        let only_first = read_file_streaming(
            ReadFileArgs {
                offset: None,
                limit: Some(1),
                ..arguments
            },
            6,
        )
        .await
        .expect("read");
        assert_eq!(only_first.text(), "aaa");
    });
}

/// A chunk size of one byte splits every multi-byte sequence.
#[test]
fn read_chars_counts_code_points_split_across_chunks() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        // `é` is 2 bytes, `😀` is 4 bytes, both split by chunk size 1.
        std::fs::write(&path, "aé😀z").expect("write");
        let arguments = ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Chars),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        };

        let whole = read_file_streaming(arguments.clone(), 1)
            .await
            .expect("read");
        assert_eq!(whole.text(), "aé😀z");
        assert_eq!(whole.read_bytes, "aé😀z".len());

        let window = read_file_streaming(
            ReadFileArgs {
                offset: Some(1),
                limit: Some(2),
                ..arguments
            },
            1,
        )
        .await
        .expect("read");
        assert_eq!(window.text(), "é😀");
        assert_eq!(window.read_bytes, "é😀".len());
    });
}

/// Invalid UTF-8 is reported with its absolute file offset, whether the
/// bad byte starts a chunk or arrives as part of a carried sequence.
#[test]
fn read_lines_reports_invalid_utf8_at_chunk_boundaries() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");

        let starts_chunk = dir.path().join("starts-chunk.txt");
        std::fs::write(&starts_chunk, [b'a', b'b', b'c', 0xff, b'd']).expect("write");
        let error = read_file_streaming(
            ReadFileArgs {
                path: starts_chunk.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            3,
        )
        .await
        .expect_err("invalid UTF-8 must fail");
        assert!(
            error
                .to_string()
                .contains("invalid utf-8 sequence of 1 bytes from index 3"),
            "unexpected error: {error}"
        );

        let carried = dir.path().join("carried.txt");
        std::fs::write(&carried, [b'a', 0xc3, 0x28, b'b']).expect("write");
        let error = read_file_streaming(
            ReadFileArgs {
                path: carried.display().to_string(),
                mode: Some(ReadMode::Chars),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            2,
        )
        .await
        .expect_err("invalid UTF-8 must fail");
        assert!(
            error
                .to_string()
                .contains("invalid utf-8 sequence of 1 bytes from index 1"),
            "unexpected error: {error}"
        );

        let trailing = dir.path().join("trailing.txt");
        std::fs::write(&trailing, [b'a', 0xc3]).expect("write");
        let error = read_file_streaming(
            ReadFileArgs {
                path: trailing.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            1,
        )
        .await
        .expect_err("truncated sequence must fail");
        assert!(
            error
                .to_string()
                .contains("incomplete utf-8 byte sequence from index 1"),
            "unexpected error: {error}"
        );
    });
}

#[test]
fn read_autodetect_keeps_utf8_first_ordering_and_sniffs_the_first_chunk() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");

        // ASCII `%PDF…` is valid UTF-8, so it stays a text result.
        let pdf_text = dir.path().join("ascii.pdf.txt");
        std::fs::write(&pdf_text, b"%PDF-1.7\n42\n").expect("write");
        let text_result = read_file_streaming(
            ReadFileArgs {
                path: pdf_text.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            4,
        )
        .await
        .expect("ASCII PDF header is text");
        assert_eq!(text_result.encoding, FileEncoding::Utf8);
        assert_eq!(text_result.text(), "%PDF-1.7\n42");
        assert_eq!(text_result.media_type, None);

        // The GIF signature is ASCII, so classification completes from the
        // first chunk even though a later chunk is invalid UTF-8.
        let gif = dir.path().join("late-binary.gif");
        let mut gif_bytes = b"GIF89a".to_vec();
        gif_bytes.extend(std::iter::repeat_n(b'x', 40));
        gif_bytes.push(0xff);
        gif_bytes.extend(std::iter::repeat_n(b'y', 10));
        std::fs::write(&gif, &gif_bytes).expect("write");
        let media_result = read_file_streaming(
            ReadFileArgs {
                path: gif.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            8,
        )
        .await
        .expect("media classification from the first chunk");
        assert_eq!(media_result.encoding, FileEncoding::Base64);
        assert_eq!(
            media_result.media_type.as_deref(),
            Some(MEDIA_TYPE_IMAGE_GIF)
        );
        assert_eq!(media_result.text(), STANDARD_BASE64.encode(&gif_bytes));
    });
}

#[test]
fn read_autodetect_streams_only_the_selected_window() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour").expect("write");
        let result = read_file_streaming(
            ReadFileArgs {
                path: path.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: Some(2),
                limit: Some(1),
            },
            3,
        )
        .await
        .expect("read");
        assert_eq!(result.text(), "three");
        assert_eq!(result.starting_line, Some(2));
    });
}

// ── Output-cap tests ──────────────────────────────────────────────────────

/// Fixture just over [`READ_OUTPUT_LIMIT_BYTES`].
fn over_cap_text_bytes() -> Vec<u8> {
    let mut bytes = vec![b'a'; READ_OUTPUT_LIMIT_BYTES + 1024];
    bytes[READ_OUTPUT_LIMIT_BYTES / 2] = b'\n';
    bytes
}

/// Assert the shared truncation contract: content cut at the cap, the
/// model-facing `truncated: true` metadata, the schema property kept only
/// when present, and the `Partial output` section label.
fn assert_truncated_read(result: &ReadResult, expected_bytes: usize) {
    assert!(result.truncated, "result must be flagged truncated");
    assert_eq!(result.read_bytes, expected_bytes);
    assert!(
        result.read_bytes <= READ_OUTPUT_LIMIT_BYTES,
        "raw selected bytes must not exceed the cap"
    );
    let metadata = result.metadata().expect("metadata");
    assert_eq!(
        metadata["truncated"],
        serde_json::json!(true),
        "metadata must report truncation: {metadata}"
    );
    let schema = serde_json::to_value(result.metadata_schema().expect("metadata schema"))
        .expect("schema serializes");
    assert!(
        schema["properties"].get("truncated").is_some(),
        "truncated must stay in the schema for a truncated result: {schema}"
    );
    // Base64 results without a UTF-8 projection carry no section at all.
    if let Some(Section::Content(section)) = result.content_sections().first() {
        assert_eq!(
            section.label.as_deref(),
            Some("Partial output"),
            "truncated content must be labeled Partial output"
        );
    }
}

#[test]
fn read_caps_text_results_at_the_output_limit() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("large.txt");
        let bytes = over_cap_text_bytes();
        std::fs::write(&path, &bytes).expect("write");
        let expected_bytes = READ_OUTPUT_LIMIT_BYTES;

        let cases = [
            ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Lines),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Chars),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            ReadFileArgs {
                path: path.display().to_string(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
            ReadFileArgs {
                path: path.display().to_string(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
        ];
        for arguments in cases {
            let result = read_file(arguments).await.expect("read");
            assert_eq!(
                result.content_sections().len(),
                1,
                "capped text results keep one content section"
            );
            assert_truncated_read(&result, expected_bytes);
        }
    });
}

#[test]
fn read_output_cap_beats_an_oversized_explicit_limit() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("large.txt");
        std::fs::write(&path, vec![b'b'; READ_OUTPUT_LIMIT_BYTES + 4096]).expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: Some(READ_OUTPUT_LIMIT_BYTES + 4096),
        })
        .await
        .expect("read");
        assert_truncated_read(&result, READ_OUTPUT_LIMIT_BYTES);

        let lines_result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Lines),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: Some(usize::MAX),
        })
        .await
        .expect("read");
        assert_truncated_read(&lines_result, READ_OUTPUT_LIMIT_BYTES);
    });
}

#[test]
fn read_base64_text_is_truncated_at_the_output_limit() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("large.txt");
        let bytes = over_cap_text_bytes();
        std::fs::write(&path, &bytes).expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: None,
            limit: None,
        })
        .await
        .expect("read");

        assert_truncated_read(&result, READ_OUTPUT_LIMIT_BYTES);
        assert_eq!(
            result.text(),
            STANDARD_BASE64.encode(&bytes[..READ_OUTPUT_LIMIT_BYTES])
        );
        assert_eq!(result.media_type, None, "truncated is never complete");
        assert!(result.attachments().is_empty());
    });
}

#[test]
fn read_recognized_media_just_over_the_output_limit_stays_complete() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let mut png = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
        png.extend(std::iter::repeat_n(
            0u8,
            READ_OUTPUT_LIMIT_BYTES + 1024 - png.len(),
        ));
        let path = dir.path().join("large.png");
        std::fs::write(&path, &png).expect("write");

        let explicit = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Bytes),
            encoding: FileEncoding::Base64,
            offset: None,
            limit: None,
        })
        .await
        .expect("complete media read");
        assert!(!explicit.truncated, "media reads bypass the cap");
        assert_eq!(explicit.read_bytes, png.len());
        assert_eq!(
            explicit.media_type.as_deref(),
            Some(MEDIA_TYPE_IMAGE_PNG),
            "complete media stays attached past the cap"
        );
        assert_eq!(explicit.attachments().len(), 1);
        let metadata = explicit.metadata().expect("metadata");
        assert!(
            metadata.get("truncated").is_none(),
            "untruncated metadata must omit truncated: {metadata}"
        );

        let autodetected = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: None,
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        })
        .await
        .expect("autodetected media read");
        assert!(!autodetected.truncated);
        assert_eq!(
            autodetected.media_type.as_deref(),
            Some(MEDIA_TYPE_IMAGE_PNG)
        );
        assert_eq!(autodetected.read_bytes, png.len());
    });
}

#[test]
fn read_complete_media_over_the_size_ceiling_is_rejected() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
        bytes.resize(READ_MEDIA_MAX_BYTES + 1, 0);
        let path = dir.path().join("huge.png");
        std::fs::write(&path, &bytes).expect("write");
        let path_string = path.display().to_string();

        let cases = [
            ReadFileArgs {
                path: path_string.clone(),
                mode: Some(ReadMode::Bytes),
                encoding: FileEncoding::Base64,
                offset: None,
                limit: None,
            },
            ReadFileArgs {
                path: path_string.clone(),
                mode: None,
                encoding: FileEncoding::Utf8,
                offset: None,
                limit: None,
            },
        ];
        for arguments in cases {
            let error = read_file(arguments)
                .await
                .expect_err("an oversized complete media read must fail");
            let message = error.to_string();
            assert!(
                message.contains(&format!("over the {}-byte limit", READ_MEDIA_MAX_BYTES)),
                "error must state the ceiling: {message}"
            );
            assert!(
                message.contains(&path_string),
                "error must name the path: {message}"
            );
            assert!(
                message.contains(r#"mode: "bytes""#),
                "error must carry the range-read remedy: {message}"
            );
        }
    });
}

#[test]
fn read_untruncated_result_omits_truncated_metadata_and_section_label() {
    crate::test_support::run_locked(async {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "one\ntwo").expect("write");
        let result = read_file(ReadFileArgs {
            path: path.display().to_string(),
            mode: Some(ReadMode::Lines),
            encoding: FileEncoding::Utf8,
            offset: None,
            limit: None,
        })
        .await
        .expect("read");

        assert!(!result.truncated);
        let metadata = result.metadata().expect("metadata");
        assert!(
            metadata.get("truncated").is_none(),
            "untruncated metadata must omit truncated: {metadata}"
        );
        let schema = serde_json::to_value(result.metadata_schema().expect("metadata schema"))
            .expect("schema serializes");
        assert!(
            schema["properties"].get("truncated").is_none(),
            "truncated must be pruned from the schema when absent: {schema}"
        );
        let sections = result.content_sections();
        let Section::Content(section) = &sections[0] else {
            panic!("expected a text content section");
        };
        assert_eq!(
            section.label.as_deref(),
            Some("Output"),
            "uncapped reads label their output Result"
        );
    });
}
