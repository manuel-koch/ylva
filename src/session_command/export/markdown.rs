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

//! Streaming Markdown projection of the export records. Only correlation
//! metadata is retained between passes.

mod text;

use super::inventory::{Bundle, Edge, EdgeKind, Inventory, IssueReason, Member, Snapshot};
use super::records::{OmissionReason, Processed, Record};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use uuid::Uuid;

use self::text::{arguments, authored, fence, inline_code, single_line};

#[derive(Default)]
pub(super) struct Index {
    title: Option<String>,
    first_starts: HashMap<Uuid, u64>,
    pairs: HashMap<(u64, usize), Pairing>,
    paired_results: HashSet<u64>,
}

#[derive(Clone, Copy)]
enum Pairing {
    ResultAt(u64),
    Ambiguous,
}

#[derive(Default)]
struct CallOccurrences {
    calls: Vec<(u64, usize, bool)>,
    results: Vec<(u64, u64)>,
}

impl Index {
    pub fn build(
        snapshot: &Snapshot,
        processed: &Processed,
        interrupted: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let mut index = Self::default();
        let mut events = RecordReader::open(processed)?;
        let mut original = BufReader::new(File::open(&snapshot.bytes)?);
        let mut original_bytes = Vec::new();
        let mut original_line = 0;
        let mut occurrences = HashMap::<Uuid, CallOccurrences>::new();
        while let Some((offset, record)) = events.next()? {
            check_interruption(interrupted)?;
            let line = record.source.line;
            while original_line < line {
                original_line += 1;
                original_bytes.clear();
                if original.read_until(b'\n', &mut original_bytes)? == 0 {
                    return Err(io::Error::other(
                        "export record is outside the captured snapshot",
                    ));
                }
            }
            let visible = record.event.as_ref();
            if let Some(event) = visible {
                if line == 1 && event["type"] == "meta" {
                    index.title = event["title"]
                        .as_str()
                        .filter(|title| !title.trim().is_empty())
                        .map(single_line);
                }
                if event["type"] == "subagent_start" {
                    if let Some(edge) = edge_at(snapshot, line) {
                        index
                            .first_starts
                            .entry(edge.child_session_id)
                            .or_insert(line);
                    }
                }
            }
            let Ok(fields) = crate::strict_json::object(&original_bytes) else {
                continue;
            };
            if fields.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(message) = fields.get("message") else {
                continue;
            };
            match message["role"].as_str() {
                Some("assistant") => {
                    if let Some(calls) = message["tool_calls"].as_array() {
                        let visible =
                            visible.is_some_and(|event| event["message"]["role"] == "assistant");
                        for (position, call) in calls.iter().enumerate() {
                            if let Some(identity) = call_id(call) {
                                occurrences
                                    .entry(identity)
                                    .or_default()
                                    .calls
                                    .push((line, position, visible));
                            }
                        }
                    }
                }
                Some("tool_result" | "tool_error") => {
                    if let Some(identity) = call_id(message) {
                        occurrences
                            .entry(identity)
                            .or_default()
                            .results
                            .push((line, offset));
                    }
                }
                _ => {}
            }
        }
        for occurrence in occurrences.into_values() {
            match (occurrence.calls.as_slice(), occurrence.results.as_slice()) {
                ([(call_line, position, true)], [(result_line, offset)]) => {
                    index
                        .pairs
                        .insert((*call_line, *position), Pairing::ResultAt(*offset));
                    index.paired_results.insert(*result_line);
                }
                (calls, results) if calls.len() > 1 || results.len() > 1 => {
                    for &(line, position, _) in calls.iter().filter(|call| call.2) {
                        index.pairs.insert((line, position), Pairing::Ambiguous);
                    }
                }
                _ => {}
            }
        }
        Ok(index)
    }
}

fn call_id(value: &Value) -> Option<Uuid> {
    Uuid::parse_str(value["id"].as_str()?).ok()
}

