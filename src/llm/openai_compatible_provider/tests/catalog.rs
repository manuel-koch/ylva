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
