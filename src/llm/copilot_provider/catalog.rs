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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use crate::llm::openai_compatible_provider::{model_list_entries, ModelCatalog};
use crate::llm::{order_reasoning_efforts, InputModality, ModelInfo};

/// Reads Copilot's `/models` shape and remembers which models its catalog
/// lists for `/responses` but not for `/chat/completions`.
#[derive(Debug, Default)]
pub(crate) struct CopilotModelCatalog {
    responses_only_model_ids: RwLock<HashSet<String>>,
    catalog_loaded: AtomicBool,
}

impl ModelCatalog for CopilotModelCatalog {
    fn parse(&self, body: &serde_json::Value) -> Vec<ModelInfo> {
        let responses_only_model_ids = model_list_entries(body)
            .filter(|entry| is_responses_only(entry))
            .filter_map(|entry| entry.get("id")?.as_str().map(str::to_owned))
            .collect();
        if let Ok(mut known) = self.responses_only_model_ids.write() {
            *known = responses_only_model_ids;
            self.catalog_loaded.store(true, Ordering::Release);
        }
        model_list_entries(body)
            .filter(|entry| serves_a_chat_endpoint(entry))
            .filter_map(|entry| {
                Some(ModelInfo {
                    reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                    id: entry.get("id")?.as_str()?.to_string(),
                    context_length: parse_context_length(entry),
                    reasoning_efforts: parse_reasoning_efforts(entry),
                    reasoning_default: None,
                    input_modalities: parse_input_modalities(entry),
                })
            })
            .collect()
    }

    fn is_loaded(&self) -> bool {
        self.catalog_loaded.load(Ordering::Acquire)
    }

    fn uses_responses_api(&self, model: &str) -> bool {
        self.responses_only_model_ids
            .read()
            .is_ok_and(|known| known.contains(model))
    }
}

fn is_responses_only(entry: &serde_json::Value) -> bool {
    let Some(endpoints) = entry
        .get("supported_endpoints")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let endpoints = endpoints
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    endpoints.contains(&"/responses") && !endpoints.contains(&"/chat/completions")
}

/// Whether an entry can answer `/chat/completions` or `/responses`. Copilot
/// lists embedding and completion models (`capabilities.type`) and models
/// serving neither endpoint (`supported_endpoints`); entries without that
/// metadata are kept.
fn serves_a_chat_endpoint(entry: &serde_json::Value) -> bool {
    let endpoint_allows_chat = entry
        .get("supported_endpoints")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|endpoints| {
            endpoints.iter().any(|endpoint| {
                matches!(endpoint.as_str(), Some("/chat/completions" | "/responses"))
            })
        });
    let type_allows_chat = entry
        .pointer("/capabilities/type")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|kind| kind == "chat");
    endpoint_allows_chat && type_allows_chat
}

