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

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use serde_json::Value;
use std::fmt::Write as _;

fn visible(text: &str) -> String {
    let mut rendered = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                characters.next_if_eq(&'\n');
                rendered.push('\n');
            }
            '\n' | '\t' => rendered.push(character),
            control if control.is_control() => rendered.extend(control.escape_unicode()),
            other => rendered.push(other),
        }
    }
    rendered
}

pub(super) fn single_line(text: &str) -> String {
    escape(&visible(text).replace(['\n', '\t', '\u{2028}', '\u{2029}'], " "))
}

fn escape(text: &str) -> String {
    let mut output = String::new();
    let tail = text.trim_start();
    let marker_start = text.len() - tail.len();
    let ordered_end = marker_start + tail.bytes().take_while(u8::is_ascii_digit).count();
    let bare_marker = tail.starts_with(['+', '-', '='])
        && tail.chars().all(|character| tail.starts_with(character));
    for (index, character) in text.char_indices() {
        let list_marker = matches!(character, '+' | '-' | '=')
            && (index == marker_start
                && tail
                    .trim_start_matches(character)
                    .starts_with(char::is_whitespace)
                || bare_marker);
        let ordered_marker = matches!(character, '.' | ')')
            && index == ordered_end
            && index > marker_start
            && text[index + character.len_utf8()..].starts_with(char::is_whitespace);
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~' | '!' | '&'
        ) || list_marker
            || ordered_marker
        {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn field_key(key: &str) -> String {
    if key.is_empty() {
        return "\"\"".to_owned();
    }
    let mut output = String::new();
    for character in key.chars() {
        match character {
            '\\' => output.push_str(r"\\\\"),
            '\n' => output.push_str(r"\n"),
            '\r' => output.push_str(r"\r"),
            '\t' => output.push_str(r"\t"),
            '\u{0008}' => output.push_str(r"\b"),
            '\u{000c}' => output.push_str(r"\f"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => {
                if matches!(
                    character,
                    '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~' | '!' | '&'
                ) {
                    output.push('\\');
                }
                output.push(character);
            }
        }
    }
    output
}

fn longest_backticks(text: &str) -> usize {
    let mut current = 0;
    let mut longest = 0;
    for byte in text.bytes() {
        if byte == b'`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

pub(super) fn inline_code(text: &str) -> String {
    let content = visible(text).replace(['\n', '\t', '\u{2028}', '\u{2029}'], " ");
    let delimiter = "`".repeat(longest_backticks(&content) + 1);
    if content.is_empty() {
        return "`\"\"`".to_owned();
    }
    let pad = content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ')
            && content.ends_with(' ')
            && !content.trim_matches(' ').is_empty());
    let padding = if pad { " " } else { "" };
    format!("{delimiter}{padding}{content}{padding}{delimiter}")
}

pub(super) fn fence(language: &str, body: &str) -> String {
    let body = visible(body);
    let delimiter = "`".repeat((longest_backticks(&body) + 1).max(3));
    let newline = if body.is_empty() || body.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{delimiter}{language}\n{body}{newline}{delimiter}")
}

pub(super) fn indented(text: &str, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        if line != "\n" && !line.is_empty() {
            output.push_str(&prefix);
        }
        output.push_str(line);
    }
    output
}

pub(super) fn authored(text: &str) -> String {
    let mut body = visible(text);
    match boundary(&body) {
        Boundary::Safe => return body,
        Boundary::UnclosedAt(start) => {
            let opening = body[start..].trim_start();
            if let Some(closing) = closure(opening) {
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                body.push_str(&closing);
                if matches!(boundary(&body), Boundary::Safe) {
                    return body;
                }
            }
        }
        Boundary::Hidden => {}
    }
    // Quote the body as literal Markdown only when a following generated
    // heading would otherwise be consumed by an unterminated construct.
    literal(text)
}

pub(super) fn starts_with_block(text: &str) -> bool {
    Parser::new(text).next().is_some_and(|event| {
        matches!(
            event,
            Event::Start(
                Tag::CodeBlock(_)
                    | Tag::HtmlBlock
                    | Tag::Heading { .. }
                    | Tag::List(_)
                    | Tag::BlockQuote(_)
            )
        )
    })
}

enum Boundary {
    Safe,
    UnclosedAt(usize),
    Hidden,
}

fn boundary(body: &str) -> Boundary {
    let mut probe = String::with_capacity(body.len() + 30);
    probe.push_str(body);
    probe.push_str("\n\n");
    let boundary = probe.len();
    probe.push_str("## Ylva structural boundary\n");
    let mut unclosed = None;
    for (event, range) in Parser::new(&probe).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) if range.start == boundary => return Boundary::Safe,
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)) | Tag::HtmlBlock)
                if range.start < boundary && range.end > boundary =>
            {
                unclosed = Some(range.start)
            }
            _ => {}
        }
    }
    unclosed.map_or(Boundary::Hidden, Boundary::UnclosedAt)
}

