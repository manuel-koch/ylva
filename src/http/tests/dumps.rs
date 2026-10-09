use std::path::PathBuf;

async fn start_json_response_server() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let address = listener.local_addr().expect("server address");
    let handle = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request_head = [0_u8; 4096];
            let bytes_read = stream.read(&mut request_head).await.expect("read request");
            let response_body = if request_head[..bytes_read].starts_with(b"GET ") {
                "{\n  \"kind\": \"get\"\n}"
            } else {
                "{\n  \"kind\": \"post\"\n}"
            };
            let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    (format!("http://{address}"), handle)
}

fn dump_paths(directory: &std::path::Path, suffix: &str) -> Vec<PathBuf> {
    let mut paths = std::fs::read_dir(directory)
        .expect("dump directory")
        .map(|entry| entry.expect("dump entry").path())
        .filter(|path| path.to_string_lossy().ends_with(suffix))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

use super::*;

#[test]
fn build_dump_path_strips_extension_from_template() {
    let path =
        build_dump_path_from_template("/tmp/dumps/ylva.json", "1_002", "dump-a", "request", "json")
            .expect("path");
    let file_name = path.file_name().and_then(|n| n.to_str()).expect("name");
    assert!(
        file_name.starts_with("ylva_"),
        "expected stem prefix 'ylva_', got {file_name}"
    );
    assert!(
        !file_name.contains(".json_"),
        "extension must not appear mid-name: {file_name}"
    );
    assert_eq!(file_name, "ylva_1_002_dump-a_request.json");
}

#[test]
fn build_dump_path_uses_stem_and_absolute_dir() {
    let path =
        build_dump_path_from_template("/tmp/dumps/my_req", "1_002", "dump-a", "response", "jsonl")
            .expect("path");
    let parent = path.parent().expect("parent");
    let file_name = path.file_name().and_then(|n| n.to_str()).expect("name");
    assert_eq!(parent, Path::new("/tmp/dumps"));
    assert_eq!(file_name, "my_req_1_002_dump-a_response.jsonl");
}

#[test]
fn whitespace_dump_template_returns_no_paths() {
    let context = RequestDumpContext::new(&RequestDumpSource::from_template("   "));
    assert!(context.request_path().is_none());
    assert!(context
        .response_path_for_content_type(Some("text/html"), "bin")
        .is_none());
}

#[test]
fn response_dump_extension_follows_content_type() {
    assert_eq!(
        response_dump_extension(Some("text/html; charset=UTF-8"), "bin"),
        "html"
    );
    assert_eq!(
        response_dump_extension(Some("application/json"), "bin"),
        "json"
    );
    assert_eq!(
        response_dump_extension(Some("text/event-stream"), "bin"),
        "jsonl"
    );
    assert_eq!(response_dump_extension(None, "bin"), "bin");
    assert_eq!(
        response_dump_extension(Some("application/octet-stream"), "bin"),
        "bin"
    );
}

#[test]
fn non_streaming_json_helpers_dump_requests_and_responses() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let dump_source = RequestDumpSource::from_template(temp_dir.path().join("requests.jsonl"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio runtime");

    let (base_url, server) = runtime.block_on(start_json_response_server());
    let (get_body, post_body) = runtime.block_on(async {
        let client = reqwest::Client::new();
        let get_body = get_json_with_dump_source(
            &client,
            &format!("{base_url}/get"),
            Some("secret-key"),
            "get",
            &dump_source,
        )
        .await
        .expect("GET response");
        let post_body = post_json_with_dump_source(
            &client,
            &format!("{base_url}/post"),
            &serde_json::json!({"request": "body"}),
            Some("secret-key"),
            "post",
            &dump_source,
        )
        .await
        .expect("POST response");
        (get_body, post_body)
    });
    runtime.block_on(server).expect("server task");

    assert_eq!(get_body["kind"], "get");
    assert_eq!(post_body["kind"], "post");

    let request_paths = dump_paths(temp_dir.path(), "_request.jsonl");
    assert_eq!(request_paths.len(), 2, "one request dump per helper call");
    let mut request_bodies = Vec::new();
    for path in request_paths {
        let contents = std::fs::read_to_string(path).expect("request dump");
        let mut lines = contents.lines();
        let metadata: serde_json::Value =
            serde_json::from_str(lines.next().expect("request metadata")).expect("metadata");
        request_bodies
            .push(serde_json::from_str(lines.next().expect("request body")).expect("body"));
        assert_eq!(metadata["headers"]["authorization"], REDACTED_HEADER_VALUE);
        assert!(lines.next().is_none(), "request dump has exactly two lines");
    }
    assert!(request_bodies.contains(&serde_json::Value::Null));
    assert!(request_bodies.contains(&serde_json::json!({"request": "body"})));

    let response_paths = dump_paths(temp_dir.path(), "_response.json");
    assert_eq!(response_paths.len(), 2, "one response dump per helper call");
    let response_contents = response_paths
        .into_iter()
        .map(|path| std::fs::read_to_string(path).expect("response dump"))
        .collect::<Vec<_>>();
    assert!(response_contents.contains(&"{\n  \"kind\": \"get\"\n}".to_string()));
    assert!(response_contents.contains(&"{\n  \"kind\": \"post\"\n}".to_string()));
}

#[test]
fn build_dump_path_returns_none_when_template_is_whitespace_only() {
    assert!(build_dump_path_from_template("   ", "1_002", "dump-a", "request", "json").is_none());
}

#[test]
fn build_dump_path_falls_back_to_default_stem_when_template_has_no_stem() {
    let path =
        build_dump_path_from_template("/", "1_002", "dump-a", "request", "json").expect("path");
    let file_name = path.file_name().and_then(|n| n.to_str()).expect("name");
    assert_eq!(file_name, "ylva_req_1_002_dump-a_request.json");
}

#[test]
fn build_dump_path_resolves_relative_template_against_cwd() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cwd = std::env::current_dir().expect("cwd");
    let path = build_dump_path_from_template("sub/req", "1_002", "dump-a", "request", "json")
        .expect("path");
    assert!(
        path.starts_with(cwd.join("sub")),
        "expected path under CWD/sub, got {path:?}"
    );
}

#[test]
fn current_dump_timestamp_matches_seconds_underscore_millis_shape() {
    let timestamp = current_dump_timestamp();
    let (secs, millis) = timestamp.split_once('_').expect("underscore separator");
    assert!(secs.parse::<u64>().is_ok(), "seconds should be numeric");
    assert_eq!(millis.len(), 3, "millis should be zero-padded to 3 digits");
    assert!(millis.parse::<u32>().is_ok(), "millis should be numeric");
}

#[test]
fn dump_request_writes_metadata_then_body_as_two_json_lines() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let file_path = temp_dir.path().join("request.jsonl");
    let body = serde_json::json!({"model": "test", "messages": []});
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-custom", "value".parse().expect("header value"));
    let redacted_headers = redacted_headers_json(&headers);

    dump_request(
        "https://example.com/v1/chat/completions?stream=true",
        &redacted_headers,
        &body,
        &file_path,
    );

    let contents = std::fs::read_to_string(&file_path).expect("read file");
    let mut lines = contents.lines();
    let meta: serde_json::Value =
        serde_json::from_str(lines.next().expect("meta line")).expect("valid json");
    let written_body: serde_json::Value =
        serde_json::from_str(lines.next().expect("body line")).expect("valid json");
    assert!(lines.next().is_none(), "exactly two lines expected");

    assert_eq!(meta["scheme"], "https");
    assert_eq!(meta["host"], "example.com");
    assert_eq!(meta["path"], "/v1/chat/completions");
    assert_eq!(meta["query"], "stream=true");
    assert_eq!(meta["headers"]["x-custom"], "value");
    assert_eq!(written_body, body);
}

