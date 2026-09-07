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

use std::path::{Path, PathBuf};
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
/// it is applied on every attempt. `request_dump_path` is computed once by
/// the caller (via [`build_request_dump_path`] with a shared
/// [`current_dump_timestamp`]) rather than inside this function so the
/// caller can derive a paired response-dump path (via
/// [`build_response_dump_path`]) using that exact same timestamp —
/// computing the timestamp twice could otherwise land the request and its
/// streamed response in differently-named files.
pub(crate) async fn post_with_retry(
    client: &reqwest::Client,
    url: &str,
    headers: &reqwest::header::HeaderMap,
    body: &serde_json::Value,
    cancel: &CancellationToken,
    request_dump_path: Option<&Path>,
) -> anyhow::Result<reqwest::Response> {
    if let Some(dump_path) = request_dump_path {
        dump_request_body(body, dump_path);
    }

    let mut retries = 0usize;
    loop {
        let request = client.post(url).headers(headers.clone()).json(body);

        // Honour the cancel token during the initial HTTP round-trip.  The
        // LLM server may take tens of seconds before sending back 200 +
        // headers; without this check an in-flight `request.send()` would
        // keep the request alive until the server finally responds, making
        // cancellation appear stuck for the entire server-side think time.
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(anyhow::anyhow!("cancelled"));
            }
            result = request.send() => result,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if retries >= MAX_RETRIES {
                    tracing::error!(url, retry = retries, error = %error, "request failed after retries");
                    return Err(anyhow::anyhow!(
                        "request to {url} failed after {MAX_RETRIES} retries: {error}"
                    ));
                }
                retries += 1;
                tracing::warn!(url, retry = retries, error = %error, "request failed; retrying");
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
            tracing::error!(url, status = status.as_u16(), "non-retryable status");
            return Err(provider_status_error(status, url, &detail, retries));
        }

        if retries >= MAX_RETRIES {
            let detail = response_body_detail(response).await;
            tracing::error!(
                url,
                status = status.as_u16(),
                retry = retries,
                "retryable status but retries exhausted"
            );
            return Err(provider_status_error(status, url, &detail, retries));
        }

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        retries += 1;
        tracing::warn!(
            url,
            status = status.as_u16(),
            retry = retries,
            "retryable status; retrying"
        );
        if sleep_cancel_aware(cancel, retry_delay(retries, retry_after))
            .await
            .is_none()
        {
            return Err(anyhow::anyhow!("cancelled while waiting to retry"));
        }
    }
}

/// Dump `body` as pretty-printed JSON to `file_path`.
///
/// The parent directory of `file_path` is created if it does not yet exist.
/// Errors are logged at `warn` level and silently ignored so a dump failure
/// never breaks a live request.
pub(crate) fn dump_request_body(body: &serde_json::Value, file_path: &Path) {
    if let Err(error) = std::fs::create_dir_all(file_path.parent().unwrap_or(Path::new("."))) {
        tracing::warn!(path = %file_path.display(), error = %error, "failed to create request dump dir");
        return;
    }

    match serde_json::to_string_pretty(body) {
        Ok(json_text) => {
            if let Err(error) = std::fs::write(file_path, json_text) {
                tracing::warn!(path = %file_path.display(), error = %error, "failed to write request dump file");
            } else {
                tracing::debug!(path = %file_path.display(), "dumped request body");
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "failed to serialize request body for dump");
        }
    }
}

/// Append one line to a response dump file, creating it (and its parent
/// directory) on first write.
///
/// Writes append-and-flush per call so a partial/cancelled stream still
/// leaves whatever was received on disk. Mirrors [`dump_request_body`]'s
/// failure handling: I/O errors are logged at `warn` and silently ignored so
/// a dump failure never breaks a live response.
pub(crate) fn append_response_dump_line(line: &str, file_path: &Path) {
    use std::io::Write;

    if let Err(error) = std::fs::create_dir_all(file_path.parent().unwrap_or(Path::new("."))) {
        tracing::warn!(path = %file_path.display(), error = %error, "failed to create response dump dir");
        return;
    }

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
    {
        Ok(mut file) => {
            if let Err(error) = writeln!(file, "{line}") {
                tracing::warn!(path = %file_path.display(), error = %error, "failed to append response dump line");
            }
        }
        Err(error) => {
            tracing::warn!(path = %file_path.display(), error = %error, "failed to open response dump file");
        }
    }
}

/// Derive the dump file path for the outgoing request body from the
/// `YLVA_REQUEST_DUMP_PATH` environment variable, embedding `timestamp` (see
/// [`current_dump_timestamp`]) so it can be paired with a response dump built
/// from the same `timestamp` via [`build_response_dump_path`].
///
/// Returns `None` when the envvar is unset, empty, or whitespace-only.
/// Delegates path construction to [`build_dump_path_from_template`].
pub(crate) fn build_request_dump_path(timestamp: &str) -> Option<PathBuf> {
    let template = std::env::var("YLVA_REQUEST_DUMP_PATH").ok()?;
    build_dump_path_from_template(&template, timestamp, "request", "json")
}

