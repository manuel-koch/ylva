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

use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{DeltaChunk, Message, StreamEvent};

use super::usage_cost::{CostLedger, RequestCost};
use super::{ModelInfo, Provider, ToolSchema};

/// One item of a deterministic [`MockProvider`] script.
///
/// The script is the single deterministic path both text-streaming and
/// tool-use-loop tests share: a sequence of text tokens and raw
/// [`DeltaChunk`] tool-call fragments, emitted at a fixed interval. `chat()`
/// always terminates the script with `StreamEvent::Done`.
#[derive(Debug, Clone)]
pub enum MockEvent {
    Text(String),
    Thinking(String),
    Delta(DeltaChunk),
}

/// Deterministic fake provider used by tests and demos.
///
/// `chat()` streams the script items one by one, sleeping `interval` between
/// them, then emits `StreamEvent::Done`. If the `CancellationToken` fires
/// mid-stream the current sleep is interrupted via `select!` and
/// `StreamEvent::Cancelled` is emitted promptly. The token sleeps make the mock
/// fully deterministic under `tokio::time::pause`.
#[derive(Debug, Clone)]
pub struct MockProvider {
    script: Vec<MockEvent>,
    interval: Duration,
    reported_cost: Option<RequestCost>,
    cost_ledger: CostLedger,
}

impl MockProvider {
    /// A text-only script from `tokens`.
    pub fn new(tokens: Vec<String>, interval: Duration) -> Self {
        let script = tokens.into_iter().map(MockEvent::Text).collect();
        MockProvider::with_script(script, interval)
    }

    /// A fully scripted provider (text + tool-call deltas) for deterministic
    /// tool-use loop tests. `chat()` emits `script` at `interval`, then `Done`.
    pub fn with_script(script: Vec<MockEvent>, interval: Duration) -> Self {
        MockProvider {
            script,
            interval,
            reported_cost: None,
            cost_ledger: CostLedger::process(),
        }
    }

    /// Record `cost` into the cost ledger once per `chat` request, as a
    /// provider with usage pricing would.
    pub fn with_reported_cost(mut self, cost: RequestCost) -> Self {
        self.reported_cost = Some(cost);
        self
    }

    /// Record reported costs into `cost_ledger` instead of the process ledger.
    pub fn with_cost_ledger(mut self, cost_ledger: CostLedger) -> Self {
        self.cost_ledger = cost_ledger;
        self
    }
}

impl Provider for MockProvider {
    fn with_runtime_config(
        &self,
        _runtime_config: crate::llm::engine::EngineRuntimeConfigHandle,
    ) -> anyhow::Result<std::sync::Arc<dyn Provider>> {
        Ok(std::sync::Arc::new(self.clone()))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let script = self.script.clone();
        let interval = self.interval;
        if let Some(cost) = self.reported_cost.clone() {
            self.cost_ledger.record(cost);
        }
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Thinking(text) => StreamEvent::Thinking(text),
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn assert_received_text(rx: &mut mpsc::Receiver<StreamEvent>, expected: &str) {
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::Text(text)) if text == expected),
            "expected Text({expected:?})"
        );
    }

    #[test]
    fn mock_provider_name_returns_mock() {
        let provider = MockProvider::new(vec![], Duration::from_millis(10));
        assert_eq!(provider.name(), "mock");
    }

    #[tokio::test]
    async fn mock_provider_list_models_returns_empty_vec() {
        let provider = MockProvider::new(vec![], Duration::from_millis(10));
        let models = provider.list_models().await.unwrap();
        assert!(models.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_tokens_then_done_at_interval() {
        let provider = MockProvider::new(
            vec!["Hello".into(), " ".into(), "World".into()],
            Duration::from_millis(10),
        );
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();

        assert_received_text(&mut rx, "Hello").await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_received_text(&mut rx, " ").await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_received_text(&mut rx, "World").await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_cancelled_when_token_fired_mid_stream() {
        let provider = MockProvider::new(
            vec!["first".into(), "second".into(), "third".into()],
            Duration::from_millis(10),
        );
        let cancel = CancellationToken::new();
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), cancel.clone())
            .await
            .unwrap();

        assert_received_text(&mut rx, "first").await;
        cancel.cancel();
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::Cancelled)),
            "expected Cancelled after token fired mid-stream"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mock_emits_first_token_without_delay() {
        let provider = MockProvider::new(vec!["instant".into()], Duration::from_millis(1000));
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();
        assert_received_text(&mut rx, "instant").await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn mock_script_emits_delta_fragments_then_done() {
        let provider = MockProvider::with_script(
            vec![
                MockEvent::Delta(DeltaChunk {
                    tool_call_index: 0,
                    tool_call_id: Some("call_1".into()),
                    function_name: Some("read".into()),
                    arguments_fragment: Some("{\"path\":".into()),
                }),
                MockEvent::Delta(DeltaChunk {
                    tool_call_index: 0,
                    tool_call_id: None,
                    function_name: None,
                    arguments_fragment: Some("\"x.txt\"}".into()),
                }),
            ],
            Duration::from_millis(10),
        );
        let mut rx = provider
            .chat(Vec::new(), Vec::new(), CancellationToken::new())
            .await
            .unwrap();
        let first = rx.recv().await;
        assert!(
            matches!(first, Some(StreamEvent::Delta(d)) if d.arguments_fragment.as_deref() == Some("{\"path\":"))
        );
        let second = rx.recv().await;
        assert!(
            matches!(second, Some(StreamEvent::Delta(d)) if d.arguments_fragment.as_deref() == Some("\"x.txt\"}"))
        );
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }
}
