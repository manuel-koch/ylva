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

//! HTTP constants and helpers shared by every outbound request Ylva
//! triggers, regardless of which subsystem issues it (LLM providers,
//! web-search backends, …).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// `User-Agent` value applied as a client-level default on every outbound
/// HTTP request so remote servers identify Ylva and its package version.
///
/// Single source of truth: never re-declare the `ylva/<version>` pattern at a
/// call site.
pub const OUTBOUND_USER_AGENT: &str = concat!("ylva/", env!("CARGO_PKG_VERSION"));

/// Maximum number of retries (after the initial attempt).
pub(crate) const MAX_RETRIES: usize = 3;
/// Base backoff for the first retry, in seconds.
pub(crate) const RETRY_BASE_SECONDS: u64 = 1;
/// Upper bound on a single backoff delay, in seconds.
pub(crate) const RETRY_MAX_SECONDS: u64 = 30;

/// Source for a request-dump path template.
#[derive(Debug, Clone, Default)]
pub(crate) enum RequestDumpSource {
    /// Resolve `YLVA_REQUEST_DUMP_PATH` when the request starts.
    #[default]
    Environment,
    /// Use this explicit template instead of the process environment.
    Template(PathBuf),
}

impl RequestDumpSource {
    /// Use an explicit path template for request and response dumps.
    pub(crate) fn from_template(path: impl Into<PathBuf>) -> Self {
        RequestDumpSource::Template(path.into())
    }

    fn template_path(&self) -> Option<PathBuf> {
        match self {
            RequestDumpSource::Environment => {
                std::env::var_os("YLVA_REQUEST_DUMP_PATH").map(PathBuf::from)
            }
            RequestDumpSource::Template(path) => Some(path.clone()),
        }
    }
}

/// Identity shared by the request dump and its paired response dump.
#[derive(Debug, Clone)]
pub(crate) struct RequestDumpContext {
    template: Option<String>,
    timestamp: String,
    dump_id: String,
}

impl RequestDumpContext {
    /// Resolve the source once at request start so request and response dumps
    /// cannot be split across different environment values.
    pub(crate) fn new(source: &RequestDumpSource) -> Self {
        let template = source
            .template_path()
            .and_then(|path| path.to_str().map(str::to_owned));
        RequestDumpContext {
            template,
            timestamp: current_dump_timestamp(),
            dump_id: Uuid::new_v4().simple().to_string(),
        }
    }

    pub(crate) fn dump_id(&self) -> &str {
        &self.dump_id
    }

    pub(crate) fn request_path(&self) -> Option<PathBuf> {
        self.path("request", "jsonl")
    }

    pub(crate) fn response_path_for_content_type(
        &self,
        content_type: Option<&str>,
        fallback_extension: &str,
    ) -> Option<PathBuf> {
        self.path(
            "response",
            response_dump_extension(content_type, fallback_extension),
        )
    }

    fn path(&self, kind: &str, extension: &str) -> Option<PathBuf> {
        build_dump_path_from_template(
            self.template.as_deref()?,
            &self.timestamp,
            &self.dump_id,
            kind,
            extension,
        )
    }
}

/// Perform a JSON `GET` request with optional bearer authentication.
pub(crate) async fn get_json(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    get_json_with_dump_source(
        client,
        url,
        api_key,
        operation,
        &RequestDumpSource::Environment,
    )
    .await
}

pub(crate) async fn get_json_with_dump_source(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    operation: &str,
    dump_source: &RequestDumpSource,
) -> anyhow::Result<serde_json::Value> {
    let headers = bearer_headers(api_key)?;
    let dump_context = RequestDumpContext::new(dump_source);
    let request_dump_path = dump_context.request_path();
    if let Some(path) = &request_dump_path {
        dump_request(
            url,
            &redacted_headers_json(&headers),
            &serde_json::Value::Null,
            path,
        );
    }
    let response = match client.get(url).headers(headers).send().await {
        Ok(response) => response,
        Err(error) => {
            let response_dump_path = dump_context.response_path_for_content_type(None, "jsonl");
            dump_response_transport_error(&response_dump_path, &error);
            return Err(error.into());
        }
    };
    let status = response.status();
    let body = read_buffered_response(response, &dump_context).await?;
    anyhow::ensure!(status.is_success(), "{operation} failed with HTTP {status}");
    Ok(serde_json::from_slice(&body)?)
}

/// Perform a JSON `POST` request with optional bearer authentication.
pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: Option<&str>,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    post_json_with_dump_source(
        client,
        url,
        body,
        api_key,
        operation,
        &RequestDumpSource::Environment,
    )
    .await
}