fn edge_at(snapshot: &Snapshot, line: u64) -> Option<&Edge> {
    let position = snapshot
        .edges
        .binary_search_by_key(&line, |edge| edge.line)
        .ok()?;
    snapshot.edges.get(position)
}

struct RecordReader {
    source: BufReader<File>,
    bytes: Vec<u8>,
    offset: u64,
}

impl RecordReader {
    fn open(processed: &Processed) -> io::Result<Self> {
        Ok(Self {
            source: BufReader::new(File::open(&processed.bytes)?),
            bytes: Vec::new(),
            offset: 0,
        })
    }

    fn next(&mut self) -> io::Result<Option<(u64, Record)>> {
        self.bytes.clear();
        let count = self.source.read_until(b'\n', &mut self.bytes)?;
        if count == 0 {
            return Ok(None);
        }
        let offset = self.offset;
        self.offset += count as u64;
        Ok(Some((offset, serde_json::from_slice(&self.bytes)?)))
    }

    fn at(&mut self, offset: u64) -> io::Result<Record> {
        self.source.seek(SeekFrom::Start(offset))?;
        self.offset = offset;
        self.next()?
            .map(|(_, record)| record)
            .ok_or_else(|| io::Error::other("a paired result is missing from the export spool"))
    }
}

pub(super) struct Links<'a> {
    pub bundle: &'a Bundle,
    pub inventory: &'a Inventory,
    pub indexes: &'a HashMap<Uuid, Index>,
    pub bundled: bool,
}

impl Links<'_> {
    fn linked_member(&self, identity: Uuid) -> Option<Member> {
        let number = *self.bundle.numbers.get(&identity)?;
        if self.bundled
            && self.indexes.contains_key(&identity)
            && self
                .bundle
                .members
                .iter()
                .any(|member| member.identity == identity)
        {
            Some(Member { identity, number })
        } else {
            None
        }
    }

    fn reference(&self, identity: Uuid) -> String {
        let label = label(self.bundle.numbers.get(&identity).copied().flatten());
        match self.linked_member(identity) {
            Some(member) => format!("[{label}]({})", member.filename("md")),
            None => label,
        }
    }

    fn fragment(&self, identity: Uuid) -> String {
        let number = self
            .bundle
            .numbers
            .get(&identity)
            .copied()
            .flatten()
            .unwrap_or(0);
        format!("ylva-start:subagent-{number}")
    }

    fn parents(&self, identity: Uuid) -> Option<String> {
        let mut links = Vec::new();
        for parent in &self.bundle.members {
            let Some(parent) = self.linked_member(parent.identity) else {
                continue;
            };
            let Some(snapshot) = self.inventory.snapshot(parent.identity) else {
                continue;
            };
            if !snapshot
                .edges
                .iter()
                .any(|edge| edge.child_session_id == identity)
            {
                continue;
            }
            let mut filename = parent.filename("md");
            if self.indexes[&parent.identity]
                .first_starts
                .contains_key(&identity)
            {
                filename.push('#');
                filename.push_str(&self.fragment(identity));
            }
            links.push(format!("[{}]({filename})", label(parent.number)));
        }
        match links.len() {
            0 => None,
            1 => Some(format!("Parent: {}", links[0])),
            _ => Some(format!("Parents: {}", links.join(", "))),
        }
    }
}

fn label(number: Option<usize>) -> String {
    match number {
        None => "Session".into(),
        Some(number) => format!("Subagent #{number}"),
    }
}