#[test]
fn request_dump_context_appends_failure_to_its_matching_response_dump() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let context = RequestDumpContext::new(&RequestDumpSource::from_template(
        temp_dir.path().join("compaction.jsonl"),
    ));

    context.dump_failure_response(&anyhow::anyhow!("summary timed out after 60 seconds"));

    let response_path = context
        .response_path_for_content_type(None, "jsonl")
        .expect("response dump path");
    let contents = std::fs::read_to_string(response_path).expect("failure response dump");
    assert_eq!(contents, "[failure] summary timed out after 60 seconds\n");
}

#[test]
fn dump_request_redacts_sensitive_header_values() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let file_path = temp_dir.path().join("request.jsonl");
    let body = serde_json::json!({});
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer super-secret-key".parse().expect("header value"),
    );
    headers.insert("x-api-key", "another-secret".parse().expect("header value"));
    headers.insert(
        "x-subscription-token",
        "brave-subscription-secret".parse().expect("header value"),
    );
    headers.insert(
        "x-custom-token",
        "custom-token-secret".parse().expect("header value"),
    );
    headers.insert(
        "x-token-id",
        "not-a-credential".parse().expect("header value"),
    );
    headers.insert("x-custom", "not-secret".parse().expect("header value"));
    let redacted_headers = redacted_headers_json(&headers);

    dump_request(
        "https://example.com/v1/chat",
        &redacted_headers,
        &body,
        &file_path,
    );

    let contents = std::fs::read_to_string(&file_path).expect("read file");
    let meta: serde_json::Value =
        serde_json::from_str(contents.lines().next().expect("meta line")).expect("valid json");
    assert_eq!(meta["headers"]["authorization"], REDACTED_HEADER_VALUE);
    assert_eq!(meta["headers"]["x-api-key"], REDACTED_HEADER_VALUE);
    assert_eq!(
        meta["headers"]["x-subscription-token"],
        REDACTED_HEADER_VALUE
    );
    assert_eq!(meta["headers"]["x-custom-token"], REDACTED_HEADER_VALUE);
    assert_eq!(meta["headers"]["x-token-id"], "not-a-credential");
    assert_eq!(meta["headers"]["x-custom"], "not-secret");
    assert!(
        is_sensitive_header_name("X-Subscription-Token"),
        "the -token suffix rule must match mixed-case header names"
    );
    assert!(
        !is_sensitive_header_name("X-Token-Id"),
        "a name that merely contains the suffix must stay unaffected"
    );
    assert!(
        !contents.contains("super-secret-key")
            && !contents.contains("another-secret")
            && !contents.contains("brave-subscription-secret")
            && !contents.contains("custom-token-secret"),
        "no secret value may appear anywhere in the dump: {contents:?}"
    );
}

