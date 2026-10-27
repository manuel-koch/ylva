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

//! The native [`WebFetchBackend`]: a plain local HTTP `GET` via `reqwest`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::StreamExt;

use super::{
    convert_body_to_result, detect_missing_content_type, network_target, truncate_text_output,
    HopPolicy, WebFetchBackend, WebFetchBody, WebFetchError, WebFetchResponse,
};
use crate::agent::tools::read::{supported_media_type, READ_MEDIA_MAX_BYTES};
use crate::bounded_buffer::BoundedBuffer;
use crate::http::OUTBOUND_USER_AGENT;
use crate::saturating_cast::saturating_usize;

const SNIFF_PREFIX_BYTES: usize = 32;

/// Maximum number of redirect hops followed for one fetch.
pub const MAX_REDIRECTS: usize = 10;

/// The native backend: a plain local HTTP `GET` via `reqwest` with the Ylva
/// user-agent. Redirects are followed by the backend itself under a
/// non-interactive [`HopPolicy`] (see [`WebFetchBackend::fetch`]): hop limit,
/// scheme allowlist, a resolved-address check on every hop (the connection is
/// pinned to the checked addresses) and explicit `deny` rules for hops to a
/// different target.
pub struct HttpFetchBackend {
    http_timeout: Duration,
    dump_source: crate::http::RequestDumpSource,
}

impl std::fmt::Debug for HttpFetchBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpFetchBackend")
            .field("http_timeout", &self.http_timeout)
            .field("dump_source", &self.dump_source)
            .finish_non_exhaustive()
    }
}

impl HttpFetchBackend {
    /// Build a backend. `http_timeout` bounds a single request (the tool's
    /// outer timeout must exceed it).
    pub fn new(http_timeout: Duration) -> Self {
        Self::with_dump_source(http_timeout, crate::http::RequestDumpSource::Environment)
    }

    /// Build a backend with an explicit dump-path template.
    ///
    /// This keeps tests on the actual request/dump path without mutating the
    /// process environment; production callers normally use [`Self::new`],
    /// which resolves `YLVA_REQUEST_DUMP_PATH` when each request starts.
    pub fn with_dump_path(http_timeout: Duration, dump_path: &Path) -> Self {
        Self::with_dump_source(
            http_timeout,
            crate::http::RequestDumpSource::from_template(dump_path),
        )
    }

    fn with_dump_source(
        http_timeout: Duration,
        dump_source: crate::http::RequestDumpSource,
    ) -> Self {
        HttpFetchBackend {
            http_timeout,
            dump_source,
        }
    }

    /// Resolve `url`'s host, require every private or reserved address to be
    /// permitted by `policy` and build a client pinned to exactly those
    /// addresses, so the connection cannot go elsewhere between check and
    /// connect (DNS rebinding). Behind a proxy the proxy resolves the
    /// destination and the pin does not apply.
    async fn pinned_client(
        &self,
        url: &reqwest::Url,
        policy: HopPolicy<'_>,
    ) -> Result<reqwest::Client, WebFetchError> {
        let host = url
            .host_str()
            .ok_or_else(|| WebFetchError::InvalidUrl {
                url: url.to_string(),
            })?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let mut builder = reqwest::Client::builder()
            .user_agent(OUTBOUND_USER_AGENT)
            .timeout(self.http_timeout)
            .redirect(reqwest::redirect::Policy::none());
        let literal_address = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .ok();
        let addresses: Vec<SocketAddr> = match literal_address {
            Some(address) => vec![SocketAddr::new(address, port)],
            None => {
                let lookup = tokio::time::timeout(
                    self.http_timeout,
                    tokio::net::lookup_host((host.as_str(), port)),
                )
                .await;
                match lookup {
                    Ok(Ok(resolved)) => resolved.collect(),
                    Ok(Err(error)) => {
                        return Err(WebFetchError::Request {
                            detail: format!("could not resolve '{host}': {error}"),
                        })
                    }
                    Err(_) => {
                        return Err(WebFetchError::Request {
                            detail: format!("resolving '{host}' timed out"),
                        })
                    }
                }
            }
        };
        if addresses.is_empty() {
            return Err(WebFetchError::Request {
                detail: format!("'{host}' resolves to no addresses"),
            });
        }
        if let Some(blocked) = addresses
            .iter()
            .find(|address| is_private_or_reserved(address.ip()))
        {
            let hop_target = network_target(url.as_str())?;
            if !(policy.allow_private_address)(&hop_target) {
                return Err(WebFetchError::Refused {
                    url: url.to_string(),
                    reason: format!(
                        "'{host}' is a private or reserved address ({}); allow it with a \
                         `network_private_address` rule in [tools.web_fetch]",
                        blocked.ip()
                    ),
                });
            }
        }
        if literal_address.is_none() {
            builder = builder.resolve_to_addrs(&host, &addresses);
        }
        builder.build().map_err(|error| WebFetchError::Request {
            detail: error.to_string(),
        })
    }

