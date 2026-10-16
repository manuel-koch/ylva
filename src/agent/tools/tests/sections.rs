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
fn section_content_roundtrip() {
    let section = Section::Content(ContentSection {
        label: None,
        content: "hello".to_string(),
        file_extension: Some("rs".to_string()),
        line_number_offset: Some(0),
    });
    assert!(matches!(section, Section::Content(_)));
}

#[test]
fn section_diff_variant() {
    let section = Section::Diff(DiffSection {
        label: Some("file.rs".to_string()),
        content: "+hello\n-world".to_string(),
    });
    assert!(matches!(section, Section::Diff(_)));
}

#[test]
fn section_search_result_variant() {
    let section = Section::SearchResult(SearchResultSection {
        file_path: "file.rs".to_string(),
        file_extension: Some("rs".to_string()),
        lines: vec![SearchResultLine {
            line_number: 42,
            is_match: true,
            content: "fn main() {}".to_string(),
        }],
    });
    assert!(matches!(section, Section::SearchResult(_)));
}

#[test]
fn tool_call_result_default_metadata_is_none() {
    let output = SimpleOutput("test".to_string());
    assert!(output.metadata().is_none());
    assert!(output.content_sections().is_empty());
}

#[test]
fn tool_call_default_pending_sections_and_argument_order() {
    let call = TestToolCall {
        id: uuid::Uuid::new_v4(),
        arguments: serde_json::json!({ "field": "val" }),
        timeout_secs: 1,
    };
    assert!(call.pending_content_sections().is_empty());
    assert_eq!(call.argument_order("any_key"), usize::MAX);
    assert!(!call.is_default_argument("any_key", &serde_json::json!("value")));
}

#[test]
fn tool_default_is_default_argument_returns_false() {
    let tool = TestTool;
    assert!(!tool.is_default_argument("field", &serde_json::json!("value")));
}
