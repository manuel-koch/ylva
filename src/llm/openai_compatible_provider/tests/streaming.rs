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

fn parse_single_sse_event(data: &str) -> Option<StreamEvent> {
    let mut events = parse_sse_payload(data);
    assert!(
        events.len() <= 1,
        "expected at most one event, got {events:?}"
    );
    events.pop()
}

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
fn mixed_display_and_tool_calls_preserve_precedence_and_order() {
    let cases = [
        (
            serde_json::json!({"content":"answer", "reasoning_content":"hidden", "reasoning":"hidden"}),
            "answer",
            false,
        ),
        (
            serde_json::json!({"content":"", "reasoning_content":"reason", "reasoning":"hidden"}),
            "reason",
            true,
        ),
        (
            serde_json::json!({"reasoning_content":"", "reasoning":"bare"}),
            "bare",
            true,
        ),
        (
            serde_json::json!({"type":"thinking_delta", "thinking":"thought", "content":"hidden", "reasoning":"hidden"}),
            "thought",
            true,
        ),
        (
            serde_json::json!({"type":"thinking_delta", "thinking":"", "content":"fallback"}),
            "fallback",
            false,
        ),
    ];
    for (mut delta, expected, thinking) in cases {
        delta["tool_calls"] = serde_json::json!([
            {"index": 2, "id": "first", "function": {"name": "read", "arguments": "prefix"}},
            {"index": 7, "id": "second", "function": {"name": "write", "arguments": "fragment"}}
        ]);
        let payload = serde_json::json!({"choices": [{"delta": delta}]}).to_string();
        let events = parse_sse_payload(&payload);
        assert_eq!(events.len(), 3);
        assert!(match &events[0] {
            StreamEvent::Thinking(text) => thinking && text == expected,
            StreamEvent::Text(text) => !thinking && text == expected,
            _ => false,
        });
        for (event, index, id, name, fragment) in [
            (&events[1], 2, "first", "read", "prefix"),
            (&events[2], 7, "second", "write", "fragment"),
        ] {
            match event {
                StreamEvent::Delta(delta) => {
                    assert_eq!(delta.tool_call_index, index);
                    assert_eq!(delta.tool_call_id.as_deref(), Some(id));
                    assert_eq!(delta.function_name.as_deref(), Some(name));
                    assert_eq!(delta.arguments_fragment.as_deref(), Some(fragment));
                }
                other => panic!("expected delta: {other:?}"),
            }
        }
    }
}

#[test]
fn mixed_empty_display_and_argument_only_continuation() {
    let payload = serde_json::json!({"choices":[{"delta": {"content":"", "reasoning_content":"", "reasoning":"", "tool_calls":[{"index":3, "function":{"arguments":"tail"}}]}}]}).to_string();
    let events = parse_sse_payload(&payload);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], StreamEvent::Delta(delta) if delta.tool_call_index == 3 && delta.tool_call_id.is_none() && delta.function_name.is_none() && delta.arguments_fragment.as_deref() == Some("tail"))
    );
}

#[test]
fn parse_done_sentinel() {
    assert!(matches!(
        parse_single_sse_event("[DONE]"),
        Some(StreamEvent::Done)
    ));
}

#[test]
fn parse_provider_error_frame_as_a_stream_error() {
    let event = parse_single_sse_event(
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
    assert!(parse_single_sse_event("").is_none());
}

#[test]
fn parse_content_delta_emits_text() {
    let event = parse_single_sse_event(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hello"));
}

#[test]
fn parse_delta_without_content_is_skipped() {
    // `role`-only first chunk.
    assert!(parse_single_sse_event(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).is_none());
}

#[test]
fn parse_anthropic_style_thinking_delta_emits_thinking() {
    let event = parse_single_sse_event(
        r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":"pondering..."}}]}"#,
    );
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "pondering..."));
}

#[test]
fn parse_empty_anthropic_thinking_delta_is_skipped() {
    assert!(parse_single_sse_event(
        r#"{"choices":[{"delta":{"type":"thinking_delta","thinking":""}}]}"#
    )
    .is_none());
}

