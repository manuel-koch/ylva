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

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

/// Maximum number of retries (after the initial attempt).
pub(crate) const MAX_RETRIES: usize = 3;
/// Base backoff for the first retry, in seconds.
pub(crate) const RETRY_BASE_SECONDS: u64 = 1;
/// Upper bound on a single backoff delay, in seconds.
pub(crate) const RETRY_MAX_SECONDS: u64 = 30;

/// Perform a `POST` with bounded exponential backoff retry for `429`/`5xx`/
/// connect failures, honoring `Retry-After` when present. Returns the first
/// successful response or a terminal [`anyhow::Error`].
///
/// `headers` carries any provider-specific auth (and other) request headers;
/// it is applied on every attempt.
pub(crate) async fn post_with_retry(
    client: &reqwest::Client,
    url: &str,
    headers: &reqwest::header::HeaderMap,
    body: &serde_json::Value,
    cancel: &CancellationToken,
) -> anyhow::Result<reqwest::Response> {
    let mut retries = 0usize;
    loop {
        let request = client.post(url).headers(headers.clone()).json(body);

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                if retries >= MAX_RETRIES {
                    return Err(anyhow::anyhow!(
                        "request to {url} failed after {MAX_RETRIES} retries: {error}"
                    ));
                }
                retries += 1;
                if sleep_cancel_aware(cancel, retry_delay(retries, None))
                    .await
                    .is_none()
                {
                    return Err(anyhow::anyhow!("cancelled while waiting to retry"));
                }
                continue;
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        if !should_retry(status) {
            let detail = response_body_detail(response).await;
            return Err(provider_status_error(status, url, &detail, retries));
        }

        if retries >= MAX_RETRIES {
            let detail = response_body_detail(response).await;
            return Err(provider_status_error(status, url, &detail, retries));
        }

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        retries += 1;
        if sleep_cancel_aware(cancel, retry_delay(retries, retry_after))
            .await
            .is_none()
        {
            return Err(anyhow::anyhow!("cancelled while waiting to retry"));
        }
    }
}

/// Whether an HTTP status is worth a retry: rate limit or server error.
pub(crate) fn should_retry(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Read a bounded snippet of the response body to attach to an error message,
/// so backend-specific explanations surface instead of a bare status code.
/// Returns an empty string when the body cannot be read or is blank.
pub(crate) async fn response_body_detail(response: reqwest::Response) -> String {
    match response.bytes().await {
        Ok(body) => snippet_from_body(&body),
        Err(_) => String::new(),
    }
}

/// Format a bounded snippet (≤512 chars) from a raw response body for use in an
/// error message. Returns an empty string for a blank body.
fn snippet_from_body(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    text.chars().take(512).collect()
}

/// Build a terminal error for an HTTP `status`.
/// Records the number of `retries` already performed when non-zero.
fn provider_status_error(
    status: reqwest::StatusCode,
    url: &str,
    body_snippet: &str,
    retries: usize,
) -> anyhow::Error {
    let status_number = status.as_u16();
    let body = if body_snippet.is_empty() {
        String::new()
    } else {
        format!(": {body_snippet}")
    };
    let retry_note = if retries > 0 {
        format!(" after {retries} retries")
    } else {
        String::new()
    };
    anyhow::anyhow!["provider returned HTTP {status_number}{body} for {url}{retry_note}"]
}

/// Compute the next backoff delay. `Retry-After` (in seconds) wins
/// when present; otherwise exponential backoff (`base * 2^(attempt-1)`) capped
/// at [`RETRY_MAX_SECONDS`], with a small jitter.
pub(crate) fn retry_delay(attempt: usize, retry_after: Option<u64>) -> Duration {
    if let Some(seconds) = retry_after {
        return Duration::from_secs(seconds.min(RETRY_MAX_SECONDS));
    }
    let exponent = attempt.saturating_sub(1).min(63);
    let base = RETRY_BASE_SECONDS.saturating_mul(1u64 << exponent);
    let capped = base.min(RETRY_MAX_SECONDS);
    let jitter = jitter_fraction();
    Duration::from_millis((capped as f64 * jitter * 1000.0) as u64)
}

/// A tiny deterministic-per-call jitter in `[0, 1)`, seeded from the wall clock
/// so successive attempts back off differently without a dedicated RNG crate.
fn jitter_fraction() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut state = (nanos & 0xFFFF_FFFF) as u64 | 1;
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D) as f64 / u64::MAX as f64
}

/// Sleep for `delay` unless `cancel` fires first; returns `Some` only when the
/// full delay elapsed, so a stop aborts a retry wait promptly.
pub(crate) async fn sleep_cancel_aware(cancel: &CancellationToken, delay: Duration) -> Option<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep(delay) => Some(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_honors_retry_after() {
        assert_eq!(retry_delay(1, Some(2)), Duration::from_secs(2));
        assert_eq!(
            retry_delay(5, Some(9999)),
            Duration::from_secs(RETRY_MAX_SECONDS)
        );
    }

    #[test]
    fn retry_delay_backs_off_bounded_by_max() {
        assert!(retry_delay(1, None) < Duration::from_secs(2));
        let first = retry_delay(2, None);
        assert!(first >= Duration::from_millis(1));
        let late = retry_delay(40, None);
        assert!(late <= Duration::from_secs(RETRY_MAX_SECONDS));
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
    fn snippet_from_body_is_bounded_to_512_chars() {
        let long_body = "x".repeat(2000);
        let snippet = snippet_from_body(long_body.as_bytes());
        assert!(snippet.chars().count() == 512);
        assert_eq!(snippet, "x".repeat(512));
    }

    #[test]
    fn provider_status_error_joins_snippet_only_when_present() {
        let message = provider_status_error(
            reqwest::StatusCode::NOT_FOUND,
            "http://example/api",
            "not found on server",
            0,
        )
        .to_string();
        assert_eq!(
            message,
            "provider returned HTTP 404: not found on server for http://example/api"
        );

        let blank =
            provider_status_error(reqwest::StatusCode::NOT_FOUND, "http://example/api", "", 0)
                .to_string();
        assert_eq!(blank, "provider returned HTTP 404 for http://example/api");

        let exhausted = provider_status_error(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "http://example/api",
            "down",
            MAX_RETRIES,
        )
        .to_string();
        assert_eq!(
            exhausted,
            format!(
                "provider returned HTTP 503: down for http://example/api after {MAX_RETRIES} retries"
            )
        );
    }

    #[test]
    fn provider_status_error_reports_prior_retries_on_non_retryable_status() {
        // Two retries happened before a final non-retryable 400; the message
        // must not claim no retry was performed.
        let message = provider_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            "http://example/api",
            "bad",
            2,
        )
        .to_string();
        assert_eq!(
            message,
            "provider returned HTTP 400: bad for http://example/api after 2 retries"
        );
    }

    #[test]
    fn response_body_detail_returns_empty_for_blank_body() {
        assert_eq!(snippet_from_body(b"   "), "");
        assert_eq!(snippet_from_body(b""), "");
    }
}
