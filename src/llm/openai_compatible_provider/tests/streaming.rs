use super::*;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/",
            $name
        ))
    };
}

#[test]
fn parse_done_sentinel() {
    assert!(matches!(
        parse_sse_payload("[DONE]"),
        Some(StreamEvent::Done)
    ));
}

#[test]
fn parse_provider_error_frame_as_a_stream_error() {
    let event = parse_sse_payload(
        r#"{"error":{"code":502,"message":"Upstream unavailable","metadata":{"error_type":"provider_unavailable"}}}"#,
    );
    assert!(matches!(event, Some(StreamEvent::Error(error))
            if error.to_string() == "provider returned HTTP 502: Upstream unavailable (provider_unavailable)"));
}

#[test]
fn parses_reported_prompt_and_input_usage_from_usage_only_chunks() {
    assert_eq!(
        parse_reported_input_tokens(r#"{"choices":[],"usage":{"prompt_tokens":17}}"#),
        Some((17, "openai_compatible.usage.prompt_tokens"))
    );
    assert_eq!(
        parse_reported_input_tokens(r#"{"usage":{"input_tokens":23}}"#),
        Some((23, "openai_compatible.usage.input_tokens"))
    );
    assert_eq!(parse_reported_input_tokens(r#"{"choices":[]}"#), None);
    assert_eq!(parse_reported_input_tokens("not json"), None);
}

#[test]
fn parse_empty_data_is_skipped() {
    assert!(parse_sse_payload("").is_none());
}

#[test]
fn parse_content_delta_emits_text() {
    let event = parse_sse_payload(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hello"));
}

#[test]
fn parse_delta_without_content_is_skipped() {
    // `role`-only first chunk.
    assert!(parse_sse_payload(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).is_none());
}

#[test]
fn parse_anthropic_style_thinking_delta_emits_thinking() {
    let event = parse_sse_payload(
        r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":"pondering..."}}]}"#,
    );
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "pondering..."));
}

#[test]
fn parse_empty_anthropic_thinking_delta_is_skipped() {
    assert!(parse_sse_payload(
        r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":""}}]}"#
    )
    .is_none());
}

#[test]
fn parse_reasoning_content_delta_emits_thinking() {
    let event = parse_sse_payload(r#"{"choices":[{"delta":{"reasoning_content":"because..."}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "because..."));
}

#[test]
fn parse_empty_reasoning_content_delta_is_skipped() {
    assert!(parse_sse_payload(r#"{"choices":[{"delta":{"reasoning_content":""}}]}"#).is_none());
}

#[test]
fn parse_bare_reasoning_delta_emits_thinking() {
    // Ollama's and vLLM's OpenAI-compatible endpoints use bare `reasoning`
    // (no `_content` suffix) for the same purpose as `reasoning_content`.
    let event = parse_sse_payload(r#"{"choices":[{"delta":{"reasoning":"thinking..."}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "thinking..."));
}

#[test]
fn parse_empty_bare_reasoning_delta_is_skipped() {
    assert!(parse_sse_payload(r#"{"choices":[{"delta":{"reasoning":""}}]}"#).is_none());
}

#[test]
fn parse_content_prefers_over_reasoning_content() {
    // If a delta somehow carries both, user-facing content wins.
    let event = parse_sse_payload(
        r#"{"choices":[{"delta":{"content":"hi","reasoning_content":"because..."}}]}"#,
    );
    assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hi"));
}

#[test]
fn parse_tool_call_chunk_emits_delta() {
    let event = parse_sse_payload(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
    );
    match event {
        Some(StreamEvent::Delta(delta)) => {
            assert_eq!(delta.tool_call_index, 0);
            assert_eq!(delta.tool_call_id.as_deref(), Some("call_1"));
            assert_eq!(delta.function_name.as_deref(), Some("read"));
            assert_eq!(delta.arguments_fragment.as_deref(), Some(""));
        }
        other => panic!("expected Delta, got {other:?}"),
    }
}

#[test]
fn parse_arguments_fragment_delta_has_no_name_or_id() {
    let payload = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\"" } }]
            }
        }]
    })
    .to_string();
    let event = parse_sse_payload(&payload);
    match event {
        Some(StreamEvent::Delta(delta)) => {
            assert_eq!(delta.tool_call_index, 0);
            assert_eq!(delta.tool_call_id, None);
            assert_eq!(delta.function_name, None);
            assert_eq!(delta.arguments_fragment.as_deref(), Some("{\"path\""));
        }
        other => panic!("expected Delta, got {other:?}"),
    }
}

#[test]
fn parse_empty_content_is_skipped() {
    assert!(parse_sse_payload(r#"{"choices":[{"delta":{"content":""}}]}"#).is_none());
}

#[test]
fn parse_malformed_json_is_error() {
    assert!(matches!(
        parse_sse_payload("{not json"),
        Some(StreamEvent::Error(_))
    ));
}

#[test]
fn plain_stream_fixture_produces_text_in_order() {
    let data = fixture!("plain_stream.sse");
    let texts: Vec<String> = data
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(parse_sse_payload)
        .filter_map(|event| match event {
            StreamEvent::Text(text) => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Hello", " ", "there", " "]);
}

#[test]
fn comment_stream_fixture_tolerates_comment_lines() {
    let data = fixture!("comment_stream.sse");
    let mut texts = Vec::new();
    let mut done = false;
    for line in data.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            match parse_sse_payload(payload) {
                Some(StreamEvent::Text(text)) => texts.push(text),
                Some(StreamEvent::Done) => done = true,
                Some(StreamEvent::Error(_)) => panic!("comment fixture must not error"),
                Some(StreamEvent::Cancelled) => {}
                Some(StreamEvent::Delta(_)) => {}
                Some(StreamEvent::Thinking(_)) => {}
                None => {}
            }
        }
    }
    assert!(done, "comment fixture must still reach [DONE]");
    assert_eq!(texts, vec!["One", "Two"]);
}

#[test]
fn reasoning_fixture_skips_reasoning_only_delta() {
    let data = fixture!("reasoning_stream.sse");
    let mut texts: Vec<String> = Vec::new();
    let mut done = false;
    for line in data.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            match parse_sse_payload(payload) {
                Some(StreamEvent::Text(text)) => texts.push(text),
                Some(StreamEvent::Done) => done = true,
                Some(StreamEvent::Error(_)) => panic!("reasoning fixture must not error"),
                Some(StreamEvent::Cancelled) => {}
                Some(StreamEvent::Delta(_)) => {}
                Some(StreamEvent::Thinking(_)) => {}
                None => {}
            }
        }
    }
    assert!(done);
    // The reasoning-only delta carries no content and is skipped; only the
    // final `content` delta surfaces as user-visible text.
    assert_eq!(texts, vec!["final"]);
}
