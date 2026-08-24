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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ylva::{
    cli_driver::run_cli_to, default_registry, DeltaChunk, Message, MockEvent, Provider,
    StreamEvent, ToolRegistry, ToolSchema,
};

/// A provider that emits `phase_one` on the first `chat()` call and
/// `phase_two` on every later call. Lets a test script a single tool call
/// followed by a text response (avoiding an infinite tool loop).
struct MockProviderWithScript {
    phase_one: Vec<MockEvent>,
    phase_two: Vec<MockEvent>,
    interval: Duration,
    calls: Arc<AtomicUsize>,
}

impl Provider for MockProviderWithScript {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let phase_one = self.phase_one.clone();
        let phase_two = self.phase_two.clone();
        let interval = self.interval;
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            let script = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                phase_one
            } else {
                phase_two
            };
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Delta(delta) => StreamEvent::Delta(delta),
                    };
                    if tx.send(stream_event).await.is_err() {
                        return;
                    }
                    if index == last_index {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            return;
                        }
                        _ = tokio::time::sleep(interval) => {}
                    }
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }
}

fn tool_call(name: &str, args: serde_json::Value) -> MockEvent {
    MockEvent::Delta(DeltaChunk {
        tool_call_index: 0,
        tool_call_id: None,
        function_name: Some(name.to_string()),
        arguments_fragment: Some(args.to_string()),
    })
}

fn write_scratch(dir: &TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("scratch write");
    path.display().to_string()
}

#[tokio::test]
async fn run_cli_streams_user_and_assistant_and_history() {
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![
            MockEvent::Text("hello ".into()),
            MockEvent::Text("world".into()),
        ],
        phase_two: Vec::new(),
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(ToolRegistry::new());
    let mut output = Vec::new();
    run_cli_to(provider, registry, "greet me".to_string(), &mut output)
        .await
        .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("---[ User ]---"), "output: {output}");
    assert!(output.contains("greet me"), "output: {output}");
    assert!(output.contains("---[ Assistant ]---"), "output: {output}");
    assert!(output.contains("hello world"), "output: {output}");
    assert!(output.contains("Conversation history"), "output: {output}");
    assert!(output.contains("\"role\":\"user\""), "output: {output}");
    assert!(
        output.contains("\"role\":\"assistant\""),
        "output: {output}"
    );
}

#[tokio::test]
async fn run_cli_auto_approves_read_and_runs_it() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scratch(&dir, "doc.md", "file body");
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![tool_call("read", serde_json::json!({ "path": path }))],
        phase_two: vec![MockEvent::Text("ok".into())],
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(default_registry());
    let mut output = Vec::new();
    run_cli_to(provider, registry, "read the file".to_string(), &mut output)
        .await
        .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    // read needs no approval → approved and executed.
    assert!(output.contains("[tool] Read"), "output: {output}");
    assert!(output.contains("done"), "output: {output}");
}

#[tokio::test]
async fn run_cli_denies_write_and_fed_back_as_error() {
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![tool_call(
            "write",
            serde_json::json!({ "path": "/tmp/out.txt", "content": "x" }),
        )],
        phase_two: vec![MockEvent::Text("ok".into())],
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(default_registry());
    let mut output = Vec::new();
    run_cli_to(provider, registry, "write a file".to_string(), &mut output)
        .await
        .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("denied"), "output: {output}");
    // error line is suppressed when the denial was already shown at ToolPending
}
