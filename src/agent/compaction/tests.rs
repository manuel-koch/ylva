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
use crate::agent::{AssistantStatus, ToolCallRequest, ToolResult};
use crate::llm::ToolSchema;
use serde_json::Value;
use std::sync::Mutex;
use tokio::sync::mpsc;

fn user(text: &str) -> Message {
    Message::User {
        content: text.to_string(),
        kind: UserMessageKind::Explicit,
    }
}

fn assistant_with_call(id: Uuid) -> Message {
    Message::Assistant {
        content: String::new(),
        status: AssistantStatus::Complete,
        tool_calls: vec![ToolCallRequest {
            id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    }
}

fn tool_result(id: Uuid) -> Message {
    Message::ToolResult {
        value: ToolResult {
            id,
            name: "read".to_string(),
            result: "file contents".to_string(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        },
    }
}

/// What [`new_recording_runtime`] builds: the compaction state, its outcome
/// channel, the activity-update receiver, the `run_id` the activity
/// sender was scoped to, a clone of the compact engine (as `sweep` takes
/// its own `engine` argument, separate from the coordinator's internal
/// compact engine), and a context already primed with one user message
/// to summarize.
type RecordingRuntimeFixture = (
    CompactionRuntime,
    mpsc::Receiver<CompactionOutcome>,
    mpsc::Receiver<super::super::UiUpdate>,
    Option<Uuid>,
    Arc<dyn Engine>,
    Context,
);

/// Builds a `CompactionRuntime` wired to a `RecordingSummaryProvider`
/// that always replies with `reply`. See [`RecordingRuntimeFixture`] for
/// what's returned.
fn new_recording_runtime(reply: &str) -> RecordingRuntimeFixture {
    let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
        Arc::new(RecordingSummaryProvider {
            recorded: Arc::new(Mutex::new(Vec::new())),
            reply: reply.to_string(),
        }),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "compact-model".to_string(),
            temperature: None,
            context_length: Some(16_000),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        },
    ));
    let (mut runtime, outcomes) = CompactionRuntime::new(
        Some(engine.clone()),
        COMPACTION_SUMMARY_PROMPT.to_string(),
        Duration::from_secs(1),
    );
    let run_id = Some(Uuid::new_v4());
    let (sender, updates) = mpsc::channel(4);
    runtime.set_activity_sender(sender, run_id);
    let mut context = Context::new();
    context.push_user("Summarize this.".to_string());
    (runtime, outcomes, updates, run_id, engine, context)
}

fn capacity_test_engine(context_length: Option<usize>) -> Arc<dyn Engine> {
    Arc::new(crate::llm::EngineImpl::new(
        Arc::new(RecordingSummaryProvider {
            recorded: Arc::new(Mutex::new(Vec::new())),
            reply: "A summary.".to_string(),
        }),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "capacity-model".to_string(),
            temperature: None,
            context_length,
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        },
    ))
}

/// One request as the recording provider saw it.
#[derive(Clone)]
struct RecordedRequest {
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
}

/// Records every request it receives and answers with a canned summary.
struct RecordingSummaryProvider {
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
    reply: String,
}

impl crate::llm::Provider for RecordingSummaryProvider {
    fn name(&self) -> &str {
        "recording-summary"
    }

    fn list_models(
        &self,
    ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.recorded
            .lock()
            .expect("recording mutex is not poisoned")
            .push(RecordedRequest { messages, tools });
        let reply = self.reply.clone();
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(StreamEvent::Text(reply)).await;
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }
}

mod boundaries;
mod budget;
mod manual;
mod runtime;
