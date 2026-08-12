mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use ylva::{
    AgentCommand, AgentLoop, AssistantStatus, Message, OpenAiProvider, Provider, StreamEvent,
    UiUpdate, DEFAULT_SYSTEM_PROMPT,
};

use common::collect_until;
use common::mock_llm_server::{content_frame, MockLlmConfig, MockLlmServer};

/// A provider (`Provider` trait object) pointed at a running [`MockLlmServer`]
/// with a fixed model. Trait-typed so consumers are decoupled from the concrete
/// HTTP provider. Use [`openai_provider_for`] when calling `.chat()` directly —
/// rust-analyzer's inference across the `dyn Provider` boundary is unreliable.
fn provider_for(server: &MockLlmServer) -> Arc<dyn Provider> {
    Arc::new(OpenAiProvider::new(
        server.base_url(),
        "mock-model".into(),
        None,
    ))
}

/// The concrete HTTP provider pointed at a running [`MockLlmServer`]. Used by
/// the tests that call `.chat()` directly, so the IDE can infer the stream type
/// without crossing the `dyn Provider` trait-object boundary.
fn openai_provider_for(server: &MockLlmServer) -> OpenAiProvider {
    OpenAiProvider::new(server.base_url(), "mock-model".into(), None)
}

/// Consume a provider receiver until `Done`, returning the streamed text chunks
/// in order. Panics if the stream errors or ends early.
async fn drain_text_until_done(rx: &mut mpsc::Receiver<StreamEvent>) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match rx.recv().await {
            Some(StreamEvent::Text(text)) => texts.push(text),
            Some(StreamEvent::Done) => return texts,
            Some(other) => panic!("unexpected stream event while draining: {other:?}"),
            None => panic!("provider stream ended without [DONE]"),
        }
    }
}

