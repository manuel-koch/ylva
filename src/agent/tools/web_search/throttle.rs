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

//! Request pacing: minimum interval between searches with random jitter.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use super::{WEB_SEARCH_THROTTLE_INTERVAL, WEB_SEARCH_THROTTLE_JITTER_FRACTION};

/// Delay before the next request may start, computed from how long ago the
/// previous request started: no request yet, or the interval already
/// elapsed → no delay; otherwise the remainder of the interval plus `jitter`.
pub(super) fn throttle_delay(
    elapsed_since_last_start: Option<Duration>,
    interval: Duration,
    jitter: Duration,
) -> Duration {
    let Some(elapsed) = elapsed_since_last_start else {
        return Duration::ZERO;
    };
    if elapsed >= interval {
        Duration::ZERO
    } else {
        interval - elapsed + jitter
    }
}

/// The random component of a throttled wait: zero up to
/// [`WEB_SEARCH_THROTTLE_JITTER_FRACTION`] of `interval`.
pub(super) fn throttle_jitter(interval: Duration) -> Duration {
    crate::http::jitter_duration(interval, WEB_SEARCH_THROTTLE_JITTER_FRACTION)
}

/// Gate one request start: sleep out the remainder of
/// [`WEB_SEARCH_THROTTLE_INTERVAL`] plus jitter when the previous request
/// started too recently, then record *this* request's start time.
///
/// The timestamp is recorded after the wait — i.e. at request start —
/// regardless of how the request turns out, so a failed search throttles the
/// next one exactly like a successful one. The sleep runs inside the tool's
/// `run()` future, so dropping that future (outer timeout or
/// `CancellationToken`) aborts it without extra cancellation code.
pub(super) async fn wait_before_request(last_request_started: &Mutex<Option<Instant>>) {
    let delay = {
        let guard = last_request_started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        throttle_delay(
            guard.as_ref().map(|started| started.elapsed()),
            WEB_SEARCH_THROTTLE_INTERVAL,
            throttle_jitter(WEB_SEARCH_THROTTLE_INTERVAL),
        )
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let mut guard = last_request_started
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(Instant::now());
}