pub(crate) async fn post_json_with_dump_source(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: Option<&str>,
    operation: &str,
    dump_source: &RequestDumpSource,
) -> anyhow::Result<serde_json::Value> {
    let headers = bearer_headers(api_key)?;
    let dump_context = RequestDumpContext::new(dump_source);
    let request_dump_path = dump_context.request_path();
    if let Some(path) = &request_dump_path {
        dump_request(url, &redacted_headers_json(&headers), body, path);
    }
    let response = match client.post(url).headers(headers).json(body).send().await {
        Ok(response) => response,
        Err(error) => {
            let response_dump_path = dump_context.response_path_for_content_type(None, "jsonl");
            dump_response_transport_error(&response_dump_path, &error);
            return Err(error.into());
        }
    };
    let status = response.status();
    let response_body = read_buffered_response(response, &dump_context).await?;
    anyhow::ensure!(status.is_success(), "{operation} failed with HTTP {status}");
    Ok(serde_json::from_slice(&response_body)?)
}

fn bearer_headers(api_key: Option<&str>) -> anyhow::Result<reqwest::header::HeaderMap> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(api_key) = api_key {
        let authorization = format!("Bearer {api_key}");
        headers.insert(
            reqwest::header::AUTHORIZATION,
            authorization
                .parse()
                .map_err(|error| anyhow::anyhow!("invalid authorization header value: {error}"))?,
        );
    }
    Ok(headers)
}

/// Read and dump a complete non-streaming response body before the caller
/// checks its status or parses its content.
async fn read_buffered_response(
    response: reqwest::Response,
    dump_context: &RequestDumpContext,
) -> anyhow::Result<Vec<u8>> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let response_dump_path = dump_context.response_path_for_content_type(content_type, "bin");
    match response.bytes().await {
        Ok(body) => {
            dump_buffered_response(&body, response_dump_path.as_deref());
            Ok(body.to_vec())
        }
        Err(error) => {
            dump_response_transport_error(&response_dump_path, &error);
            Err(error.into())
        }
    }
}

/// Return the file extension appropriate for a response `Content-Type`.
///
/// The fallback is used when the server omits or provides an unrecognized
/// media type. It is chosen by the caller because streaming and buffered
/// responses have different safe fallbacks.
pub(crate) fn response_dump_extension<'a>(
    content_type: Option<&str>,
    fallback_extension: &'a str,
) -> &'a str {
    let Some(media_type) = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return fallback_extension;
    };
    let media_type = media_type.to_ascii_lowercase();
    match media_type.as_str() {
        "text/event-stream" | "application/ndjson" | "application/x-ndjson" => "jsonl",
        "application/json" | "application/problem+json" => "json",
        value if value.ends_with("+json") => "json",
        "text/html" | "application/xhtml+xml" => "html",
        "text/plain" => "txt",
        "text/css" => "css",
        "text/javascript" | "application/javascript" => "js",
        "text/xml" | "application/xml" => "xml",
        "application/pdf" => "pdf",
        "image/gif" => "gif",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        _ => fallback_extension,
    }
}

/// Write a complete buffered response as its original bytes.
///
/// The response path's extension is selected from its `Content-Type` before
/// this function is called. Unlike SSE response dumps, buffered bodies are not
/// converted to JSON Lines or otherwise escaped.
pub(crate) fn dump_buffered_response(body: &[u8], response_dump_path: Option<&Path>) {
    let Some(path) = response_dump_path else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))) {
        tracing::warn!(path = %path.display(), error = %error, "failed to create response dump dir");
        return;
    }
    match std::fs::write(path, body) {
        Ok(()) => tracing::debug!(path = %path.display(), "dumped buffered response"),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to write response dump file");
        }
    }
}

pub(crate) fn dump_response_transport_error(
    response_dump_path: &Option<PathBuf>,
    error: &impl std::fmt::Display,
) {
    if let Some(path) = response_dump_path {
        append_response_dump_line(&format!("[transport] response error: {error}"), path);
    }
}

/// Perform a `POST` with bounded exponential backoff retry for `429`/`5xx`/
/// connect failures, honoring `Retry-After` when present. Returns the first
/// successful response or a terminal [`anyhow::Error`].
///
/// `headers` carries any caller-specific auth (and other) request headers;
/// it is applied on every attempt. Request diagnostics are handled by the
/// caller so this function remains focused on transport and retry behavior.
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

        // Honour the cancel token during the initial HTTP round-trip.  The
        // remote server may take tens of seconds before sending back 200 +
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
            return Err(status_error(status, url, &detail, retries));
        }

        if retries >= MAX_RETRIES {
            let detail = response_body_detail(response).await;
            tracing::error!(
                url,
                status = status.as_u16(),
                retry = retries,
                "retryable status but retries exhausted"
            );
            return Err(status_error(status, url, &detail, retries));
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

/// Header names whose value is redacted before ever reaching the request
/// dump — credentials/session tokens must never land in a debug artifact on
/// disk. Matched case-insensitively against the header name.
const SENSITIVE_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
];

