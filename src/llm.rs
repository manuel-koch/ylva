use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent};

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
}
