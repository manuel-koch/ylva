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

use super::ContextUsageSource;
use crate::llm::token_estimate::format_token_estimate;

/// Estimated context size against the active context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    /// Estimated token count.
    pub estimated_tokens: usize,
    /// The active window the estimate is measured against.
    pub context_length: usize,
    /// The representation this estimate covers.
    pub source: ContextUsageSource,
}

/// Render the token count and fill percentage for a context-usage estimate.
///
/// The percentage is the estimate's fill of the active window, rounded to a
/// whole percent and clamped to `100%` (an estimate can overshoot briefly
/// before compaction runs).
pub fn format_context_usage(estimated_tokens: usize, context_length: usize) -> String {
    let percentage = if context_length == 0 {
        0.0
    } else {
        (estimated_tokens as f64 / context_length as f64) * 100.0
    }
    .round()
    .clamp(0.0, 100.0) as u64;
    format!(
        "{} ({percentage}%)",
        format_token_estimate(estimated_tokens)
    )
}

/// Format the footer value independently of the estimate source.
pub fn format_context_usage_readout(context_usage: ContextUsage) -> String {
    format_context_usage(context_usage.estimated_tokens, context_usage.context_length)
}