    /// Send the request for `url`, following redirects under the hop policy,
    /// and return the first non-redirect response.
    async fn send_following_redirects(
        &self,
        url: &str,
        policy: HopPolicy<'_>,
        dump_context: &crate::http::RequestDumpContext,
    ) -> Result<reqwest::Response, WebFetchError> {
        let requested_target = network_target(url)?;
        let mut current = reqwest::Url::parse(url).map_err(|_| WebFetchError::InvalidUrl {
            url: url.to_string(),
        })?;
        for hop in 0..=MAX_REDIRECTS {
            let client = self.pinned_client(&current, policy).await?;
            let request =
                client
                    .get(current.clone())
                    .build()
                    .map_err(|error| WebFetchError::Request {
                        detail: error.to_string(),
                    })?;
            if hop == 0 {
                if let Some(path) = dump_context.request_path() {
                    crate::http::dump_request(
                        request.url().as_str(),
                        &crate::http::redacted_headers_json(request.headers()),
                        &serde_json::Value::Null,
                        &path,
                    );
                }
            }
            // No retry: every failure is terminal (see the module docs).
            let response = client.execute(request).await.map_err(|error| {
                let response_dump_path = dump_context.response_path_for_content_type(None, "jsonl");
                crate::http::dump_response_transport_error(&response_dump_path, &error);
                WebFetchError::Request {
                    detail: error.to_string(),
                }
            })?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return Ok(response);
            };
            let refuse = |reason: String| WebFetchError::Refused {
                url: location.to_string(),
                reason,
            };
            if hop == MAX_REDIRECTS {
                return Err(refuse(format!(
                    "too many redirects (more than {MAX_REDIRECTS})"
                )));
            }
            let next = current
                .join(location)
                .map_err(|error| refuse(format!("invalid redirect location: {error}")))?;
            let next_target = network_target(next.as_str())
                .map_err(|_| refuse("redirects must stay on http or https URLs".to_string()))?;
            if next_target != requested_target {
                if let Some(reason) = (policy.deny)(&next_target) {
                    return Err(refuse(reason));
                }
            }
            current = next;
        }
        unreachable!("the final hop returns or refuses")
    }
}

/// Whether `address` is loopback, unspecified, link-local, private,
/// shared-address, multicast, broadcast, documentation, benchmarking or
/// otherwise reserved — anything a public web page should not point at.
fn is_private_or_reserved(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_private_or_reserved_v4(address),
        IpAddr::V6(address) => is_private_or_reserved_v6(address),
    }
}

fn is_private_or_reserved_v4(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_documentation()
        || first == 0
        || (first == 100 && (64..128).contains(&second))
        || (first == 198 && (18..20).contains(&second))
        || first >= 240
}

fn is_private_or_reserved_v6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_private_or_reserved_v4(mapped);
    }
    let segments = address.segments();
    // NAT64 (64:ff9b::/96) embeds an IPv4 address in the last 32 bits.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [high, low] = [segments[6], segments[7]];
        let embedded = Ipv4Addr::new(
            (high >> 8) as u8,
            (high & 0xff) as u8,
            (low >> 8) as u8,
            (low & 0xff) as u8,
        );
        return is_private_or_reserved_v4(embedded);
    }
    address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