#[test]
fn dump_request_creates_parent_dir_when_missing() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let file_path = temp_dir.path().join("nested/dir/request.jsonl");
    let body = serde_json::json!({"nested": true});
    let redacted_headers = redacted_headers_json(&reqwest::header::HeaderMap::new());

    dump_request(
        "https://example.com/v1/chat",
        &redacted_headers,
        &body,
        &file_path,
    );

    assert!(file_path.exists(), "file should have been created");
}

#[test]
fn build_request_and_response_dump_paths_share_timestamp_and_differ_by_kind_and_extension() {
    let timestamp = "1_002";
    let dump_id = "dump-a";
    let request_path =
        build_dump_path_from_template("/tmp/dumps/ylva_req", timestamp, dump_id, "request", "json")
            .expect("request path");
    let response_path = build_dump_path_from_template(
        "/tmp/dumps/ylva_req",
        timestamp,
        dump_id,
        "response",
        "jsonl",
    )
    .expect("response path");

    assert_eq!(
        request_path,
        Path::new("/tmp/dumps/ylva_req_1_002_dump-a_request.json")
    );
    assert_eq!(
        response_path,
        Path::new("/tmp/dumps/ylva_req_1_002_dump-a_response.jsonl")
    );
}

#[test]
fn dump_ids_keep_same_timestamp_paths_distinct() {
    let first_path =
        build_dump_path_from_template("/tmp/dumps/ylva_req", "1_002", "dump-a", "request", "jsonl")
            .expect("first path");
    let second_path =
        build_dump_path_from_template("/tmp/dumps/ylva_req", "1_002", "dump-b", "request", "jsonl")
            .expect("second path");

    assert_ne!(first_path, second_path);
}

#[test]
fn append_response_dump_line_creates_parent_dir_and_appends_lines() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let file_path = temp_dir.path().join("nested/dir/response.jsonl");

    append_response_dump_line("data: {\"a\":1}", &file_path);
    append_response_dump_line("data: [DONE]", &file_path);

    let contents = std::fs::read_to_string(&file_path).expect("read file");
    assert_eq!(contents, "data: {\"a\":1}\ndata: [DONE]\n");
}
