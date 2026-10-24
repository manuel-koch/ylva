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

use super::*;

#[test]
fn retry_delay_honors_retry_after() {
    assert_eq!(retry_delay(1, Some(0)), Duration::ZERO);
    assert_eq!(
        retry_delay_with_jitter(1, Some(2), 0.0),
        Duration::from_secs(2)
    );
    assert_eq!(
        retry_delay_with_jitter(1, Some(2), 0.5),
        Duration::from_millis(3500)
    );
    assert!(retry_delay_with_jitter(1, Some(2), 0.9999) < Duration::from_secs(5));
    assert_eq!(
        retry_delay_with_jitter(5, Some(9999), 0.5),
        Duration::from_secs(RETRY_MAX_SECONDS)
    );
}

#[test]
fn cooldown_gate_extends_to_latest_deadline_and_never_shortens() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
        let cooldown = RetryCooldown::default();
        let shared = cooldown.clone();
        let cancel = CancellationToken::new();
        assert_eq!(
            cooldown.wait_until_open(&cancel).await,
            Some(Duration::ZERO)
        );
        cooldown.note_retry_after(Duration::from_secs(10));
        shared.note_retry_after(Duration::from_secs(2));
        let waited = shared.wait_until_open(&cancel).await.expect("gate opens");
        assert_eq!(waited, Duration::from_secs(10));
        shared.note_retry_after(Duration::from_secs(20));
        let waited = cooldown.wait_until_open(&cancel).await.expect("gate opens");
        assert_eq!(waited, Duration::from_secs(20));
    });
}

#[test]
fn cooldown_gate_wait_is_cancel_aware() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
        let cooldown = RetryCooldown::default();
        cooldown.note_retry_after(Duration::from_secs(3600));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(cooldown.wait_until_open(&cancel).await, None);
    });
}

#[test]
fn parse_retry_after_accepts_seconds_and_http_dates() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(parse_retry_after("3", now), Some(3));
    assert_eq!(
        parse_retry_after("Tue, 14 Nov 2023 22:13:23 GMT", now),
        Some(3)
    );
    assert_eq!(parse_retry_after("invalid", now), None);
}

#[test]
fn retry_delay_backs_off_bounded_by_max() {
    // Deterministic bounds against injected jitter: the wall-clock
    // wrapper samples full jitter from `[0, 1)` and may legally land on
    // a near-zero delay, so the backoff contract is asserted on fixed
    // fractions rather than on a lucky clock reading.
    assert!(
        retry_delay_with_jitter(1, None, 0.9999) < Duration::from_secs(2),
        "the first attempt stays below twice its 1s base"
    );
    assert_eq!(
        retry_delay_with_jitter(2, None, 0.5),
        Duration::from_millis(1000),
        "the second attempt backs off half of its 2s cap"
    );
    assert_eq!(
        retry_delay_with_jitter(40, None, 1.0),
        Duration::from_secs(RETRY_MAX_SECONDS),
        "the exponential backoff saturates at the cap"
    );
    assert!(
        retry_delay_with_jitter(40, None, 0.9999) <= Duration::from_secs(RETRY_MAX_SECONDS),
        "no jitter can exceed the cap"
    );
    // Structural bounds that hold for any clock-derived jitter.
    assert!(retry_delay(1, None) <= Duration::from_secs(RETRY_BASE_SECONDS));
    assert!(retry_delay(40, None) <= Duration::from_secs(RETRY_MAX_SECONDS));
}

#[test]
fn should_retry_marks_ratelimit_and_server_errors() {
    use reqwest::StatusCode;
    assert!(should_retry(StatusCode::TOO_MANY_REQUESTS));
    assert!(should_retry(StatusCode::INTERNAL_SERVER_ERROR));
    assert!(should_retry(StatusCode::SERVICE_UNAVAILABLE));
    assert!(!should_retry(StatusCode::OK));
    assert!(!should_retry(StatusCode::BAD_REQUEST));
    assert!(!should_retry(StatusCode::UNAUTHORIZED));
}

#[test]
fn snippet_from_body_is_bounded_to_max_chars() {
    let long = "x".repeat(RESPONSE_BODY_SNIPPET_MAX_CHARS + 100);
    let snippet = snippet_from_body(long.as_bytes());
    assert!(snippet.chars().count() == RESPONSE_BODY_SNIPPET_MAX_CHARS);
    assert_eq!(snippet, "x".repeat(RESPONSE_BODY_SNIPPET_MAX_CHARS));
}

#[test]
fn status_error_joins_snippet_only_when_present() {
    let message = status_error(
        reqwest::StatusCode::NOT_FOUND,
        "http://example/api",
        "request",
        "not found on server",
        0,
        Duration::ZERO,
    )
    .to_string();
    assert_eq!(
        message,
        "request failed with HTTP 404: not found on server for http://example/api"
    );

    let blank = status_error(
        reqwest::StatusCode::NOT_FOUND,
        "http://example/api",
        "request",
        "",
        0,
        Duration::ZERO,
    )
    .to_string();
    assert_eq!(blank, "request failed with HTTP 404 for http://example/api");

    let exhausted = status_error(
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "http://example/api",
        "request",
        "down",
        MAX_RETRIES,
        Duration::from_secs(74),
    )
    .to_string();
    assert_eq!(
        exhausted,
        format!(
            "request failed with HTTP 503: down for http://example/api after {MAX_RETRIES} retries over 74s"
        )
    );
}

