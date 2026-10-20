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

fn decode_all(frames: &[serde_json::Value]) -> Vec<StreamEvent> {
    let mut decoder = ResponsesStreamDecoder::default();
    frames
        .iter()
        .flat_map(|frame| decoder.decode(&frame.to_string()))
        .collect()
}

#[test]
fn input_projects_conversation_and_tool_round_trip() {
    let chat_messages = serde_json::json!([
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "list files"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}
            }]
        },
        {"role": "tool", "tool_call_id": "call-1", "content": "a.txt"},
        {"role": "assistant", "content": "done"}
    ]);

    assert_eq!(
        responses_input(&chat_messages),
        vec![
            serde_json::json!({"role": "system", "content": "be brief"}),
            serde_json::json!({"role": "user", "content": "list files"}),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call-1",
                "name": "ls",
                "arguments": "{\"path\":\".\"}"
            }),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "a.txt"
            }),
            serde_json::json!({"role": "assistant", "content": "done"}),
        ]
    );
}

#[test]
fn input_projects_media_parts() {
    let chat_messages = serde_json::json!([{
        "role": "user",
        "content": [
            {"type": "text", "text": "see"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}},
            {"type": "file", "file": {"file_data": "data:application/pdf;base64,BBB", "filename": "a.pdf"}}
        ]
    }]);

    assert_eq!(
        responses_input(&chat_messages),
        vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "input_text", "text": "see"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAA"},
                {
                    "type": "input_file",
                    "filename": "a.pdf",
                    "file_data": "data:application/pdf;base64,BBB"
                }
            ]
        })]
    );
}

#[test]
fn request_body_flattens_tools_and_carries_reasoning() {
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {"name": "ls", "description": "list", "parameters": {"type": "object"}}
    })];

    let body = responses_request_body(
        "gpt-6-luna",
        &serde_json::json!([{"role": "user", "content": "hi"}]),
        &tools,
        None,
        Some("high"),
    );

    assert_eq!(body["model"], "gpt-6-luna");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(
        body["tools"],
        serde_json::json!([{
            "type": "function",
            "name": "ls",
            "description": "list",
            "parameters": {"type": "object"}
        }])
    );
    assert!(body.get("temperature").is_none());
}

#[test]
fn request_body_omits_optional_fields_when_unset() {
    let body = responses_request_body("m", &serde_json::json!([]), &[], None, None);

    assert!(body.get("tools").is_none());
    assert!(body.get("reasoning").is_none());
}

#[test]
fn decoder_streams_text_thinking_and_completion() {
    let events = decode_all(&[
        serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "hmm"}),
        serde_json::json!({"type": "response.output_text.delta", "delta": "Hello"}),
        serde_json::json!({"type": "response.created"}),
        serde_json::json!({"type": "response.completed", "response": {}}),
    ]);

    assert!(matches!(&events[0], StreamEvent::Thinking(text) if text == "hmm"));
    assert!(matches!(&events[1], StreamEvent::Text(text) if text == "Hello"));
    assert!(matches!(events[2], StreamEvent::Done));
    assert_eq!(events.len(), 3);
}

#[test]
fn decoder_numbers_function_calls_in_order_of_appearance() {
    let events = decode_all(&[
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {"type": "function_call", "call_id": "c1", "name": "ls"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "delta": "{\"a\":"
        }),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 3,
            "item": {"type": "function_call", "call_id": "c2", "name": "cat"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 3,
            "delta": "{}"
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "delta": "1}"
        }),
    ]);

    let chunks: Vec<DeltaChunk> = events
        .into_iter()
        .map(|event| match event {
            StreamEvent::Delta(chunk) => chunk,
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| (
                chunk.tool_call_index,
                chunk.tool_call_id.as_deref(),
                chunk.function_name.as_deref(),
                chunk.arguments_fragment.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![
            (0, Some("c1"), Some("ls"), None),
            (0, None, None, Some("{\"a\":")),
            (1, Some("c2"), Some("cat"), None),
            (1, None, None, Some("{}")),
            (0, None, None, Some("1}")),
        ]
    );
}

#[test]
fn decoder_takes_arguments_from_the_finished_item_only_without_deltas() {
    let events = decode_all(&[
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "function_call", "call_id": "c1", "name": "ls"}
        }),
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {"type": "function_call", "arguments": "{\"x\":1}"}
        }),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {"type": "function_call", "call_id": "c2", "name": "cat"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "delta": "{}"
        }),
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {"type": "function_call", "arguments": "{}"}
        }),
    ]);

    let fragments: Vec<Option<String>> = events
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Delta(chunk) => Some(chunk.arguments_fragment),
            _ => None,
        })
        .collect();
    assert_eq!(
        fragments,
        vec![
            None,
            Some("{\"x\":1}".to_string()),
            None,
            Some("{}".to_string())
        ]
    );
}

#[test]
fn decoder_reports_failures_and_malformed_frames() {
    let mut decoder = ResponsesStreamDecoder::default();

    let failed = decoder
        .decode(r#"{"type":"response.failed","response":{"error":{"message":"quota exceeded"}}}"#);
    let stream_error = decoder.decode(r#"{"type":"error","message":"boom"}"#);
    let malformed = decoder.decode("{not json");
    let blank = decoder.decode("");

    assert!(
        matches!(&failed[0], StreamEvent::Error(error) if error.to_string().contains("quota exceeded"))
    );
    assert!(
        matches!(&stream_error[0], StreamEvent::Error(error) if error.to_string().contains("boom"))
    );
    assert!(matches!(&malformed[0], StreamEvent::Error(_)));
    assert!(blank.is_empty());
}