#[test]
fn parse_reasoning_content_delta_emits_thinking() {
    let event =
        parse_single_sse_event(r#"{"choices":[{"delta":{"reasoning_content":"because..."}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "because..."));
}

#[test]
fn parse_empty_reasoning_content_delta_is_skipped() {
    assert!(
        parse_single_sse_event(r#"{"choices":[{"delta":{"reasoning_content":""}}]}"#).is_none()
    );
}

#[test]
fn parse_bare_reasoning_delta_emits_thinking() {
    // Ollama's and vLLM's OpenAI-compatible endpoints use bare `reasoning`
    // (no `_content` suffix) for the same purpose as `reasoning_content`.
    let event = parse_single_sse_event(r#"{"choices":[{"delta":{"reasoning":"thinking..."}}]}"#);
    assert!(matches!(event, Some(StreamEvent::Thinking(text)) if text == "thinking..."));
}

#[test]
fn parse_empty_bare_reasoning_delta_is_skipped() {
    assert!(parse_single_sse_event(r#"{"choices":[{"delta":{"reasoning":""}}]}"#).is_none());
}

#[test]
fn parse_content_prefers_over_reasoning_content() {
    // If a delta somehow carries both, user-facing content wins.
    let event = parse_single_sse_event(
        r#"{"choices":[{"delta":{"content":"hi","reasoning_content":"because..."}}]}"#,
    );
    assert!(matches!(event, Some(StreamEvent::Text(text)) if text == "hi"));
}

#[test]
fn parse_tool_call_chunk_emits_delta() {
    let events = parse_openai_compatible_sse_payload(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
    );
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Delta(delta) => {
            assert_eq!(delta.tool_call_index, 0);
            assert_eq!(delta.tool_call_id.as_deref(), Some("call_1"));
            assert_eq!(delta.function_name.as_deref(), Some("read"));
            assert_eq!(delta.arguments_fragment.as_deref(), Some(""));
        }
        other => panic!("expected Delta, got {other:?}"),
    }
}

#[test]
fn parse_multiple_tool_calls_in_one_frame() {
    let payload = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [
                    {"index": 2, "id": "call_read", "type": "function", "function": {"name": "read", "arguments": r#"{"path":"#}},
                    {"index": 5, "id": "call_write", "type": "function", "function": {"name": "write", "arguments": r#"{"path":"out.txt"}"#}}
                ]
            }
        }]
    })
    .to_string();
    let events = parse_openai_compatible_sse_payload(&payload);
    assert_eq!(events.len(), 2);
    let deltas: Vec<_> = events
        .iter()
        .map(|event| match event {
            StreamEvent::Delta(delta) => delta,
            other => panic!("expected Delta, got {other:?}"),
        })
        .collect();
    assert_eq!(deltas[0].tool_call_index, 2);
    assert_eq!(deltas[0].tool_call_id.as_deref(), Some("call_read"));
    assert_eq!(deltas[0].function_name.as_deref(), Some("read"));
    assert_eq!(deltas[0].arguments_fragment.as_deref(), Some(r#"{"path":"#));
    assert_eq!(deltas[1].tool_call_index, 5);
    assert_eq!(deltas[1].tool_call_id.as_deref(), Some("call_write"));
    assert_eq!(deltas[1].function_name.as_deref(), Some("write"));
    assert_eq!(
        deltas[1].arguments_fragment.as_deref(),
        Some(r#"{"path":"out.txt"}"#)
    );
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
    let event = parse_single_sse_event(&payload);
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
    assert!(parse_single_sse_event(r#"{"choices":[{"delta":{"content":""}}]}"#).is_none());
}

#[test]
fn parse_malformed_json_is_error() {
    assert!(matches!(
        parse_single_sse_event("{not json"),
        Some(StreamEvent::Error(_))
    ));
}

#[test]
fn plain_stream_fixture_produces_text_in_order() {
    let data = fixture!("plain_stream.sse");
    let texts: Vec<String> = data
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .flat_map(parse_sse_payload)
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
            match parse_single_sse_event(payload) {
                Some(StreamEvent::Text(text)) => texts.push(text),
                Some(StreamEvent::Done) => done = true,
                Some(StreamEvent::Error(_)) => panic!("comment fixture must not error"),
                Some(StreamEvent::OutputTruncated(_)) => {}
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
            match parse_single_sse_event(payload) {
                Some(StreamEvent::Text(text)) => texts.push(text),
                Some(StreamEvent::Done) => done = true,
                Some(StreamEvent::Error(_)) => panic!("reasoning fixture must not error"),
                Some(StreamEvent::OutputTruncated(_)) => {}
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
