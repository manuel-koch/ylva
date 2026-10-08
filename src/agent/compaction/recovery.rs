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

//! Lossy, continuity-preserving recovery of the model view.
//!
//! When a summary is unavailable or too late, the hard-limit path drops old
//! complete tool groups from the model view. Dropping alone can leave a
//! system-only request, so every drop carries a [`RecoveryProjection`]: a
//! deterministic handoff built only from real transcript content (never
//! fabricated conclusions) plus the newest explicit user request as the task
//! anchor. The transcript itself is never rewritten.

use crate::agent::{Message, UserMessageKind};
use crate::llm::token_estimate::truncate_text_to_token_budget;
use crate::llm::TokenEstimator;

/// Marker prefixing the recovery handoff in the model view. Like
/// [`super::COMPACTION_SUMMARY_MARKER`] it is projected as a `user` message,
/// never a mid-conversation `system` message.
pub const RECOVERY_HANDOFF_MARKER: &str = "[context recovery handoff — not from the user]";

/// Most recent removed tool outcomes listed in a handoff.
const MAX_LISTED_OUTCOMES: usize = 24;

/// Characters of each removed tool outcome kept as an excerpt.
const OUTCOME_EXCERPT_CHARS: usize = 120;

/// Smallest budget a handoff or anchor is shortened to.
#[cfg(test)]
const MINIMUM_PROJECTION_TOKENS: usize = 64;

/// The model-view replacement for content a lossy recovery removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryProjection {
    /// First transcript index kept verbatim after this recovery. A later
    /// summary covering at least this boundary supersedes the projection.
    pub boundary: usize,
    /// Marked `user` message listing what was removed.
    pub handoff: Message,
    /// The newest explicit user request, verbatim unless shortened; `None`
    /// while that request is still part of the verbatim tail.
    pub anchor: Option<Message>,
    /// Estimated tokens of `handoff` plus `anchor`.
    pub token_count: usize,
}

/// What one [`Context::recover_model_view`](crate::agent::Context) call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoveryReport {
    /// Whether the model view changed.
    pub changed: bool,
    /// Estimated payload tokens of the model view afterwards (tool schemas
    /// excluded).
    pub estimated_tokens_after: usize,
    /// The view is at or below the compaction target.
    pub fits_target: bool,
    /// The view is at or below the safe request ceiling.
    pub fits_ceiling: bool,
    pub stage: u8,
    pub clipped_results: usize,
}

pub(crate) const RESULT_OMISSION_MARKER: &str =
    "\n[tool output omitted from the model view; full output remains in the transcript]";
pub(crate) const REQUEST_OMISSION_MARKER: &str =
    "\n[request shortened in the model view; full request remains in the transcript]";
pub(crate) const SUMMARY_OMISSION_MARKER: &str = "\n[older context shortened in the model view]";

/// Markers are part of the allowance, not text appended after truncation.
pub(crate) fn marked_text(
    estimator: TokenEstimator,
    text: &str,
    marker: &str,
    allowance: usize,
) -> String {
    if estimator.estimate_tokens(text) <= allowance {
        return text.to_string();
    }
    let reserved = estimator.estimate_tokens(marker);
    let (mut prefix, _) =
        truncate_text_to_token_budget(estimator, text, allowance.saturating_sub(reserved));
    while !prefix.is_empty() && estimator.estimate_tokens(&format!("{prefix}{marker}")) > allowance
    {
        prefix.pop();
    }
    format!("{prefix}{marker}")
}

