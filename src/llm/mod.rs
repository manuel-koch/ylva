use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};

pub mod openai_provider;

pub use openai_provider::OpenAiProvider;

/// Runtime abstraction over a chat-completions-style streaming provider.
///
/// Native async trait (Rust 1.75+): no `async-trait` crate. The Provider
/// creates its own internal `mpsc::channel` and returns the receiver, so
/// retry logic and stream parsing stay hidden inside the implementation.
/// Returns a boxed future so the trait stays `dyn`-compatible (an `impl
/// Future` return position would make `Arc<dyn Provider>` unbuildable).
/// The implementation polls the `CancellationToken`; when it fires, the
/// in-flight stream is aborted and the receiver eventually yields
/// `StreamEvent::Cancelled`.
pub trait Provider: Send + Sync {
    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>;
}

/// Deterministic fake provider used by Phase 1 tests and demos.
///
/// `chat()` streams `tokens` one by one, sleeping `interval` between them,
/// then emits `StreamEvent::Done`. If the `CancellationToken` fires mid-stream
/// the current sleep is interrupted via `select!` and `StreamEvent::Cancelled`
/// is emitted promptly. The token sleeps make the mock fully deterministic
/// under `tokio::time::pause`.
#[derive(Debug)]
pub struct MockProvider {
    tokens: Vec<String>,
    interval: Duration,
}

impl MockProvider {
    pub fn new(tokens: Vec<String>, interval: Duration) -> Self {
        MockProvider { tokens, interval }
    }
}

impl Provider for MockProvider {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let tokens = self.tokens.clone();
        let interval = self.interval;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = tokens.len().saturating_sub(1);
                for (index, token) in tokens.iter().enumerate() {
                    if tx.send(StreamEvent::Text(token.clone())).await.is_err() {
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

/// Maps the domain `Message` history onto the OpenAI chat-completions wire
/// format. The domain model stays decoupled from the wire format (blueprint
/// §4A): this is the only place the two communicate.
///
/// Phase 2 maps `System`/`User`/`Assistant`; `Assistant` serializes only its
/// `content` (tool-call serialization is Phase 3). `ToolResult`/`ToolError`
/// cannot occur in Phase 2 and are **skipped silently**; Phase 3 adds their
/// serialization when tools become reachable.
pub fn serialize_messages(messages: &[Message]) -> serde_json::Value {
    let serialized: Vec<serde_json::Value> = messages
        .iter()
        .filter_map(|message| match message {
            Message::System { content } => Some(serde_json::json!({
                "role": "system",
                "content": content,
            })),
            Message::User { content } => Some(serde_json::json!({
                "role": "user",
                "content": content,
            })),
            Message::Assistant { content, .. } => Some(serde_json::json!({
                "role": "assistant",
                "content": content,
            })),
            Message::ToolResult { .. } | Message::ToolError { .. } => None,
        })
        .collect();
    serde_json::Value::Array(serialized)
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

    #[tokio::test(start_paused = true)]
    async fn mock_emits_tokens_then_done_at_interval() {
        let provider = MockProvider::new(
            vec!["Hello".into(), " ".into(), "World".into()],
            Duration::from_millis(10),
        );
        let mut rx = provider
            .chat(Vec::new(), CancellationToken::new())
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
        let mut rx = provider.chat(Vec::new(), cancel.clone()).await.unwrap();

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
            .chat(Vec::new(), CancellationToken::new())
            .await
            .unwrap();
        assert_received_text(&mut rx, "instant").await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::Done)));
    }

    #[test]
    fn serialize_messages_maps_system_user_assistant() {
        use crate::agent::AssistantStatus;
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::User {
                content: "usr".into(),
            },
            Message::Assistant {
                content: "ast".into(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
        ];
        let value = serialize_messages(&messages);
        assert_eq!(
            value,
            serde_json::json!([
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "usr" },
                { "role": "assistant", "content": "ast" },
            ])
        );
    }

    #[test]
    fn serialize_messages_skips_tool_messages_silently() {
        use crate::agent::AssistantStatus;
        let messages = vec![
            Message::User {
                content: "keep".into(),
            },
            Message::ToolResult {
                id: uuid::Uuid::new_v4(),
                name: "tool".into(),
                result: "ok".into(),
            },
            Message::ToolError {
                id: uuid::Uuid::new_v4(),
                name: "tool".into(),
                error: "err".into(),
            },
            Message::Assistant {
                content: "also-kept".into(),
                status: AssistantStatus::Interrupted,
                tool_calls: Vec::new(),
            },
        ];
        let value = serialize_messages(&messages);
        assert_eq!(
            value,
            serde_json::json!([
                { "role": "user", "content": "keep" },
                { "role": "assistant", "content": "also-kept" },
            ])
        );
    }

    #[test]
    fn serialize_messages_empty_slice_yields_empty_array() {
        assert_eq!(serialize_messages(&[]), serde_json::json!([]));
    }
}