fn closure(opening: &str) -> Option<String> {
    let character = opening.chars().next()?;
    if matches!(character, '`' | '~') {
        return Some(
            character.to_string().repeat(
                opening
                    .chars()
                    .take_while(|&item| item == character)
                    .count(),
            ),
        );
    }
    let lowercase = opening
        .get(..opening.len().min(12))
        .unwrap_or(opening)
        .to_ascii_lowercase();
    [
        ("<!--", "-->"),
        ("<?", "?>"),
        ("<![cdata[", "]]>"),
        ("<pre", "</pre>"),
        ("<script", "</script>"),
        ("<style", "</style>"),
        ("<textarea", "</textarea>"),
        ("<!", ">"),
    ]
    .into_iter()
    .find(|(prefix, _)| lowercase.starts_with(prefix))
    .map(|(_, ending)| ending.to_owned())
}

fn literal(text: &str) -> String {
    visible(text)
        .split('\n')
        .map(escape)
        .collect::<Vec<_>>()
        .join("\\\n")
}

pub(super) fn arguments(tool: &str, value: &Value, omit_content: bool) -> String {
    render_value(value, Some((tool, omit_content)))
}

fn render_value(value: &Value, tool: Option<(&str, bool)>) -> String {
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            let mut output = String::new();
            let mut separated = false;
            for (key, value) in fields {
                if !output.is_empty() {
                    output.push('\n');
                    if separated {
                        output.push('\n');
                    }
                }
                let _ = write!(output, "- **{}:**", field_key(key));
                let field = tool.map_or(Field::Ordinary, |(tool, omit_content)| {
                    classify_field(tool, key, omit_content, value)
                });
                let (body, block) = match field {
                    Field::Omitted => (r"\[Encoded content omitted.\]".to_owned(), false),
                    Field::Fence(language) => {
                        (fence(language, value.as_str().unwrap_or_default()), true)
                    }
                    Field::Markdown => (authored(value.as_str().unwrap_or_default()), true),
                    Field::Ordinary => (render_value(value, None), container(value)),
                };
                separated = block;
                if block {
                    output.push_str("\n\n");
                    output.push_str(&indented(&body, 2));
                } else {
                    output.push(' ');
                    output.push_str(&continuation(&body, 2));
                }
            }
            output
        }
        Value::Array(values) if !values.is_empty() => {
            let mut output = String::new();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push('\n');
                }
                let marker = format!("{}.", index + 1);
                let width = marker.len() + 1;
                output.push_str(&marker);
                let body = render_value(value, None);
                if container(value) {
                    output.push('\n');
                    output.push_str(&indented(&body, width));
                } else {
                    output.push(' ');
                    output.push_str(&continuation(&body, width));
                }
            }
            output
        }
        Value::Null => "`null`".to_owned(),
        Value::String(value) if value.is_empty() => "`\"\"`".to_owned(),
        Value::String(value) => literal(value),
        Value::Object(_) => "`{}`".to_owned(),
        Value::Array(_) => "`[]`".to_owned(),
        other => other.to_string(),
    }
}

fn continuation(text: &str, indent: usize) -> String {
    match text.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{}", indented(rest, indent)),
        None => text.to_owned(),
    }
}

fn container(value: &Value) -> bool {
    value.as_array().is_some_and(|values| !values.is_empty())
        || value.as_object().is_some_and(|fields| !fields.is_empty())
}

enum Field {
    Ordinary,
    Omitted,
    Fence(&'static str),
    Markdown,
}

fn classify_field(tool: &str, key: &str, omit_content: bool, value: &Value) -> Field {
    if omit_content && key == "content" {
        return Field::Omitted;
    }
    if value.as_str().is_none_or(str::is_empty) {
        return Field::Ordinary;
    }
    match (tool, key) {
        ("bash", "command") => Field::Fence("bash"),
        ("write", "content") | ("edit", "old_string" | "new_string") => Field::Fence("text"),
        ("spawn_subagent", "task" | "briefing")
        | ("clarify", "question" | "context")
        | ("agent_finish", "result" | "error") => Field::Markdown,
        _ => Field::Ordinary,
    }
}