pub(crate) fn project_body(
    estimator: TokenEstimator,
    message: &Message,
    allowance: usize,
) -> Option<Message> {
    if crate::agent::estimate_message_tokens(estimator, message) <= allowance {
        return None;
    }
    if let Message::User {
        content,
        kind: UserMessageKind::ImplicitToolNote,
    } = message
    {
        if content.contains("Subagent completion event;") {
            if let Some(start) = content.find("\n{") {
                let json_start = start + 1;
                if let Some(end) = content[json_start..].find('\n') {
                    let json_end = json_start + end;
                    if let Ok(mut event) =
                        serde_json::from_str::<serde_json::Value>(&content[json_start..json_end])
                    {
                        let original_result = event["result"].as_str().map(str::to_string);
                        let original_error = event["error"].as_str().map(str::to_string);
                        event["result_reduced"] = serde_json::Value::Bool(true);
                        let render = |event: &serde_json::Value| Message::User {
                            content: format!(
                                "{}{}{}",
                                &content[..json_start],
                                event,
                                &content[json_end..]
                            ),
                            kind: UserMessageKind::ImplicitToolNote,
                        };
                        for field in ["result", "error"] {
                            if event[field].is_string() {
                                event[field] = serde_json::Value::String(String::new());
                            }
                        }
                        let framing =
                            crate::agent::estimate_message_tokens(estimator, &render(&event));
                        let mut body_allowance = allowance.saturating_sub(framing) / 2;
                        loop {
                            for (field, original) in
                                [("result", &original_result), ("error", &original_error)]
                            {
                                if let Some(original) = original {
                                    event[field] = serde_json::Value::String(marked_text(
                                        estimator,
                                        original,
                                        RESULT_OMISSION_MARKER,
                                        body_allowance,
                                    ));
                                }
                            }
                            let projected = render(&event);
                            let estimate =
                                crate::agent::estimate_message_tokens(estimator, &projected);
                            if estimate <= allowance || body_allowance == 0 {
                                return Some(projected);
                            }
                            body_allowance = body_allowance
                                .saturating_sub(estimate.saturating_sub(allowance).max(1));
                        }
                    }
                }
            }
        }
    }
    let mut projected = message.clone();
    if let Message::ToolResult { value } = &mut projected {
        value.content_sections.clear();
        value.summary.clear();
        value.result_section_id = None;
    }
    if let Message::ToolError {
        content_sections, ..
    } = &mut projected
    {
        content_sections.clear();
    }
    let body = match &mut projected {
        Message::ToolResult { value } => {
            if !value.attachments.is_empty() {
                value.attachments.clear();
                value.result = "[binary attachment omitted from the model view; complete data remains in the transcript]".to_string();
                return Some(projected);
            }
            &mut value.result
        }
        Message::ToolError { error, .. } => error,
        Message::User {
            content,
            kind: UserMessageKind::ImplicitToolNote,
        } => content,
        _ => return None,
    };
    let original = body.clone();
    body.clear();
    let framing = crate::agent::estimate_message_tokens(estimator, &projected);
    let body = match &mut projected {
        Message::ToolResult { value } => &mut value.result,
        Message::ToolError { error, .. } => error,
        Message::User { content, .. } => content,
        _ => unreachable!(),
    };
    *body = marked_text(
        estimator,
        &original,
        RESULT_OMISSION_MARKER,
        allowance.saturating_sub(framing),
    );
    // JSON escaping and BPE joins can make body-plus-envelope estimates differ.
    while crate::agent::estimate_message_tokens(estimator, &projected) > allowance {
        let excess =
            crate::agent::estimate_message_tokens(estimator, &projected).saturating_sub(allowance);
        let body = match &mut projected {
            Message::ToolResult { value } => &mut value.result,
            Message::ToolError { error, .. } => error,
            Message::User { content, .. } => content,
            _ => unreachable!(),
        };
        let Some(prefix) = body.strip_suffix(RESULT_OMISSION_MARKER) else {
            break;
        };
        if prefix.is_empty() {
            break;
        }
        let next_allowance = estimator.estimate_tokens(prefix).saturating_sub(excess + 1);
        let prefix = truncate_text_to_token_budget(estimator, prefix, next_allowance).0;
        *body = format!("{prefix}{RESULT_OMISSION_MARKER}");
    }
    (crate::agent::estimate_message_tokens(estimator, &projected)
        < crate::agent::estimate_message_tokens(estimator, message))
    .then_some(projected)
}

/// Budgets that bound one projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProjectionBudget {
    pub handoff_tokens: usize,
    pub anchor_tokens: usize,
}

#[cfg(test)]
impl ProjectionBudget {
    /// Derive the budgets from the compaction target: the handoff may use an
    /// eighth of it, the anchored request up to half.
    pub fn for_target(target_tokens: usize) -> Self {
        Self {
            handoff_tokens: (target_tokens / 8).max(MINIMUM_PROJECTION_TOKENS),
            anchor_tokens: (target_tokens / 2).max(MINIMUM_PROJECTION_TOKENS),
        }
    }
}