pub(super) fn render(
    output: &mut dyn Write,
    identity: Uuid,
    processed: &Processed,
    links: &Links<'_>,
    interrupted: &impl Fn() -> bool,
) -> io::Result<()> {
    let snapshot = links
        .inventory
        .snapshot(identity)
        .ok_or_else(|| io::Error::other("missing source for Markdown"))?;
    let index = links
        .indexes
        .get(&identity)
        .ok_or_else(|| io::Error::other("missing source index for Markdown"))?;
    let mut markdown = Markdown {
        output,
        wrote_block: false,
    };
    let title = index.title.as_deref().unwrap_or("Session");
    markdown.block(&format!("# {title}"))?;
    if let Some(parents) = links.parents(identity) {
        markdown.block(&parents)?;
    }
    let mut events = RecordReader::open(processed)?;
    let mut results = RecordReader::open(processed)?;
    while let Some((_, record)) = events.next()? {
        check_interruption(interrupted)?;
        let line = record.source.line;
        if index.paired_results.contains(&line) {
            continue;
        }
        let Some(event) = &record.event else {
            markdown.block("> Content withheld.")?;
            continue;
        };
        match event["type"].as_str() {
            Some(
                "meta" | "model_changed" | "agent_type_changed" | "compaction" | "model_projection",
            ) => {}
            Some("subagent_start" | "subagent_result") => {
                let edge = edge_at(snapshot, line)
                    .ok_or_else(|| io::Error::other("missing subagent edge for Markdown"))?;
                markdown.delegation(event, edge, links, index)?;
            }
            Some("message") => {
                markdown.message(&record, event, links, index, snapshot, &mut results)?
            }
            _ => markdown.unknown(event)?,
        }
    }
    if snapshot
        .issues
        .last()
        .is_some_and(|issue| issue.reason == IssueReason::IncompleteTail)
    {
        markdown.block("> Incomplete trailing content omitted.")?;
    }
    Ok(())
}

fn check_interruption(interrupted: &impl Fn() -> bool) -> io::Result<()> {
    if interrupted() {
        return Err(io::Error::other("export interrupted"));
    }
    Ok(())
}

struct Markdown<'a> {
    output: &'a mut dyn Write,
    wrote_block: bool,
}

