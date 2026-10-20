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

#[test]
fn parse_model_list_preserves_context_length() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "openai/gpt-oss-120b",
                "name": "OpenAI: GPT OSS 120B",
                "context_length": 131072
            },
            {"name": "missing-id"}
        ]
    });

    assert_eq!(
        parse_model_list(&body),
        vec![crate::llm::ModelInfo {
            id: "openai/gpt-oss-120b".to_string(),
            context_length: Some(131072),
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        }]
    );
}

#[test]
fn parse_model_list_orders_reasoning_efforts_weakest_to_strongest() {
    let body = serde_json::json!({
        "data": [{
            "id": "provider/unordered",
            "reasoning": { "supported_efforts": ["high", "none", "xhigh", "low"] }
        }]
    });

    assert_eq!(
        parse_model_list(&body)[0].reasoning_efforts,
        Some(vec![
            "none".to_string(),
            "low".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
        ])
    );
}

#[test]
fn parse_model_list_reads_copilot_capability_reasoning_efforts() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "gpt-6-sol",
                "capabilities": {"supports": {"reasoning_effort": ["high", "none", "low"]}}
            },
            {"id": "gpt-4o", "capabilities": {"supports": {"streaming": true}}}
        ]
    });

    let models = parse_model_list(&body);

    assert_eq!(
        models[0].reasoning_efforts,
        Some(vec![
            "none".to_string(),
            "low".to_string(),
            "high".to_string()
        ])
    );
    assert_eq!(models[1].reasoning_efforts, None);
}

#[test]
fn parse_model_list_reads_copilot_capability_limits_and_vision() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "with-prompt-limit",
                "capabilities": {
                    "limits": {
                        "max_context_window_tokens": 200000,
                        "max_prompt_tokens": 100000,
                        "vision": {"supported_media_types": ["image/png", "application/pdf"]}
                    },
                    "supports": {"vision": true}
                }
            },
            {
                "id": "window-only",
                "capabilities": {
                    "limits": {"max_context_window_tokens": 64000},
                    "supports": {"vision": true}
                }
            },
            {"id": "text-only", "capabilities": {"supports": {"streaming": true}}},
            {"id": "unknown"}
        ]
    });

    let models = parse_model_list(&body);

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
fn parse_model_list_drops_models_without_a_chat_endpoint() {
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

    let ids: Vec<String> = parse_model_list(&body)
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
    let known = ResponsesOnlyModels::default();
    assert!(!known.is_loaded());

    known.replace_from_catalog(&serde_json::json!({
        "data": [
            {"id": "chat", "supported_endpoints": ["/chat/completions", "/responses"]},
            {"id": "responses-only", "supported_endpoints": ["/responses", "ws:/responses"]},
            {"id": "no-metadata"}
        ]
    }));

    assert!(known.is_loaded());
    assert!(known.contains("responses-only"));
    assert!(!known.contains("chat"));
    assert!(!known.contains("no-metadata"));
}

#[test]
fn reported_input_tokens_are_read_from_responses_completion_frames() {
    let usage = parse_reported_input_tokens(
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":42}}}"#,
    );

    assert_eq!(
        usage,
        Some((42, "openai_compatible.response.usage.input_tokens"))
    );
}

#[test]
fn parse_model_list_extracts_reasoning_effort_scenarios() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "provider/with-efforts",
                "reasoning": {
                    "supported_efforts": ["low", "high"]
                }
            },
            {
                "id": "provider/without-selector",
                "reasoning": {}
            },
            {"id": "provider/without-reasoning"}
        ]
    });

    let models = parse_model_list(&body);
    assert_eq!(
        models
            .iter()
            .map(|model| model.reasoning_efforts.clone())
            .collect::<Vec<_>>(),
        vec![
            Some(vec!["low".to_string(), "high".to_string()]),
            Some(Vec::new()),
            None,
        ]
    );
}

#[test]
fn parse_model_list_extracts_the_provider_advertised_default_effort() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "provider/with-default",
                "reasoning": {
                    "supported_efforts": ["low", "high"],
                    "default_effort": "high"
                }
            },
            {
                "id": "provider/without-default",
                "reasoning": {"supported_efforts": ["low"]}
            },
            {
                "id": "provider/non-string-default",
                "reasoning": {"default_effort": 3}
            },
            {
                "id": "provider/empty-default",
                "reasoning": {"default_effort": ""}
            },
            {"id": "provider/no-reasoning"}
        ]
    });

    let models = parse_model_list(&body);
    assert_eq!(
        models
            .iter()
            .map(|model| model.reasoning_default.clone())
            .collect::<Vec<_>>(),
        vec![Some("high".to_string()), None, None, None, None,],
        "only a non-empty string default_effort becomes a reasoning_default"
    );
    assert_eq!(
        models[0].reasoning_efforts,
        Some(vec!["low".to_string(), "high".to_string()]),
        "supported_efforts parsing must be unchanged"
    );
}

#[test]
fn parse_model_list_extracts_supported_input_modalities_conservatively() {
    let body = serde_json::json!({
        "data": [
            {
                "id": "vision-and-files",
                "architecture": {"input_modalities": ["text", "image", "file"]}
            },
            {
                "id": "known-text-only",
                "architecture": {"input_modalities": ["text", "audio"]}
            },
            {"id": "unknown"}
        ]
    });

    let models = parse_model_list(&body);
    assert_eq!(
        models
            .iter()
            .map(|model| model.input_modalities.clone())
            .collect::<Vec<_>>(),
        vec![
            Some(vec![InputModality::Image, InputModality::File]),
            Some(vec![]),
            None,
        ]
    );
}