fn excerpt(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = collapsed.chars();
    let head: String = characters.by_ref().take(OUTCOME_EXCERPT_CHARS).collect();
    if characters.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn strip_marker(handoff: &Message) -> &str {
    match handoff {
        Message::User { content, .. } => content
            .strip_prefix(RECOVERY_HANDOFF_MARKER)
            .unwrap_or(content)
            .trim(),
        _ => "",
    }
}

/// Build the projection that stands in for `messages[from..cut]`.
///
/// `prior` is the projection of an earlier recovery whose handoff is folded
/// in, clipped to the budget. The anchor is the newest explicit user message
/// of the whole transcript, projected only when it lies before `cut`.
pub(crate) fn build_projection(
    estimator: TokenEstimator,
    messages: &[Message],
    from: usize,
    cut: usize,
    prior: Option<&RecoveryProjection>,
    budget: ProjectionBudget,
) -> RecoveryProjection {
    let cut = cut.min(messages.len());
    let removed = &messages[from.min(cut)..cut];
    let anchor = messages
        .iter()
        .rposition(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        })
        .filter(|index| *index < cut)
        .and_then(|index| match &messages[index] {
            Message::User { content, .. } => Some(content),
            _ => None,
        })
        .map(|content| Message::User {
            content: marked_text(
                estimator,
                content,
                REQUEST_OMISSION_MARKER,
                budget.anchor_tokens,
            ),
            kind: UserMessageKind::Explicit,
        });

    let outcomes: Vec<String> = removed
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { value } => Some(format!(
                "- {} ok (tool_call_id {}): {}",
                value.name,
                value.id,
                excerpt(&value.result)
            )),
            Message::ToolError {
                id, name, error, ..
            } => Some(format!(
                "- {name} error (tool_call_id {id}): {}",
                excerpt(error)
            )),
            _ => None,
        })
        .collect();
    let omitted_outcomes = outcomes.len().saturating_sub(MAX_LISTED_OUTCOMES);

    let mut text = format!(
        "{RECOVERY_HANDOFF_MARKER}\n\nOlder conversation content was removed from your \
         view to fit the context window. This is lossy: details may be missing, and \
         anything below is only what the harness recorded, not a conclusion.\n"
    );
    if anchor.is_some() {
        text.push_str("\nThe latest explicit user request follows this handoff.\n");
    }
    if !outcomes.is_empty() {
        text.push_str("\nTool outcomes removed from your view");
        if omitted_outcomes > 0 {
            text.push_str(&format!(
                " ({omitted_outcomes} earlier outcomes omitted; most recent shown)"
            ));
        }
        text.push_str(":\n");
        text.push_str(&outcomes[omitted_outcomes..].join("\n"));
        text.push('\n');
    }
    if let Some(prior) = prior {
        let earlier = strip_marker(&prior.handoff);
        if !earlier.is_empty() {
            text.push_str("\nEarlier recovery handoff (clipped):\n");
            text.push_str(earlier);
            text.push('\n');
        }
    }
    let handoff = Message::User {
        content: marked_text(
            estimator,
            &text,
            SUMMARY_OMISSION_MARKER,
            budget.handoff_tokens,
        ),
        kind: UserMessageKind::ImplicitToolNote,
    };
    let token_count = crate::agent::estimate_message_tokens(estimator, &handoff)
        + anchor.as_ref().map_or(0, |message| {
            crate::agent::estimate_message_tokens(estimator, message)
        });
    RecoveryProjection {
        boundary: cut,
        handoff,
        anchor,
        token_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AssistantStatus, ToolCallRequest, ToolResult};
    use serde_json::Value;
    use std::time::Duration;
    use uuid::Uuid;

    fn explicit_user(text: &str) -> Message {
        Message::User {
            content: text.to_string(),
            kind: UserMessageKind::Explicit,
        }
    }

    fn call_and_result(name: &str, result: &str) -> [Message; 2] {
        let id = Uuid::new_v4();
        [
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id,
                    name: name.to_string(),
                    arguments: Value::Null,
                    persisted_title_arguments: Vec::new(),
                }],
            },
            Message::ToolResult {
                value: ToolResult {
                    id,
                    name: name.to_string(),
                    result: result.to_string(),
                    attachments: vec![],
                    result_section_id: None,
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections: vec![],
                    duration: Duration::ZERO,
                },
            },
        ]
    }

    fn handoff_text(projection: &RecoveryProjection) -> &str {
        match &projection.handoff {
            Message::User { content, .. } => content,
            other => panic!("handoff is a user message, got {other:?}"),
        }
    }

    #[test]
    fn attachment_projection_omits_payload_and_descriptors_without_mutating_metadata() {
        let [_, mut message] = call_and_result("web_fetch", &"QUJD".repeat(10_000));
        let Message::ToolResult { value } = &mut message else {
            unreachable!()
        };
        value.attachments.push(crate::agent::ToolAttachment::Image {
            media_type: "image/png".to_string(),
        });
        value.result_metadata = Some(serde_json::json!({ "status": 200, "source": "fixture" }));
        value.result_metadata_schema = Some(serde_json::json!({ "type": "object" }));
        let canonical = message.clone();
        let projected = project_body(TokenEstimator::Tiktoken, &message, 256).expect("projected");
        let Message::ToolResult { value } = &projected else {
            unreachable!()
        };
        assert!(value.attachments.is_empty());
        assert!(value.result.contains("binary attachment omitted"));
        assert_eq!(
            value.result_metadata,
            Some(serde_json::json!({ "status": 200, "source": "fixture" }))
        );
        assert_eq!(
            value.result_metadata_schema,
            Some(serde_json::json!({ "type": "object" }))
        );
        assert!(crate::agent::estimate_message_tokens(TokenEstimator::Tiktoken, &projected) <= 256);
        assert_eq!(message, canonical);
    }

    #[test]
    fn projection_keeps_the_request_verbatim_and_lists_real_outcomes() {
        let mut messages = vec![explicit_user("summarize the three pages")];
        messages.extend(call_and_result("web_fetch", "page one body"));
        let projection = build_projection(
            TokenEstimator::FourCharactersPerToken,
            &messages,
            0,
            messages.len(),
            None,
            ProjectionBudget::for_target(4_000),
        );
        assert_eq!(
            projection.anchor,
            Some(explicit_user("summarize the three pages"))
        );
        let content = handoff_text(&projection);
        assert!(content.starts_with(RECOVERY_HANDOFF_MARKER));
        assert!(content.contains("web_fetch ok"));
        assert!(content.contains("page one body"));
        assert!(content.contains("lossy"));
    }

    #[test]
    fn projection_shortens_oversized_request_and_handoff_with_markers() {
        let mut messages = vec![explicit_user(&"request ".repeat(5_000))];
        for _ in 0..40 {
            messages.extend(call_and_result("bash", &"output ".repeat(200)));
        }
        let budget = ProjectionBudget::for_target(800);
        let projection = build_projection(
            TokenEstimator::FourCharactersPerToken,
            &messages,
            0,
            messages.len(),
            None,
            budget,
        );
        let Some(Message::User { content, .. }) = &projection.anchor else {
            panic!("anchor present");
        };
        assert!(content.contains("request shortened"));
        let content = handoff_text(&projection);
        assert!(
            content.contains("earlier outcomes omitted")
                || content.contains(SUMMARY_OMISSION_MARKER)
        );
        assert!(
            TokenEstimator::FourCharactersPerToken.estimate_tokens(content)
                <= budget.handoff_tokens + 32
        );
    }

    #[test]
    fn projection_folds_a_prior_handoff_without_growing_unbounded() {
        let mut messages = vec![explicit_user("task")];
        messages.extend(call_and_result("read", "first"));
        let budget = ProjectionBudget::for_target(2_000);
        let mut projection = build_projection(
            TokenEstimator::FourCharactersPerToken,
            &messages,
            0,
            messages.len(),
            None,
            budget,
        );
        for round in 0..6 {
            let from = messages.len();
            messages.extend(call_and_result(
                "read",
                &format!("round {round} {}", "x".repeat(400)),
            ));
            projection = build_projection(
                TokenEstimator::FourCharactersPerToken,
                &messages,
                from,
                messages.len(),
                Some(&projection),
                budget,
            );
        }
        let content = handoff_text(&projection);
        assert!(content.contains("round 5"));
        assert!(
            TokenEstimator::FourCharactersPerToken.estimate_tokens(content)
                <= budget.handoff_tokens + 32
        );
        assert_eq!(content.matches(RECOVERY_HANDOFF_MARKER).count(), 1);
    }

    #[test]
    fn no_anchor_when_the_request_is_still_in_the_verbatim_tail() {
        let mut messages = call_and_result("read", "body").to_vec();
        let cut = messages.len();
        messages.push(explicit_user("still visible"));
        let projection = build_projection(
            TokenEstimator::FourCharactersPerToken,
            &messages,
            0,
            cut,
            None,
            ProjectionBudget::for_target(1_000),
        );
        assert_eq!(projection.anchor, None);
    }
}