/// Read the usable input size: `capabilities.limits.max_prompt_tokens`,
/// falling back to `max_context_window_tokens`, which also counts the output
/// budget.
fn parse_context_length(entry: &serde_json::Value) -> Option<usize> {
    entry
        .pointer("/capabilities/limits/max_prompt_tokens")
        .or_else(|| entry.pointer("/capabilities/limits/max_context_window_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|length| length as usize)
}

/// Copilot reports `supports.vision` and the document types it accepts
/// (`limits.vision.supported_media_types`). `None` without `supports`.
fn parse_input_modalities(entry: &serde_json::Value) -> Option<Vec<InputModality>> {
    let supports = entry.pointer("/capabilities/supports")?;
    let mut modalities = Vec::new();
    if supports.get("vision").and_then(serde_json::Value::as_bool) == Some(true) {
        modalities.push(InputModality::Image);
        let accepts_documents = entry
            .pointer("/capabilities/limits/vision/supported_media_types")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|types| {
                types
                    .iter()
                    .any(|kind| kind.as_str() == Some("application/pdf"))
            });
        if accepts_documents {
            modalities.push(InputModality::File);
        }
    }
    Some(modalities)
}

/// Read the selectable efforts from `capabilities.supports.reasoning_effort`.
fn parse_reasoning_efforts(entry: &serde_json::Value) -> Option<Vec<String>> {
    let efforts = entry.pointer("/capabilities/supports/reasoning_effort")?;
    Some(order_reasoning_efforts(
        efforts
            .as_array()
            .map(|efforts| {
                efforts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|effort| !effort.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_capability_reasoning_efforts() {
        let body = serde_json::json!({
            "data": [
                {"id": "gpt-6-sol",
                 "capabilities": {"supports": {"reasoning_effort": ["high", "none", "low"]}}},
                {"id": "gpt-4o", "capabilities": {"supports": {"streaming": true}}}
            ]
        });

        let models = CopilotModelCatalog::default().parse(&body);

        assert_eq!(
            models[0].reasoning_efforts,
            Some(["none", "low", "high"].map(String::from).to_vec())
        );
        assert_eq!(models[1].reasoning_efforts, None);
    }

    #[test]
    fn reads_capability_limits_and_vision() {
        let body = serde_json::json!({
            "data": [
                {"id": "with-prompt-limit", "capabilities": {
                    "limits": {
                        "max_context_window_tokens": 200000,
                        "max_prompt_tokens": 100000,
                        "vision": {"supported_media_types": ["image/png", "application/pdf"]}
                    },
                    "supports": {"vision": true}}},
                {"id": "window-only", "capabilities": {
                    "limits": {"max_context_window_tokens": 64000},
                    "supports": {"vision": true}}},
                {"id": "text-only", "capabilities": {"supports": {"streaming": true}}},
                {"id": "unknown"}
            ]
        });

        let models = CopilotModelCatalog::default().parse(&body);

        assert_eq!(models[0].context_length, Some(100000));
        assert_eq!(
            models[0].input_modalities,
            Some(vec![InputModality::Image, InputModality::File])
        );
        assert_eq!(models[1].context_length, Some(64000));
        assert_eq!(models[1].input_modalities, Some(vec![InputModality::Image]));
        assert_eq!(models[2].input_modalities, Some(vec![]));
        assert_eq!(models[3].context_length, None);
        assert_eq!(models[3].input_modalities, None);
    }

    #[test]
    fn drops_models_without_a_chat_endpoint() {
        let body = serde_json::json!({
            "data": [
                {"id": "chat", "supported_endpoints": ["/v1/messages", "/chat/completions"]},
                {"id": "responses-only", "supported_endpoints": ["/responses", "ws:/responses"]},
                {"id": "other-endpoint", "supported_endpoints": ["/v1/messages"]},
                {"id": "embedding", "capabilities": {"type": "embeddings"}},
                {"id": "typed-chat", "capabilities": {"type": "chat"}},
                {"id": "no-metadata"}
            ]
        });

        let ids: Vec<String> = CopilotModelCatalog::default()
            .parse(&body)
            .into_iter()
            .map(|model| model.id)
            .collect();

        assert_eq!(
            ids,
            vec!["chat", "responses-only", "typed-chat", "no-metadata"]
        );
    }

    #[test]
    fn responses_only_models_are_those_listed_without_chat_completions() {
        let catalog = CopilotModelCatalog::default();
        assert!(!catalog.is_loaded());

        catalog.parse(&serde_json::json!({
            "data": [
                {"id": "chat", "supported_endpoints": ["/chat/completions", "/responses"]},
                {"id": "responses-only", "supported_endpoints": ["/responses", "ws:/responses"]},
                {"id": "no-metadata"}
            ]
        }));

        assert!(catalog.is_loaded());
        assert!(catalog.uses_responses_api("responses-only"));
        assert!(!catalog.uses_responses_api("chat"));
        assert!(!catalog.uses_responses_api("no-metadata"));
    }
}
