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

//! Attribution of the model view's estimated tokens to their origins, for
//! the `/status` popup.

use super::{estimate_message_tokens, Context, Message};
use crate::context_breakdown::{ContextBreakdown, ContextCategory, ContextSegment};
use crate::llm::ToolSchema;
use crate::system_prompt_origins::{PromptOrigin, SystemPromptOrigins};

/// Segments in display order; repeated categories accumulate into one segment.
#[derive(Default)]
struct SegmentAccumulator {
    segments: Vec<ContextSegment>,
}

impl SegmentAccumulator {
    fn add(&mut self, category: ContextCategory, tokens: usize) {
        if let Some(segment) = self
            .segments
            .iter_mut()
            .find(|segment| segment.category == category)
        {
            segment.tokens += tokens;
        } else {
            self.segments.push(ContextSegment { category, tokens });
        }
    }
}

fn category_for_origin(origin: &PromptOrigin) -> ContextCategory {
    match origin {
        PromptOrigin::Soul { source } => ContextCategory::Soul {
            source: source.clone(),
        },
        PromptOrigin::InstructionSafetyPreamble => ContextCategory::InstructionSafetyPreamble,
        PromptOrigin::InstructionFile { source } => ContextCategory::InstructionFile {
            source: source.clone(),
        },
        PromptOrigin::AgentPersona | PromptOrigin::SkillAdvertisement => {
            ContextCategory::AgentPrompt
        }
        PromptOrigin::Unattributed => ContextCategory::UnattributedSystemPrompt,
    }
}

impl Context {
    /// Estimated model-view tokens per origin, against `context_length`.
    ///
    /// `origins` are the sections of the identity prompt (message 0); without
    /// them that message counts as one unattributed block. Counts are the same
    /// advisory estimates compaction budgeting uses.
    pub fn context_breakdown(
        &self,
        tools: &[ToolSchema],
        origins: Option<&SystemPromptOrigins>,
        context_length: usize,
    ) -> ContextBreakdown {
        let mut accumulator = SegmentAccumulator::default();
        let leading_system_len = self.leading_system_len();
        let identity_tokens = self.token_count(0).unwrap_or(0);
        match origins.filter(|origins| !origins.is_empty()) {
            Some(origins) => {
                let section_tokens: Vec<usize> = origins
                    .sections()
                    .iter()
                    .map(|section| self.estimator.estimate_tokens(&section.text))
                    .collect();
                // Separators and message framing belong to the first section.
                let framing = identity_tokens.saturating_sub(section_tokens.iter().sum());
                for (index, (section, tokens)) in
                    origins.sections().iter().zip(section_tokens).enumerate()
                {
                    let tokens = if index == 0 { tokens + framing } else { tokens };
                    accumulator.add(category_for_origin(&section.origin), tokens);
                }
            }
            None if leading_system_len > 0 => {
                accumulator.add(ContextCategory::UnattributedSystemPrompt, identity_tokens);
            }
            None => {}
        }
        let mut tool_tokens = self.tool_schema_token_count(tools);
        for index in 1..leading_system_len {
            let tokens = self.token_count(index).unwrap_or(0);
            if index == 1 {
                tool_tokens += tokens;
            } else {
                accumulator.add(ContextCategory::UnattributedSystemPrompt, tokens);
            }
        }
        accumulator.add(ContextCategory::ToolSchemas, tool_tokens);

        let compaction_messages = usize::from(self.compaction.is_some())
            + self
                .recovery
                .as_ref()
                .map_or(0, |recovery| 1 + usize::from(recovery.anchor.is_some()));
        let model_messages = self.model_messages();
        for (offset, message) in model_messages.iter().enumerate().skip(leading_system_len) {
            let tokens = estimate_message_tokens(self.estimator, message);
            if offset - leading_system_len < compaction_messages {
                accumulator.add(ContextCategory::CompactionSummary, tokens);
                continue;
            }
            match message {
                Message::System { .. } => {
                    accumulator.add(ContextCategory::UnattributedSystemPrompt, tokens)
                }
                Message::User { .. } => accumulator.add(ContextCategory::UserMessages, tokens),
                Message::Assistant {
                    content,
                    status,
                    tool_calls,
                } => {
                    let text_only = Message::Assistant {
                        content: content.clone(),
                        status: status.clone(),
                        tool_calls: Vec::new(),
                    };
                    let text_tokens =
                        estimate_message_tokens(self.estimator, &text_only).min(tokens);
                    accumulator.add(ContextCategory::AssistantMessages, text_tokens);
                    if !tool_calls.is_empty() {
                        accumulator.add(ContextCategory::ToolCalls, tokens - text_tokens);
                    }
                }
                Message::ToolResult { .. } | Message::ToolError { .. } => {
                    accumulator.add(ContextCategory::ToolCalls, tokens)
                }
            }
        }
        ContextBreakdown::new(accumulator.segments, context_length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::UserMessageKind;
    use crate::context_breakdown::ContextGroup;
    use crate::llm::TokenEstimator;

    fn context_with_origins() -> (Context, SystemPromptOrigins) {
        let mut origins = SystemPromptOrigins::default();
        origins.push(
            PromptOrigin::Soul {
                source: "built-in".into(),
            },
            "a".repeat(400),
        );
        origins.push(PromptOrigin::AgentPersona, "b".repeat(200));
        let mut context = Context::with_system_prompt(origins.render());
        context.set_token_estimator(TokenEstimator::FourCharactersPerToken);
        (context, origins)
    }

    #[test]
    fn prompt_tokens_are_attributed_to_their_origins() {
        let (context, origins) = context_with_origins();

        let breakdown = context.context_breakdown(&[], Some(&origins), 10_000);

        let tokens_of = |category: ContextCategory| {
            breakdown
                .segments()
                .iter()
                .find(|segment| segment.category == category)
                .map(|segment| segment.tokens)
        };
        assert_eq!(tokens_of(ContextCategory::AgentPrompt), Some(50));
        assert!(
            tokens_of(ContextCategory::Soul {
                source: "built-in".into()
            })
            .unwrap()
                >= 100
        );
        assert!(tokens_of(ContextCategory::ToolSchemas).unwrap() > 0);
    }

    #[test]
    fn without_origins_the_identity_prompt_is_one_unattributed_block() {
        let (context, _) = context_with_origins();

        let breakdown = context.context_breakdown(&[], None, 10_000);

        assert_eq!(
            breakdown.segments()[0].category,
            ContextCategory::UnattributedSystemPrompt
        );
        assert_eq!(
            breakdown.segments()[0].tokens,
            context.token_count(0).unwrap()
        );
    }

    #[test]
    fn conversation_messages_are_grouped_by_kind_and_sum_to_the_model_view() {
        let (mut context, origins) = context_with_origins();
        context.push_entry(Message::User {
            content: "x".repeat(80),
            kind: UserMessageKind::Explicit,
        });
        context.push_entry(Message::Assistant {
            content: "y".repeat(40),
            status: crate::agent::AssistantStatus::Complete,
            tool_calls: Vec::new(),
        });

        let breakdown = context.context_breakdown(&[], Some(&origins), 10_000);

        assert_eq!(breakdown.used_tokens(), context.model_view_token_count());
        assert!(breakdown.group_tokens(ContextGroup::UserMessages) >= 20);
        assert!(breakdown.group_tokens(ContextGroup::AssistantMessages) >= 10);
        assert_eq!(breakdown.group_tokens(ContextGroup::ToolCalls), 0);
    }
}
