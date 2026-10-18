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

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bounded_buffer::BoundedBuffer;

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
/// A caller-visible handle that the provider binds to the response dump paired
/// with a request. It lets caller-side timeouts append their failure reason to
/// the right response artifact even after stream processing has been dropped.
#[derive(Clone)]
pub struct RequestDumpFailureHandler {
    callback: std::sync::Arc<std::sync::Mutex<Option<ResponseDumpFailureCallback>>>,
}

type ResponseDumpFailureCallback = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

impl Default for RequestDumpFailureHandler {
    fn default() -> Self {
        Self {
            callback: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl RequestDumpFailureHandler {
    pub(crate) fn register(&self, callback: impl Fn(&str) + Send + Sync + 'static) {
        *self
            .callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(std::sync::Arc::new(callback));
    }

    pub(crate) fn report(&self, failure: &str) {
        let callback = self
            .callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(callback) = callback {
            callback(failure);
        }
    }
}
/// Generic concurrency budget for a class of HTTP requests.
#[derive(Debug, Clone)]
pub(crate) struct RequestConcurrencyLimiter {
    semaphore: std::sync::Arc<Semaphore>,
}

impl RequestConcurrencyLimiter {
    pub(crate) fn new(max_concurrent_requests: usize) -> Self {
        Self {
            semaphore: std::sync::Arc::new(Semaphore::new(max_concurrent_requests.max(1))),
        }
    }

    pub(crate) async fn acquire(&self) -> anyhow::Result<OwnedSemaphorePermit> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP request concurrency limiter is closed"))
    }
}
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

    /// Append a terminal failure to this request's response dump when request
    /// dumps are enabled. The generic JSONL path is used because a timeout may
    /// happen before response headers reveal a content type.
    pub(crate) fn dump_failure_response(&self, failure: &(impl std::fmt::Display + ?Sized)) {
        if let Some(path) = self.response_path_for_content_type(None, "jsonl") {
            append_response_dump_line(&format!("[failure] {failure}"), &path);
        }
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
    let response = send_with_retry(url, operation, &CancellationToken::new(), || {
        client.get(url).headers(headers.clone())
    })
    .await?;
    let body = read_buffered_response(response, &dump_context).await?;
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
    let response = send_with_retry(url, operation, &CancellationToken::new(), || {
        client.post(url).headers(headers.clone()).json(body)
    })
    .await?;
    let response_body = read_buffered_response(response, &dump_context).await?;
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

/// Read and dump a complete successful response body before parsing its content.
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
    send_with_retry(url, "chat completion", cancel, || {
        client.post(url).headers(headers.clone()).json(body)
    })
    .await
}

pub(crate) async fn post_with_retry_and_limiter(
    client: &reqwest::Client,
    url: &str,
    headers: &reqwest::header::HeaderMap,
    body: &serde_json::Value,
    cancel: &CancellationToken,
    limiter: &RequestConcurrencyLimiter,
) -> anyhow::Result<(reqwest::Response, OwnedSemaphorePermit)> {
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(anyhow::anyhow!("cancelled")),
        permit = limiter.acquire() => permit?,
    };
    let response = post_with_retry(client, url, headers, body, cancel).await?;
    Ok((response, permit))
}

async fn send_with_retry<RequestBuilderFactory>(
    url: &str,
    operation: &str,
    cancel: &CancellationToken,
    mut build_request: RequestBuilderFactory,
) -> anyhow::Result<reqwest::Response>
where
    RequestBuilderFactory: FnMut() -> reqwest::RequestBuilder,
{
    let mut retries = 0usize;
    loop {
        let request = build_request();

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
                tracing::warn!(url, operation, retry = retries, error = %error, "request failed; retrying");
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
            tracing::error!(
                url,
                operation,
                status = status.as_u16(),
                "non-retryable status"
            );
            return Err(status_error(status, url, operation, &detail, retries));
        }

        if retries >= MAX_RETRIES {
            let detail = response_body_detail(response).await;
            tracing::error!(
                url,
                operation,
                status = status.as_u16(),
                retry = retries,
                "retryable status but retries exhausted"
            );
            return Err(status_error(status, url, operation, &detail, retries));
        }

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, SystemTime::now()));
        retries += 1;
        tracing::warn!(
            url,
            operation,
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

fn parse_retry_after(value: &str, now: SystemTime) -> Option<u64> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(retry_at.duration_since(now).unwrap_or_default().as_secs())
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

/// Suffix recognized as credential-carrying by [`is_sensitive_header_name`],
/// matched case-insensitively (e.g. `x-subscription-token`).
const SENSITIVE_HEADER_SUFFIX: &str = "-token";

/// `true` when `name` is a known credential-carrying header (see
/// [`SENSITIVE_HEADER_NAMES`]) or ends in [`SENSITIVE_HEADER_SUFFIX`] — the
/// generic rule that keeps backend subscription tokens (Brave's
/// `x-subscription-token`) out of request dumps. Both matches are
/// case-insensitive.
fn is_sensitive_header_name(name: &str) -> bool {
    SENSITIVE_HEADER_NAMES
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
        || name.to_ascii_lowercase().ends_with(SENSITIVE_HEADER_SUFFIX)
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
/// Reading stops after [`RESPONSE_BODY_SNIPPET_READ_BYTES`], so a huge or
/// endless error body is never buffered. Returns an empty string when the
/// body cannot be read or is blank.
pub(crate) async fn response_body_detail(response: reqwest::Response) -> String {
    use futures::StreamExt;

    let mut stream = response.bytes_stream();
    let mut body = BoundedBuffer::new(RESPONSE_BODY_SNIPPET_READ_BYTES);
    while !body.overflowed() {
        match stream.next().await {
            Some(Ok(chunk)) => {
                body.push(&chunk);
            }
            Some(Err(_)) if body.is_empty() => return String::new(),
            Some(Err(_)) | None => break,
        }
    }
    body.trim_incomplete_utf8_suffix();
    snippet_from_body(body.as_slice())
}

/// Most body bytes read for an error snippet: room for
/// [`RESPONSE_BODY_SNIPPET_MAX_CHARS`] four-byte characters plus leading
/// whitespace that the snippet trims.
const RESPONSE_BODY_SNIPPET_READ_BYTES: usize = 8 * 1024;

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
    operation: &str,
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
    anyhow::anyhow!["{operation} failed with HTTP {status_number}{body} for {url}{retry_note}"]
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
mod tests;
