use std::sync::{Arc, RwLock};

use base64::Engine as _;
use tempfile::TempDir;
use ylva::{default_registry, AutoApproveTool, Sandbox, ToolRegistry, ToolRegistryConfig};

fn binary_registry(workspace: &TempDir) -> ToolRegistry {
    let mut config = ToolRegistryConfig::default();
    config.auto_approve.insert(AutoApproveTool::Write);
    default_registry(
        Arc::new(RwLock::new(Sandbox::with_root(
            workspace.path().to_path_buf(),
        ))),
        config,
    )
}

#[tokio::test]
async fn registry_round_trips_binary_and_text_calls() {
    let workspace = TempDir::new().expect("temporary workspace");
    let registry = binary_registry(&workspace);
    let binary_path = workspace.path().join("binary.dat");
    let binary_bytes = vec![0x00, 0xff, 0x80, 0x01, 0xfe, b'Y'];
    let binary_content = base64::engine::general_purpose::STANDARD.encode(&binary_bytes);

    let write_call = registry
        .create_call(
            "write",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": binary_path,
                "content": binary_content,
                "encoding": "base64",
            }),
        )
        .expect("write is registered");
    let write_result = write_call.run().await.expect("binary write succeeds");
    assert_eq!(
        write_result.metadata(),
        Some(serde_json::json!({ "bytes_written": binary_bytes.len() }))
    );
    assert!(write_result.content_sections().is_empty());
    assert_eq!(
        std::fs::read(&binary_path).expect("binary file exists"),
        binary_bytes
    );

    let read_call = registry
        .create_call(
            "read",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": binary_path,
                "mode": "bytes",
                "encoding": "base64",
            }),
        )
        .expect("read is registered");
    let read_result = read_call.run().await.expect("binary read succeeds");
    assert_eq!(read_result.text(), binary_content);
    assert_eq!(
        read_result.metadata(),
        Some(serde_json::json!({ "read_bytes": binary_bytes.len() }))
    );
    assert!(read_result.content_sections().is_empty());

    let text_binary_path = workspace.path().join("text-through-base64.txt");
    let text_binary = "one\ntwo";
    let text_binary_content = base64::engine::general_purpose::STANDARD.encode(text_binary);
    let text_binary_write_call = registry
        .create_call(
            "write",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": text_binary_path,
                "content": text_binary_content,
                "encoding": "base64",
            }),
        )
        .expect("write is registered");
    let text_binary_write_result = text_binary_write_call
        .run()
        .await
        .expect("text Base64 write succeeds");
    assert_eq!(text_binary_write_result.content_sections().len(), 1);

    let text_binary_read_call = registry
        .create_call(
            "read",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": text_binary_path,
                "mode": "bytes",
                "encoding": "base64",
            }),
        )
        .expect("read is registered");
    let text_binary_read_result = text_binary_read_call
        .run()
        .await
        .expect("text Base64 read succeeds");
    assert_eq!(text_binary_read_result.text(), text_binary_content);
    assert_eq!(
        text_binary_read_result.metadata(),
        Some(serde_json::json!({
            "read_bytes": text_binary.len(),
            "read_lines": 2,
        }))
    );
    assert_eq!(text_binary_read_result.content_sections().len(), 1);

    let text_path = workspace.path().join("text.txt");
    let text_write_call = registry
        .create_call(
            "write",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": text_path,
                "content": "one\ntwo",
            }),
        )
        .expect("text write is registered");
    let text_write_result = text_write_call.run().await.expect("text write succeeds");
    assert_eq!(
        text_write_result.text(),
        format!("wrote 7 bytes to '{}'", text_path.display())
    );
    assert_eq!(text_write_result.content_sections().len(), 1);

    let text_read_call = registry
        .create_call(
            "read",
            uuid::Uuid::new_v4(),
            serde_json::json!({ "path": text_path }),
        )
        .expect("text read is registered");
    let text_read_result = text_read_call.run().await.expect("text read succeeds");
    assert_eq!(text_read_result.text(), "one\ntwo");
    assert_eq!(
        text_read_result.metadata(),
        Some(serde_json::json!({
            "read_bytes": 7,
            "read_lines": 2,
        }))
    );
    assert_eq!(text_read_result.content_sections().len(), 1);
}

#[tokio::test]
async fn invalid_binary_write_returns_public_error_without_mutating_target() {
    let workspace = TempDir::new().expect("temporary workspace");
    let registry = binary_registry(&workspace);
    let target = workspace.path().join("existing.bin");
    let original = b"original bytes";
    std::fs::write(&target, original).expect("seed target");

    let call = registry
        .create_call(
            "write",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": target,
                "content": "INVALID-BASE64!",
                "encoding": "base64",
            }),
        )
        .expect("write is registered");
    let error = call.run().await.expect_err("invalid Base64 must fail");
    match error {
        ylva::ToolError::Failed { name, message, .. } => {
            assert_eq!(name, "write");
            assert!(message.contains("failed to decode Base64 content"));
            assert!(message.contains("existing.bin"));
        }
    }
    assert_eq!(std::fs::read(&target).expect("target remains"), original);
}

#[tokio::test]
async fn binary_read_in_text_mode_returns_public_error() {
    let workspace = TempDir::new().expect("temporary workspace");
    let registry = binary_registry(&workspace);
    let path = workspace.path().join("image.png");
    std::fs::write(&path, [137, 80, 78, 71, 0, 255]).expect("write fixture");

    let call = registry
        .create_call(
            "read",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": path,
                "mode": "lines",
            }),
        )
        .expect("read is registered");
    let error = call.run().await.expect_err("binary text read must fail");
    match error {
        ylva::ToolError::Failed { name, message, .. } => {
            assert_eq!(name, "read");
            assert!(message.contains("invalid UTF-8"));
        }
    }
}

#[test]
fn registry_schemas_expose_encoding_defaults_and_order() {
    let workspace = TempDir::new().expect("temporary workspace");
    let registry = binary_registry(&workspace);
    let schemas = registry.schemas();

    let read_schema = schemas
        .iter()
        .find(|schema| schema.name == "read")
        .expect("read schema");
    assert_eq!(
        read_schema.parameters["properties"]["encoding"]["default"],
        "utf8"
    );
    assert!(read_schema.description.contains("base64"));

    let write_schema = schemas
        .iter()
        .find(|schema| schema.name == "write")
        .expect("write schema");
    assert_eq!(
        write_schema.parameters["properties"]["encoding"]["default"],
        "utf8"
    );
    assert!(write_schema.description.contains("base64"));

    let read_call = registry
        .create_call(
            "read",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "path": "binary.dat",
                "mode": "bytes",
                "encoding": "base64",
            }),
        )
        .expect("read is registered");
    assert_eq!(
        read_call
            .title_arguments()
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        vec!["path", "mode", "encoding"]
    );
}