/// Placeholder written in place of any [`is_sensitive_header_name`] header's
/// value in a request dump.
const REDACTED_HEADER_VALUE: &str = "<REDACTED>";

/// `true` when `name` is a known credential-carrying header (see
/// [`SENSITIVE_HEADER_NAMES`]).
fn is_sensitive_header_name(name: &str) -> bool {
    SENSITIVE_HEADER_NAMES
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// Derive the `scheme://host` network-sandbox match key of a URL — the
/// string [`crate::sandbox::SandboxContext::check_network_request`] matches
/// `network_request` rule `target` patterns against (explicit port included
/// when present in the URL).
///
/// Returns `None` for URLs ylva may not request: unparsable URLs, URLs
/// without a host, and everything that is not an absolute `http`/`https`
/// URL (the only schemes the web backends fetch).
pub(crate) fn network_target(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
        None => format!("{}://{}", parsed.scheme(), host),
    })
}

/// Render `headers` as a JSON object for the request dump, replacing the
/// value of any [`is_sensitive_header_name`] header with
/// [`REDACTED_HEADER_VALUE`] so an `Authorization`/API-key value is never
/// written to disk. Multiple values for the same header name are joined with
/// `", "`, matching how the wire form of a repeated header is usually
/// rendered.
pub(crate) fn redacted_headers_json(headers: &reqwest::header::HeaderMap) -> serde_json::Value {
    let mut redacted = serde_json::Map::new();
    for name in headers.keys() {
        let value = if is_sensitive_header_name(name.as_str()) {
            REDACTED_HEADER_VALUE.to_string()
        } else {
            headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().unwrap_or("<non-utf8>"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        redacted.insert(name.as_str().to_string(), serde_json::Value::String(value));
    }
    serde_json::Value::Object(redacted)
}

/// Dump an outgoing request to `file_path` as two JSON Lines: a metadata line
/// (URL scheme/host/path/query plus an already-redacted headers projection) followed
/// by an already-redacted request body, compact (not
/// pretty-printed, so it stays on its own single line).
///
/// The parent directory of `file_path` is created if it does not yet exist.
/// Errors are logged at `warn` level and silently ignored so a dump failure
/// never breaks a live request.
pub(crate) fn dump_request(
    url: &str,
    redacted_headers: &serde_json::Value,
    redacted_body: &serde_json::Value,
    file_path: &Path,
) {
    if let Err(error) = std::fs::create_dir_all(file_path.parent().unwrap_or(Path::new("."))) {
        tracing::warn!(path = %file_path.display(), error = %error, "failed to create request dump dir");
        return;
    }

    let parsed_url = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(url, error = %error, "failed to parse request URL for dump");
            return;
        }
    };
    let meta = serde_json::json!({
        "scheme": parsed_url.scheme(),
        "host": parsed_url.host_str().unwrap_or(""),
        "path": parsed_url.path(),
        "query": parsed_url.query(),
        "headers": redacted_headers,
    });

    let (meta_line, body_line) = match (
        serde_json::to_string(&meta),
        serde_json::to_string(redacted_body),
    ) {
        (Ok(meta_line), Ok(body_line)) => (meta_line, body_line),
        (meta_result, body_result) => {
            if let Err(error) = meta_result {
                tracing::warn!(error = %error, "failed to serialize request metadata for dump");
            }
            if let Err(error) = body_result {
                tracing::warn!(error = %error, "failed to serialize request body for dump");
            }
            return;
        }
    };

    match std::fs::write(file_path, format!("{meta_line}\n{body_line}\n")) {
        Ok(()) => tracing::debug!(path = %file_path.display(), "dumped request"),
        Err(error) => {
            tracing::warn!(path = %file_path.display(), error = %error, "failed to write request dump file");
        }
    }
}

/// Append one line to a response dump file, creating it (and its parent
/// directory) on first write.
///
/// Writes append-and-flush per call so a partial/cancelled stream still
/// leaves whatever was received on disk. Mirrors [`dump_request`]'s failure
/// handling: I/O errors are logged at `warn` and silently ignored so a dump
/// failure never breaks a live response.
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

/// A human-readable `<unix_seconds>_<millis>` timestamp component, computed
/// once per request and embedded in both dump paths for easy sorting.
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
    dump_id: &str,
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

    let file_name = format!("{stem}_{timestamp}_{dump_id}_{kind}.{extension}");
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

/// Maximum characters of a response-body snippet attached to an error message.
const RESPONSE_BODY_SNIPPET_MAX_CHARS: usize = 512;

