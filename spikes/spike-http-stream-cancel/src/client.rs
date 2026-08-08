use std::time::Instant;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// A streaming chat-completions (OpenAI spec) endpoint configuration.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl EndpointConfig {
    /// Compose the chat-completions URL for a base like
    /// `http://localhost:PORT` or `https://openrouter.ai/api/v1`.
    pub fn completions_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }
}

/// Which cancel mechanism (and sub-case) an iteration exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelMode {
    /// Drop the stream future directly, without a `CancellationToken`.
    Drop(DropSubCase),
    /// Cooperatively abort via a `CancellationToken` polled in `tokio::select!`.
    Token,
}

/// The two direct-drop sub-cases that adjudicate Pitfall #8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropSubCase {
    /// Drop only the `eventsource`/`bytes_stream` adapter.
    AdapterOnly,
    /// Drop the adapter and the `Response` together.
    AdapterAndResponse,
}

/// Why a run's receive loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakReason {
    Cancelled,
    Dropped,
    StreamEnded,
    Error(String),
}

/// Per-run result.
#[derive(Debug)]
pub struct RunSummary {
    pub events: usize,
    pub accumulated_text: String,
    /// The raw `data:` payload of every non-`[DONE]` SSE event, in receive order.
    pub received: Vec<String>,
    pub break_reason: BreakReason,
    /// The wall-clock moment the cancel was triggered (drop or token fire).
    pub triggered_at: Option<Instant>,
    /// Tokens delivered by the stream after the cancel was triggered (race probe).
    pub events_after_trigger: usize,
}

/// Map a fractional offset `0.0 < fraction <= 1.0` onto an event index into a
/// stream budget of `budget_events`, guaranteeing the trigger lands mid-stream
/// (never at index 0 and never past the final frame).
pub fn event_offset(fraction: f64, budget_events: usize) -> usize {
    if budget_events <= 1 {
        return 0;
    }
    let scaled = (fraction.clamp(0.0, 1.0) * budget_events as f64).floor() as usize;
    scaled.clamp(1, budget_events - 1)
}

/// Build the OpenAI-style streaming request body.
fn request_body(endpoint: &EndpointConfig, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": endpoint.model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": true
    })
}

fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("client build failed")
}

/// A raw `reqwest` + `eventsource-stream` client that streams from an endpoint
/// and stops by **dropping the stream future** at `cancel_at_event`.
///
/// `sub_case` selects whether the `Response` is dropped together with the SSE
/// adapter (Pitfall #8 comparison).
pub async fn stream_and_drop(
    endpoint: &EndpointConfig,
    prompt: &str,
    sub_case: DropSubCase,
    cancel_at_event: usize,
) -> Result<RunSummary, Box<dyn std::error::Error>> {
    let client = make_client();
    let body = request_body(endpoint, prompt);

    let mut request = client.post(endpoint.completions_url()).json(&body);
    if let Some(key) = &endpoint.api_key {
        request = request.bearer_auth(key);
    }

    let response = request.send().await?;

    let mut events = 0usize;
    let mut accumulated = String::new();
    let mut received: Vec<String> = Vec::new();
    let mut triggered_at: Option<Instant> = None;
    let events_after_trigger = 0usize;
    let mut break_reason = BreakReason::StreamEnded;

    match sub_case {
        DropSubCase::AdapterOnly => {
            // `Response::bytes_stream(self)` consumes the `Response`, so the SSE
            // adapter owns the byte stream; dropping just the adapter is the
            // Pitfall #8 comparison case.
            let mut stream = response.bytes_stream().eventsource();
            while let Some(item) = stream.next().await {
                events += 1;
                match item {
                    Ok(event) if event.data != "[DONE]" => {
                        received.push(event.data.clone());
                        accumulated.push_str(&event.data);
                    }
                    Ok(_) => break,
                    Err(message) => {
                        break_reason = BreakReason::Error(message.to_string());
                        break;
                    }
                }
                if events == cancel_at_event {
                    triggered_at = Some(Instant::now());
                    break_reason = BreakReason::Dropped;
                    drop(stream);
                    break;
                }
            }
        }
        DropSubCase::AdapterAndResponse => {
            // Build the byte stream from `Response::chunk` so the `Response` is
            // owned by the stream and is guaranteed dropped together with it —
            // the "drop summary and Response together" sub-case.
            let byte_stream = futures::stream::unfold(Some(response), |response| async move {
                match response {
                    Some(mut current) => match current.chunk().await {
                        Ok(Some(bytes)) => Some((Ok(bytes), Some(current))),
                        Ok(None) => None,
                        Err(message) => Some((Err(message), None)),
                    },
                    None => None,
                }
            });
            let stream = byte_stream.eventsource();
            futures::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                events += 1;
                match item {
                    Ok(event) if event.data != "[DONE]" => {
                        received.push(event.data.clone());
                        accumulated.push_str(&event.data);
                    }
                    Ok(_) => break,
                    Err(message) => {
                        break_reason = BreakReason::Error(message.to_string());
                        break;
                    }
                }
                if events == cancel_at_event {
                    triggered_at = Some(Instant::now());
                    break_reason = BreakReason::Dropped;
                    // Break so the pinned stream (which owns the `Response`) is
                    // dropped together with the adapter.
                    break;
                }
            }
        }
    }

    Ok(RunSummary {
        events,
        accumulated_text: accumulated,
        received,
        break_reason,
        triggered_at,
        events_after_trigger,
    })
}