#[test]
fn status_error_reports_prior_retries_on_non_retryable_status() {
    // Two retries happened before a final non-retryable 400; the message
    // must not claim no retry was performed.
    let message = status_error(
        reqwest::StatusCode::BAD_REQUEST,
        "http://example/api",
        "request",
        "bad",
        2,
        Duration::from_millis(1500),
    )
    .to_string();
    assert_eq!(
        message,
        "request failed with HTTP 400: bad for http://example/api after 2 retries over 1s"
    );
}

#[test]
fn generic_request_limiter_bounds_concurrent_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
        let limiter = RequestConcurrencyLimiter::new(3);
        let active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum_active_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = (0..12)
            .map(|_| {
                let limiter = limiter.clone();
                let active_requests = std::sync::Arc::clone(&active_requests);
                let maximum_active_requests = std::sync::Arc::clone(&maximum_active_requests);
                tokio::spawn(async move {
                    let _permit = limiter.acquire().await.expect("request permit");
                    let active =
                        active_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    maximum_active_requests.fetch_max(active, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    active_requests.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
            })
            .collect::<Vec<_>>();
        for request in requests {
            request.await.expect("request task");
        }
        assert_eq!(
            maximum_active_requests.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    });
}

#[test]
fn get_json_retries_rate_limit_and_honors_zero_retry_after() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("server address");
            let server = tokio::spawn(async move {
                for (status, headers, body) in [
                    ("429 Too Many Requests", "Retry-After: 0\r\n", "rate limited"),
                    ("200 OK", "", r#"{"ok":true}"#),
                ] {
                    let (mut stream, _) = listener.accept().await.expect("accept request");
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request).await.expect("read request");
                    let response = format!(
                        "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.expect("write response");
                }
            });
            let client = reqwest::Client::new();
            let body = get_json(
                &client,
                &format!("http://{address}/catalog"),
                None,
                "model listing",
            )
            .await
            .expect("second response succeeds");
            assert_eq!(body, serde_json::json!({"ok": true}));
            server.await.expect("server task");
        });
}

#[test]
fn post_json_retries_server_errors() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("server address");
            let server = tokio::spawn(async move {
                for (status, body) in [
                    ("503 Service Unavailable", "try again"),
                    ("200 OK", r#"{"ok":true}"#),
                ] {
                    let (mut stream, _) = listener.accept().await.expect("accept request");
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request).await.expect("read request");
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.expect("write response");
                }
            });
            let client = reqwest::Client::new();
            let response_body = post_json(
                &client,
                &format!("http://{address}/details"),
                &serde_json::json!({"model":"test"}),
                None,
                "model details",
            )
            .await
            .expect("retry response succeeds");
            assert_eq!(response_body, serde_json::json!({"ok": true}));
            server.await.expect("server task");
        });
}

#[test]
fn response_body_detail_returns_empty_for_blank_body() {
    assert_eq!(snippet_from_body(b"   "), "");
    assert_eq!(snippet_from_body(b""), "");
}

#[tokio::test]
async fn response_body_detail_stops_reading_a_huge_error_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("server address");
    // Announces a 1 GiB body, sends 64 KiB and then stalls: reading the whole
    // body would hang, reading a bounded prefix must not.
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.expect("read request");
        stream
            .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 1073741824\r\n\r\n")
            .await
            .expect("write head");
        stream
            .write_all(&vec![b'e'; 64 * 1024])
            .await
            .expect("write body prefix");
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let response = reqwest::get(format!("http://{address}/"))
        .await
        .expect("response head");
    let detail = tokio::time::timeout(Duration::from_secs(10), response_body_detail(response))
        .await
        .expect("a bounded prefix is read without waiting for the whole body");
    assert_eq!(detail.chars().count(), RESPONSE_BODY_SNIPPET_MAX_CHARS);
    server.abort();
}

#[tokio::test]
async fn provider_client_fails_a_response_that_stalls_after_headers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.expect("read request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
            .await
            .expect("write partial response");
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let client = provider_client_builder(Duration::from_secs(5), Duration::from_millis(200))
        .build()
        .expect("client");

    let response = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .expect("headers arrive");
    let error = response.bytes().await.expect_err("stalled body must fail");
    assert!(error.is_timeout());
    server.abort();
}

#[tokio::test]
async fn provider_client_fails_a_server_that_never_answers() {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("server address");
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept request");
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let client = provider_client_builder(Duration::from_secs(5), Duration::from_millis(200))
        .build()
        .expect("client");

    let error = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .expect_err("silent server must time out");
    assert!(error.is_timeout());
    server.abort();
}