#[tokio::test]
async fn text_streaming_emits_chunks_in_order_then_done() {
    let config = MockLlmConfig {
        frames: vec![
            content_frame("Hello"),
            content_frame(" "),
            content_frame("there"),
        ],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_provider_for(&server);
    let mut rx = provider
        .chat(Vec::new(), tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_or_else(|error| {
            panic!("provider chat should succeed against a 200 mock: {error:#?}")
        });

    let texts = drain_text_until_done(&mut rx).await;
    assert_eq!(texts, vec!["Hello", " ", "there"]);
    assert_eq!(
        server.cumulative_connections(),
        1,
        "a 200 stream must need only one connection"
    );
    server.abort();
}

#[tokio::test]
async fn cancel_mid_stream_yields_cancelled_and_releases_connection() {
    // Default config streams forever with generated frames (no [DONE]).
    let server = MockLlmServer::start(MockLlmConfig::default())
        .await
        .expect("bind failed");
    let live = server.live_counter();

    let provider = openai_provider_for(&server);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut rx = provider
        .chat(Vec::new(), cancel.clone())
        .await
        .expect("provider chat should succeed");

    // Receive a couple of live tokens so the socket is genuinely established.
    let texts = drain_text_count(&mut rx, 3).await;
    assert_eq!(texts.len(), 3);
    assert!(server.live_connections() > 0);

    cancel.cancel();
    let cancelled = tokio::time::timeout(Duration::from_secs(5), async { rx.recv().await })
        .await
        .expect("cancel must promptly yield a terminal event");
    assert!(matches!(cancelled, Some(StreamEvent::Cancelled)));

    // The dropped request future must close the underlying TCP socket: the
    // mock server's live-connection counter returns to zero (Spike 2 pattern).
    for _ in 0..200 {
        if live.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "server must observe the client disconnect via cooperative cancel"
    );
    server.abort();
}

#[tokio::test]
async fn retry_after_500_then_succeeds() {
    let config = MockLlmConfig {
        statuses: vec![500],
        frames: vec![content_frame("recovered")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_provider_for(&server);
    let mut rx = provider
        .chat(Vec::new(), tokio_util::sync::CancellationToken::new())
        .await
        .expect("chat should eventually succeed after a retry");

    let texts = tokio::time::timeout(Duration::from_secs(20), drain_text_until_done(&mut rx))
        .await
        .expect("retry + stream must complete within timeout");
    assert_eq!(texts, vec!["recovered"]);
    assert!(
        server.cumulative_connections() >= 2,
        "a 500 then success must open at least two connections"
    );
    server.abort();
}

#[tokio::test]
async fn persistent_500_yields_pre_stream_error() {
    let config = MockLlmConfig {
        statuses: vec![500; 5],
        fallback_status: 500,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_provider_for(&server);
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        provider.chat(Vec::new(), tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("retries must finish within timeout");

    let message = result
        .expect_err("persistent 500 must exhaust retries and return an error")
        .to_string();
    assert!(
        message.contains("mock 500"),
        "the error should surface the backend's response body: {message}"
    );
    server.abort();
}

#[tokio::test]
async fn non_retryable_status_reports_error_with_backend_body() {
    let config = MockLlmConfig {
        statuses: vec![404],
        frames: vec![content_frame("must not be reached")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");

    let provider = openai_provider_for(&server);
    let message = provider
        .chat(Vec::new(), tokio_util::sync::CancellationToken::new())
        .await
        .expect_err("a 4xx is a terminal pre-stream failure")
        .to_string();
    assert!(
        message.contains("HTTP 404"),
        "error should name the status code: {message}"
    );
    assert!(
        message.contains("Not Found"),
        "error should include the backend's explanation body: {message}"
    );
    assert_eq!(
        server.cumulative_connections(),
        1,
        "a 4xx must never be retried"
    );
    server.abort();
}

#[tokio::test]
async fn agent_loop_round_trip_commits_streamed_text() {
    let config = MockLlmConfig {
        frames: vec![content_frame("The "), content_frame("answer.")],
        end_after_frames: true,
        ..Default::default()
    };
    let server = MockLlmServer::start(config).await.expect("bind failed");
    let provider = provider_for(&server);

    let (mut agent, mut ui_rx) = AgentLoop::new(provider);
    agent
        .send_command(AgentCommand::Prompt("question".into()))
        .await;
    let handle = agent.run();

    collect_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: "question".to_string(),
            },
            Message::Assistant {
                content: "The answer.".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
        ]
    );
    server.abort();
}

async fn drain_text_count(rx: &mut mpsc::Receiver<StreamEvent>, count: usize) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        match rx.recv().await {
            Some(StreamEvent::Text(text)) => {
                texts.push(text);
                if texts.len() >= count {
                    return texts;
                }
            }
            Some(other) => panic!("unexpected stream event while counting: {other:?}"),
            None => panic!("provider stream ended before {count} texts"),
        }
    }
}

/// Verifies that cancellation closes the underlying TCP socket, confirmed with
/// the OS socket table (`lsof`/`netstat`) rather than only the mock server's
/// live-connection counter. Absent tools report nothing and are tolerated. Runs
/// as part of `make test` for now; re-gate behind `#[ignore]` once CI actually
/// runs the suite on machines guaranteed to have those tools.
#[tokio::test]
async fn cancel_closes_socket_verified_via_os_tools() {
    let server = MockLlmServer::start(MockLlmConfig::default())
        .await
        .expect("bind failed");
    let port = server.addr.port();

    let provider = openai_provider_for(&server);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut rx = provider
        .chat(Vec::new(), cancel.clone())
        .await
        .expect("provider chat should succeed");

    let _ = drain_text_count(&mut rx, 2).await;
    cancel.cancel();
    let _ = rx.recv().await; // StreamEvent::Cancelled

    // Poll the OS socket table until no live (ESTABLISHED/CLOSE_WAIT) entry
    // remains for the server's port, i.e. the socket closed promptly.
    let mut socket_lines = lingering_socket_lines(port);
    for _ in 0..100 {
        if socket_lines.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        socket_lines = lingering_socket_lines(port);
    }
    assert!(
        socket_lines.is_empty(),
        "socket must close promptly on cancel; lingering: {socket_lines:?}"
    );
    server.abort();
}

/// Inspect the OS socket table for live (ESTABLISHED / CLOSE_WAIT) entries on
/// `port`, via `lsof` then `netstat`. Absent tools simply report nothing.
fn lingering_socket_lines(port: u16) -> Vec<String> {
    let mut findings = Vec::new();
    if let Ok(output) = std::process::Command::new("lsof")
        .args(["-i", &format!(":{port}")])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if is_lingering_line(line) {
                findings.push(format!("lsof: {line}"));
            }
        }
    }
    if let Ok(output) = std::process::Command::new("netstat").args(["-an"]).output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains(&format!(":{port}")) && is_lingering_line(line) {
                findings.push(format!("netstat: {line}"));
            }
        }
    }
    findings
}

fn is_lingering_line(line: &str) -> bool {
    line.contains("ESTABLISHED") || line.contains("CLOSE_WAIT")
}