/// Stream from an endpoint and cooperatively abort via a `CancellationToken`
/// polled with `biased` priority in a `tokio::select!` — mirroring
/// `docs/plan.md` §4A.
///
/// The token is fired from within the loop once `cancel_at_event` events have
/// been received, which maximises contention between `cancel.cancelled()` and
/// `stream.next()` (the race the spike must rule out).
pub async fn stream_with_token(
    endpoint: &EndpointConfig,
    prompt: &str,
    cancel_at_event: usize,
) -> Result<RunSummary, Box<dyn std::error::Error>> {
    let client = make_client();
    let body = request_body(endpoint, prompt);

    let mut request = client.post(endpoint.completions_url()).json(&body);
    if let Some(key) = &endpoint.api_key {
        request = request.bearer_auth(key);
    }

    let response = request.send().await?;

    let stream = response.bytes_stream().eventsource();
    futures::pin_mut!(stream);

    let cancel = CancellationToken::new();

    let mut events = 0usize;
    let mut accumulated = String::new();
    let mut received: Vec<String> = Vec::new();
    let mut triggered_at: Option<Instant> = None;
    let mut events_at_trigger: usize = 0;

    let break_reason = 'outer: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Cooperative burst: break out of the select promptly.
                break 'outer BreakReason::Cancelled;
            }
            item = stream.next() => {
                match item {
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" {
                            break 'outer BreakReason::StreamEnded;
                        }
                        events += 1;
                        received.push(event.data.clone());
                        accumulated.push_str(&event.data);
                        if Some(events) == Some(cancel_at_event) {
                            triggered_at = Some(Instant::now());
                            events_at_trigger = events;
                            cancel.cancel();
                        }
                    }
                    Some(Err(message)) => {
                        break 'outer BreakReason::Error(message.to_string());
                    }
                    None => {
                        break 'outer BreakReason::StreamEnded;
                    }
                }
            }
        }
    };

    let events_after_trigger = if triggered_at.is_some() {
        events.saturating_sub(events_at_trigger)
    } else {
        0
    };

    Ok(RunSummary {
        events,
        accumulated_text: accumulated,
        received,
        break_reason,
        triggered_at,
        events_after_trigger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_offset_lands_mid_stream() {
        assert_eq!(event_offset(0.5, 100), 50);
        assert_eq!(event_offset(0.1, 100), 10);
        assert_eq!(event_offset(0.0, 100), 1);
        assert_eq!(event_offset(1.0, 100), 99);
        assert_eq!(event_offset(0.5, 0), 0);
        assert_eq!(event_offset(0.5, 1), 0);
    }

    #[test]
    fn completions_url_is_joined_correctly() {
        let cfg = EndpointConfig {
            base_url: "http://localhost:11434".into(),
            model: "test".into(),
            api_key: None,
        };
        assert_eq!(
            cfg.completions_url(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn request_body_sets_stream_true() {
        let cfg = EndpointConfig {
            base_url: "http://localhost:1".into(),
            model: "some-model".into(),
            api_key: None,
        };
        let body = request_body(&cfg, "hello");
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "some-model");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn token_cancel_releases_mock_connection() {
        let server = crate::mock_server::MockServer::start(Some(1000))
            .await
            .expect("mock server failed to start");
        let live = server.live_counter();

        let endpoint = EndpointConfig {
            base_url: format!("http://{}", server.addr),
            model: "test".into(),
            api_key: None,
        };

        let summary = stream_with_token(&endpoint, "ping", 3)
            .await
            .expect("raw client should stream from the mock server");
        assert_eq!(summary.break_reason, BreakReason::Cancelled);

        for _ in 0..100 {
            if live.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            live.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "server must observe the client disconnect via cooperative cancel"
        );
        server.abort();
    }
}