#[test]
fn redacted_url_for_display_keeps_parameter_names_only() {
    assert_eq!(
        redacted_url_for_display(
            "https://user:pass@example.com:8443/v1/search?q=secret&page=2#frag"
        ),
        "https://<REDACTED>:<REDACTED>@example.com:8443/v1/search?q=<REDACTED>&page=<REDACTED>"
    );
    assert_eq!(
        redacted_url_for_display("http://user@example/api"),
        "http://<REDACTED>@example/api"
    );
    assert_eq!(
        redacted_url_for_display("http://example/api"),
        "http://example/api"
    );
    assert_eq!(
        redacted_url_for_display("http://example/api?flag"),
        "http://example/api?flag=<REDACTED>"
    );
    assert_eq!(
        redacted_url_for_display("not a url?token=secret"),
        "<invalid URL>"
    );
}

#[test]
fn status_error_redacts_query_values() {
    let message = status_error(
        reqwest::StatusCode::BAD_REQUEST,
        "https://user:pass@example.com/api?key=topsecret",
        "request",
        "",
        0,
        Duration::ZERO,
    )
    .to_string();
    assert_eq!(
        message,
        "request failed with HTTP 400 for https://<REDACTED>:<REDACTED>@example.com/api?key=<REDACTED>"
    );
}

#[test]
fn http_status_failure_logs_and_errors_without_query_values() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let client = reqwest::Client::new();
    let mut error_text = String::new();
    let logs = crate::test_support::capture_logs(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("server address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.expect("read request");
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write response");
        });
        let url = format!("http://{address}/search?q=topsecret&page=2");
        error_text = get_json(&client, &url, None, "search")
            .await
            .expect_err("400 fails")
            .to_string();
    });
    assert!(error_text.contains("/search?q=<REDACTED>&page=<REDACTED>"));
    assert!(logs.contains("/search?q=<REDACTED>&page=<REDACTED>"));
    for text in [&error_text, &logs] {
        assert!(!text.contains("topsecret"), "leaked query value: {text}");
    }
}

#[test]
fn connection_failure_logs_and_errors_without_query_values() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let url = format!("http://user:hunter2@{address}/search?q=topsecret");
    let client = reqwest::Client::new();
    let mut error_text = String::new();
    let logs = crate::test_support::capture_logs(async {
        tokio::time::pause();
        error_text = get_json(&client, &url, None, "search")
            .await
            .expect_err("refused connection fails")
            .to_string();
    });
    assert!(error_text.contains("/search?q=<REDACTED>"));
    assert!(error_text.contains("<REDACTED>:<REDACTED>@"));
    for text in [&error_text, &logs] {
        assert!(!text.contains("topsecret"), "leaked query value: {text}");
        assert!(!text.contains("hunter2"), "leaked userinfo: {text}");
        assert!(!text.contains("user:"), "leaked username: {text}");
    }
}

#[test]
fn concurrent_callers_pause_together_after_retry_after_and_all_succeed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const CALLERS: usize = 3;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build Tokio runtime");
    runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("server address");
        let arrivals = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Instant>::new()));
        let server_arrivals = std::sync::Arc::clone(&arrivals);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let arrivals = std::sync::Arc::clone(&server_arrivals);
                tokio::spawn(async move {
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request).await.expect("read request");
                    let attempt = {
                        let mut arrivals = arrivals.lock().expect("arrivals");
                        arrivals.push(Instant::now());
                        arrivals.len()
                    };
                    let (status, headers, body) = if attempt <= CALLERS {
                        ("429 Too Many Requests", "Retry-After: 1\r\n", "slow down")
                    } else {
                        ("200 OK", "", r#"{"ok":true}"#)
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.expect("write response");
                });
            }
        });
        let limiter = RequestConcurrencyLimiter::new(CALLERS);
        let client = reqwest::Client::new();
        let url = format!("http://{address}/chat");
        let callers = (0..CALLERS).map(|_| {
            let (limiter, client, url) = (limiter.clone(), client.clone(), url.clone());
            tokio::spawn(async move {
                post_with_retry_and_limiter(
                    &client,
                    &url,
                    &reqwest::header::HeaderMap::new(),
                    &serde_json::json!({}),
                    &CancellationToken::new(),
                    &limiter,
                )
                .await
                .map(|(response, _permit)| response.status())
            })
        });
        for caller in callers.collect::<Vec<_>>() {
            let status = caller.await.expect("caller task").expect("caller succeeds");
            assert_eq!(status, reqwest::StatusCode::OK);
        }
        let arrivals = arrivals.lock().expect("arrivals");
        let first_rejection = arrivals[0];
        assert_eq!(arrivals.len(), CALLERS * 2);
        for retry_arrival in &arrivals[CALLERS..] {
            assert!(
                *retry_arrival - first_rejection >= Duration::from_secs(1),
                "no retry may start before the shared Retry-After elapsed"
            );
        }
    });
}
