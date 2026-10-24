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
/// A queued tool call awaiting approval, with its tool-defined timeout.
///
/// Once approved, [`PendingTool::execute`] runs the tool in its own task and
/// reports completion through `agent_tx` so the loop never blocks on a tool.
pub struct PendingTool {
    pub call: Box<dyn ToolCall>,
}
impl PendingTool {
    /// Spawn the tool in its own task, bounded by `timeout_secs` (or the
    /// default) and a per-tool `CancellationToken`. Completion is reported via
    /// `agent_tx` (`ToolDone`/`ToolError`/`ToolKilled`); returns the token the
    /// outer loop stores as the running tool's cancel.
    pub fn execute(
        self,
        agent_tx: mpsc::Sender<(u64, AgentEvent)>,
        generation: u64,
        _max_output_tokens: Option<usize>,
    ) -> CancellationToken {
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let id = self.call.id();
        let name = self.call.name().to_string();
        let timeout_secs = self.call.timeout_secs();
        // Captured before `run()` consumes the call so failure events can
        // lead with the same body sections the pending/running bubble showed
        // (e.g. bash's `Label`/`Command`), keeping replay identical to live.
        let pending_content_sections = self.call.pending_content_sections();
        let span = tracing::info_span!("tool_execute", tool = %name, tool_call_id = %id);
        tokio::spawn(
            async move {
                let outcome = tokio::select! {
                    biased;
                    _ = run_cancel.cancelled() => {
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolKilled {
                                    id,
                                    name: name.clone(),
                                },
                            ))
                            .await;
                        return;
                    }
                    result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
                        let start = std::time::Instant::now();
                        let result = self.call.run().await;
                        let elapsed = start.elapsed();
                        (result, elapsed)
                    }) => result,
                };
                match outcome {
                    Ok((Ok(tool_output), duration)) => {
                        let result_metadata = tool_output
                            .metadata()
                            .filter(|m| !m.as_object().map(|o| o.is_empty()).unwrap_or(true));
                        let result_metadata_schema = result_metadata.as_ref().and_then(|_| {
                            tool_output
                                .metadata_schema()
                                .and_then(|schema| serde_json::to_value(schema).ok())
                        });
                        let result = tool_output.text();
                        let display_result = tool_output.display_text();
                        let summary = tool_output.display_summary();
                        let content_sections = tool_output.content_sections();
                        let attachments = tool_output.attachments();
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolDone {
                                    id,
                                    name,
                                    result,
                                    requires_complete_delivery: tool_output
                                        .requires_complete_delivery(),
                                    display_result,
                                    result_metadata,
                                    result_metadata_schema,
                                    summary,
                                    duration,
                                    content_sections,
                                    attachments,
                                },
                            ))
                            .await;
                    }
                    Ok((Err(error), duration)) => {
                        let partial_output = error
                            .partial_output()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        let mut content_sections = pending_content_sections;
                        if let Some(output) = &partial_output {
                            content_sections.push(crate::agent::tools::Section::Content(
                                crate::agent::tools::ContentSection {
                                    label: Some("Partial result".to_string()),
                                    content: output.clone(),
                                    file_extension: None,
                                    line_number_offset: Some(0),
                                },
                            ));
                        }
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolError {
                                    id,
                                    name,
                                    error: error.to_string(),
                                    partial_output,
                                    duration,
                                    content_sections,
                                },
                            ))
                            .await;
                    }
                    Err(_elapsed) => {
                        let message = format!("Tool '{name}' timed out after {timeout_secs}s");
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::ToolError {
                                    id,
                                    name,
                                    error: message,
                                    partial_output: None,
                                    duration: Duration::from_secs(timeout_secs),
                                    content_sections: pending_content_sections,
                                },
                            ))
                            .await;
                    }
                }
            }
            .instrument(span),
        );
        cancel
    }
}
impl std::fmt::Debug for PendingTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingTool")
            .field("name", &self.call.name())
            .field("id", &self.call.id())
            .finish()
    }
}