impl WebFetchBackend for HttpFetchBackend {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        policy: HopPolicy<'a>,
    ) -> BoxFuture<'a, Result<WebFetchResponse, WebFetchError>> {
        Box::pin(async move {
            let dump_context = crate::http::RequestDumpContext::new(&self.dump_source);
            let response = self
                .send_following_redirects(url, policy, &dump_context)
                .await?;
            let status = response.status();
            let final_url = response.url().to_string();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let content_length = response.content_length();
            let response_dump_path =
                dump_context.response_path_for_content_type(content_type.as_deref(), "bin");
            if !status.is_success() {
                return Err(WebFetchError::Status {
                    url: final_url,
                    status: status.as_u16(),
                });
            }

            let limit = READ_MEDIA_MAX_BYTES;
            let content_length_over_limit =
                content_length.is_some_and(|length| length > limit as u64);
            let mut raw_body = BoundedBuffer::new(limit);
            let mut stream = response.bytes_stream();

            // Decide the body kind from a small prefix (magic numbers first)
            // so the size policy can differ by kind: a known-oversize media
            // body fails before it is downloaded, text is read up to `limit`.
            let mut sniffed_media = false;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    crate::http::dump_response_transport_error(&response_dump_path, &error);
                    WebFetchError::Request {
                        detail: error.to_string(),
                    }
                })?;
                raw_body.push(&chunk);
                if supported_media_type(raw_body.as_slice()).is_some() {
                    sniffed_media = true;
                    break;
                }
                if raw_body.len() >= SNIFF_PREFIX_BYTES || raw_body.overflowed() {
                    break;
                }
            }
            if sniffed_media && (content_length_over_limit || raw_body.overflowed()) {
                return Err(WebFetchError::TooLarge {
                    size: saturating_usize(content_length.unwrap_or_default()),
                    limit,
                });
            }

            let mut truncated_raw = content_length_over_limit;
            if sniffed_media {
                // Media must arrive complete: a partial image or PDF is
                // unusable (mirrors `read`'s whole-file media policy).
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|error| {
                        crate::http::dump_response_transport_error(&response_dump_path, &error);
                        WebFetchError::Request {
                            detail: error.to_string(),
                        }
                    })?;
                    let received = raw_body.len() + chunk.len();
                    if !raw_body.push(&chunk) {
                        return Err(WebFetchError::TooLarge {
                            size: saturating_usize(content_length.unwrap_or(received as u64)),
                            limit,
                        });
                    }
                }
            } else {
                // Text is read to at most `limit` bytes; over the limit the
                // partial body is classified and converted, flagged truncated.
                while !raw_body.overflowed() {
                    match stream.next().await {
                        None => break,
                        Some(Err(error)) => {
                            crate::http::dump_response_transport_error(&response_dump_path, &error);
                            return Err(WebFetchError::Request {
                                detail: error.to_string(),
                            });
                        }
                        Some(Ok(chunk)) => {
                            raw_body.push(&chunk);
                        }
                    }
                }
                truncated_raw |= raw_body.overflowed();
                if truncated_raw {
                    // Cut on a UTF-8 character boundary so the partial text
                    // body stays convertible.
                    raw_body.trim_incomplete_utf8_suffix();
                }
            }
            let raw_body = raw_body.into_bytes();
            crate::http::dump_buffered_response(&raw_body, response_dump_path.as_deref());

            let body = convert_body_to_result(content_type.as_deref(), &raw_body)?;
            let detected_content_type =
                detect_missing_content_type(content_type.as_deref(), &raw_body);
            let (body, truncated) = match body {
                WebFetchBody::Media { .. } if truncated_raw => {
                    return Err(WebFetchError::TooLarge {
                        size: saturating_usize(content_length.unwrap_or(raw_body.len() as u64)),
                        limit,
                    });
                }
                media @ WebFetchBody::Media { .. } => (media, false),
                WebFetchBody::Text { text } => {
                    let (text, output_truncated) = truncate_text_output(&text);
                    (
                        WebFetchBody::Text { text },
                        truncated_raw || output_truncated,
                    )
                }
            };
            Ok(WebFetchResponse {
                final_url,
                status: status.as_u16(),
                content_type,
                detected_content_type,
                body,
                truncated,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_blocked(text: &str) -> bool {
        is_private_or_reserved(text.parse().expect("valid address"))
    }

    #[test]
    fn private_loopback_and_reserved_addresses_are_blocked() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
        ] {
            assert!(is_blocked(address), "{address} must be blocked");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for address in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_blocked(address), "{address} must be allowed");
        }
    }
}