/// Format a bounded snippet (≤[`RESPONSE_BODY_SNIPPET_MAX_CHARS`] chars) from
/// a raw response body for use in an error message. Returns an empty string
/// for a blank body.
fn snippet_from_body(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    text.chars().take(RESPONSE_BODY_SNIPPET_MAX_CHARS).collect()
}

/// Build a terminal error for an HTTP `status`.
/// Records the number of `retries` already performed when non-zero.
fn status_error(
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
    anyhow::anyhow!["server returned HTTP {status_number}{body} for {url}{retry_note}"]
}

/// Compute the next backoff delay. `Retry-After` (in seconds) wins
/// when present; otherwise exponential backoff (`base * 2^(attempt-1)`) capped
/// at [`RETRY_MAX_SECONDS`], with a small jitter.
pub(crate) fn retry_delay(attempt: usize, retry_after: Option<u64>) -> Duration {
    retry_delay_with_jitter(attempt, retry_after, jitter_fraction())
}

/// Pure core of [`retry_delay`] with the jitter fraction injected, so the
/// delay bounds are asserted deterministically instead of depending on the
/// wall clock (the established pure-core test pattern). Full jitter is
/// sampled from `[0, 1)`, so the wrapper may legally produce a near-zero
/// delay; bounds that must always hold are stated against fixed fractions.
fn retry_delay_with_jitter(attempt: usize, retry_after: Option<u64>, jitter: f64) -> Duration {
    if let Some(seconds) = retry_after {
        return Duration::from_secs(seconds.min(RETRY_MAX_SECONDS));
    }
    let exponent = attempt.saturating_sub(1).min(63);
    let base = RETRY_BASE_SECONDS.saturating_mul(1u64 << exponent);
    let capped = base.min(RETRY_MAX_SECONDS);
    Duration::from_millis((capped as f64 * jitter * 1000.0) as u64)
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

/// A tiny deterministic-per-call jitter in `[0, 1)`, seeded from the wall clock
/// so successive requests back off differently without a dedicated RNG crate.
pub(crate) fn jitter_fraction() -> f64 {
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

/// A random delay in `[0, fraction * interval)` — the shared jitter primitive
/// for pacing outbound requests (retry backoff, web-search throttling).
pub(crate) fn jitter_duration(interval: Duration, fraction: f64) -> Duration {
    Duration::from_secs_f64(interval.as_secs_f64() * fraction * jitter_fraction())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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
            "not found on server",
            0,
        )
        .to_string();
        assert_eq!(
            message,
            "server returned HTTP 404: not found on server for http://example/api"
        );

        let blank =
            status_error(reqwest::StatusCode::NOT_FOUND, "http://example/api", "", 0).to_string();
        assert_eq!(blank, "server returned HTTP 404 for http://example/api");

        let exhausted = status_error(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "http://example/api",
            "down",
            MAX_RETRIES,
        )
        .to_string();
        assert_eq!(
            exhausted,
            format!(
                "server returned HTTP 503: down for http://example/api after {MAX_RETRIES} retries"
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
            "bad",
            2,
        )
        .to_string();
        assert_eq!(
            message,
            "server returned HTTP 400: bad for http://example/api after 2 retries"
        );
    }

    #[test]
    fn response_body_detail_returns_empty_for_blank_body() {
        assert_eq!(snippet_from_body(b"   "), "");
        assert_eq!(snippet_from_body(b""), "");
    }

    #[test]
    fn build_dump_path_strips_extension_from_template() {
        let path = build_dump_path_from_template(
            "/tmp/dumps/ylva.json",
            "1_002",
            "dump-a",
            "request",
            "json",
        )
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
        let path = build_dump_path_from_template(
            "/tmp/dumps/my_req",
            "1_002",
            "dump-a",
            "response",
            "jsonl",
        )
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
            .expect("runtime");

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
        assert!(
            build_dump_path_from_template("   ", "1_002", "dump-a", "request", "json").is_none()
        );
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
        assert_eq!(meta["headers"]["x-custom"], "not-secret");
        assert!(
            !contents.contains("super-secret-key") && !contents.contains("another-secret"),
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
        let request_path = build_dump_path_from_template(
            "/tmp/dumps/ylva_req",
            timestamp,
            dump_id,
            "request",
            "json",
        )
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
        let first_path = build_dump_path_from_template(
            "/tmp/dumps/ylva_req",
            "1_002",
            "dump-a",
            "request",
            "jsonl",
        )
        .expect("first path");
        let second_path = build_dump_path_from_template(
            "/tmp/dumps/ylva_req",
            "1_002",
            "dump-b",
            "request",
            "jsonl",
        )
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
}