/// Derive the dump file path for the raw SSE transcript of a streamed
/// response from the `YLVA_REQUEST_DUMP_PATH` environment variable, embedding
/// `timestamp` (see [`current_dump_timestamp`]) so it can be paired with the
/// request dump built from the same `timestamp` via [`build_request_dump_path`].
///
/// Returns `None` when the envvar is unset, empty, or whitespace-only.
/// Delegates path construction to [`build_dump_path_from_template`].
pub(crate) fn build_response_dump_path(timestamp: &str) -> Option<PathBuf> {
    let template = std::env::var("YLVA_REQUEST_DUMP_PATH").ok()?;
    build_dump_path_from_template(&template, timestamp, "response", "jsonl")
}

/// A shared `<unix_seconds>_<millis>` timestamp component, computed once per
/// chat exchange and embedded in both [`build_request_dump_path`] and
/// [`build_response_dump_path`] so the request and its paired streamed
/// response land in matching, easy-to-find filenames.
pub(crate) fn current_dump_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    format!("{}_{:03}", now.as_secs(), now.subsec_millis())
}

/// Construct a dump file path from `template`, embedding `timestamp`, `kind`
/// (`"request"` or `"response"`), and `extension` in the filename.
///
/// The template is treated as a path: the parent directory and stem are
/// extracted from it; the actual file is named
/// `<stem>_<timestamp>_<kind>.<extension>` (stem falls back to `"ylva_req"`
/// when the template has no file component). A relative parent directory is
/// resolved against the current working directory; failure to resolve the cwd
/// is logged at `warn` level and causes `None` to be returned.
fn build_dump_path_from_template(
    template: &str,
    timestamp: &str,
    kind: &str,
    extension: &str,
) -> Option<PathBuf> {
    if template.trim().is_empty() {
        return None;
    }
    let template_path = Path::new(template);

    let parent = template_path.parent().unwrap_or(Path::new(""));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let dump_dir = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(parent),
            Err(error) => {
                tracing::warn!(error = %error, "failed to resolve CWD for YLVA_REQUEST_DUMP_PATH");
                return None;
            }
        }
    };

    let stem = template_path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("ylva_req");

    let file_name = format!("{stem}_{timestamp}_{kind}.{extension}");
    Some(dump_dir.join(file_name))
}

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

    #[test]
    fn build_dump_path_strips_extension_from_template() {
        let path =
            build_dump_path_from_template("/tmp/dumps/ylva.json", "1_002", "request", "json")
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
        assert_eq!(file_name, "ylva_1_002_request.json");
    }

    #[test]
    fn build_dump_path_uses_stem_and_absolute_dir() {
        let path = build_dump_path_from_template("/tmp/dumps/my_req", "1_002", "response", "jsonl")
            .expect("path");
        let parent = path.parent().expect("parent");
        let file_name = path.file_name().and_then(|n| n.to_str()).expect("name");
        assert_eq!(parent, Path::new("/tmp/dumps"));
        assert_eq!(file_name, "my_req_1_002_response.jsonl");
    }

    #[test]
    fn build_request_dump_path_returns_none_when_env_var_unset() {
        // Guard: ensure the var is absent for this check. Because we are not
        // setting it, there is no race with other tests.
        if std::env::var("YLVA_REQUEST_DUMP_PATH").is_ok() {
            return; // var happens to be set in this process — skip rather than flake
        }
        assert!(build_request_dump_path("1_002").is_none());
    }

    #[test]
    fn build_dump_path_returns_none_when_template_is_whitespace_only() {
        assert!(build_dump_path_from_template("   ", "1_002", "request", "json").is_none());
    }

    #[test]
    fn build_dump_path_falls_back_to_default_stem_when_template_has_no_stem() {
        let path = build_dump_path_from_template("/", "1_002", "request", "json").expect("path");
        let file_name = path.file_name().and_then(|n| n.to_str()).expect("name");
        assert_eq!(file_name, "ylva_req_1_002_request.json");
    }

    #[test]
    fn build_dump_path_resolves_relative_template_against_cwd() {
        let cwd = std::env::current_dir().expect("cwd");
        let path =
            build_dump_path_from_template("sub/req", "1_002", "request", "json").expect("path");
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
    fn dump_request_body_writes_pretty_json_to_given_path() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("request.json");
        let body = serde_json::json!({"model": "test", "messages": []});

        dump_request_body(&body, &file_path);

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file_path).expect("read file"))
                .expect("valid json");
        assert_eq!(written, body);
    }

    #[test]
    fn dump_request_body_creates_parent_dir_when_missing() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("nested/dir/request.json");
        let body = serde_json::json!({"nested": true});

        dump_request_body(&body, &file_path);

        assert!(file_path.exists(), "file should have been created");
    }

    #[test]
    fn build_request_and_response_dump_paths_share_timestamp_and_differ_by_kind_and_extension() {
        let timestamp = "1_002";
        let request_path =
            build_dump_path_from_template("/tmp/dumps/ylva_req", timestamp, "request", "json")
                .expect("request path");
        let response_path =
            build_dump_path_from_template("/tmp/dumps/ylva_req", timestamp, "response", "jsonl")
                .expect("response path");

        assert_eq!(
            request_path,
            Path::new("/tmp/dumps/ylva_req_1_002_request.json")
        );
        assert_eq!(
            response_path,
            Path::new("/tmp/dumps/ylva_req_1_002_response.jsonl")
        );
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
}