impl Markdown<'_> {
    fn block(&mut self, block: &str) -> io::Result<()> {
        if self.wrote_block {
            self.output.write_all(b"\n")?;
        }
        self.wrote_block = true;
        self.output.write_all(block.as_bytes())?;
        if !block.ends_with('\n') {
            self.output.write_all(b"\n")?;
        }
        Ok(())
    }

    fn unknown(&mut self, event: &Value) -> io::Result<()> {
        self.block("## Other event")?;
        self.block(&fence("json", &serde_json::to_string(event)?))
    }

    fn message(
        &mut self,
        record: &Record,
        event: &Value,
        links: &Links<'_>,
        index: &Index,
        snapshot: &Snapshot,
        results: &mut RecordReader,
    ) -> io::Result<()> {
        let message = &event["message"];
        let role = match message["role"].as_str() {
            Some("user") if message["kind"] == "implicit_tool_note" => {
                return self.block("> Internal harness notice omitted.")
            }
            Some("user") => "User",
            Some("assistant") => "Assistant",
            Some("system") => "System",
            Some("tool_result" | "tool_error") => {
                self.block(&format!(
                    "## Tool result: {}",
                    inline_code(string(&message["name"]))
                ))?;
                return self.result(record, links, snapshot);
            }
            _ => return self.unknown(event),
        };
        self.block(&format!("## {role}"))?;
        let content = string(&message["content"]);
        if !content.is_empty() {
            self.block(&authored(content))?;
        }
        if role == "Assistant" {
            if message["status"] == "interrupted" {
                self.block("> Assistant response interrupted.")?;
            }
            if let Some(calls) = message["tool_calls"].as_array() {
                let mut totals = HashMap::<&str, usize>::new();
                for call in calls {
                    *totals.entry(string(&call["name"])).or_default() += 1;
                }
                let mut seen = HashMap::<&str, usize>::new();
                for (position, call) in calls.iter().enumerate() {
                    let name = string(&call["name"]);
                    let number = seen.entry(name).or_default();
                    *number += 1;
                    let suffix = if totals[name] > 1 {
                        format!(" #{number}")
                    } else {
                        String::new()
                    };
                    self.block(&format!("### Tool: {}{suffix}", inline_code(name)))?;
                    self.block("**Arguments:**")?;
                    let omit_content = record.omissions.iter().any(|omission| {
                        omission.reason == OmissionReason::EncodedContent
                            && omission.field.as_deref()
                                == Some(&format!(
                                    "/message/tool_calls/{position}/arguments/content"
                                ))
                    });
                    self.block(&arguments(name, &call["arguments"], omit_content))?;
                    self.block("**Result:**")?;
                    match index.pairs.get(&(record.source.line, position)) {
                        Some(Pairing::ResultAt(offset)) => {
                            self.result(&results.at(*offset)?, links, snapshot)?
                        }
                        Some(Pairing::Ambiguous) => self.block("> Result not paired.")?,
                        None => self.block("> Result not recorded.")?,
                    }
                }
            }
        }
        Ok(())
    }

    fn result(
        &mut self,
        record: &Record,
        links: &Links<'_>,
        snapshot: &Snapshot,
    ) -> io::Result<()> {
        let Some(event) = &record.event else {
            return self.block("> Content withheld.");
        };
        let message = &event["message"];
        if let Some(omission) = record
            .omissions
            .iter()
            .find(|omission| omission.field.as_deref() == Some("/message/result"))
        {
            match omission.reason {
                OmissionReason::Media => {
                    let media_types: Vec<_> = message["attachments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|item| matches!(item["type"].as_str(), Some("image" | "file")))
                        .map(|item| {
                            single_line(item["media_type"].as_str().unwrap_or("unknown type"))
                        })
                        .collect();
                    let media_types = if media_types.is_empty() {
                        "unknown type".to_owned()
                    } else {
                        media_types.join(", ")
                    };
                    return self.block(&format!("> Media omitted: {media_types}."));
                }
                OmissionReason::EncodedContent => return self.block("> Encoded content omitted."),
                _ => {}
            }
        }
        if let Some(edge) =
            edge_at(snapshot, record.source.line).filter(|edge| edge.kind == EdgeKind::Admission)
        {
            let status = crate::strict_json::object(string(&message["result"]).as_bytes())
                .ok()
                .and_then(|fields| {
                    fields
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            if let Some(status) = status {
                return self.block(&format!(
                    "{} accepted ({}).",
                    links.reference(edge.child_session_id),
                    inline_code(&status)
                ));
            }
            return self.block("> Content withheld.");
        }
        if message["role"] == "tool_error" {
            self.block("> Tool error:")?;
            self.block(&fence("text", string(&message["error"])))
        } else {
            self.block(&fence("text", string(&message["result"])))
        }
    }

    fn delegation(
        &mut self,
        event: &Value,
        edge: &Edge,
        links: &Links<'_>,
        index: &Index,
    ) -> io::Result<()> {
        let reference = links.reference(edge.child_session_id);
        if edge.kind == EdgeKind::Start {
            if index.first_starts.get(&edge.child_session_id) == Some(&edge.line) {
                self.block(&format!(
                    "<a name=\"{}\"></a>",
                    links.fragment(edge.child_session_id)
                ))?;
            }
            self.block(&format!("## {reference} started"))?;
            let task = string(&event["task"]);
            if text::starts_with_block(task) {
                self.block("Task:")?;
                self.block(&authored(task))
            } else {
                self.block(&authored(&format!("Task: {task}")))
            }
        } else {
            self.block(&format!("## {reference} completed"))?;
            let mut detail = format!("- Status: {}", inline_code(string(&event["status"])));
            if let Some(result) = event["result"].as_str().or_else(|| event["error"].as_str()) {
                detail.push_str("\n- Result:");
                if text::starts_with_block(result) {
                    detail.push_str("\n\n");
                    detail.push_str(&text::indented(&authored(result), 2));
                } else {
                    let result = text::indented(&authored(result), 2);
                    detail.push(' ');
                    detail.push_str(result.strip_prefix("  ").unwrap_or(&result));
                }
            }
            self.block(&detail)
        }
    }
}

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
