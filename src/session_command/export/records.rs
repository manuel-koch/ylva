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

use super::inventory::{IssueReason, Snapshot};
use crate::strict_json;
use crate::trufflehog::{Findings, ScanText, Scanner, ScannerError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use tempfile::{NamedTempFile, TempPath};
use uuid::Uuid;

const BATCH_BYTES: usize = 256 * 1024;
const BATCH_RECORDS: usize = 128;
const ENCODED_MARKER: &str = "[Encoded content omitted.]";

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Record {
    pub export_version: u32,
    pub source: SourceLine,
    pub event: Option<Value>,
    pub omissions: Vec<Omission>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct SourceLine {
    pub session_id: Uuid,
    pub line: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct Omission {
    pub reason: OmissionReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OmissionReason {
    MalformedEvent,
    DuplicateKeys,
    InvalidDelegation,
    UnsafeTitle,
    UnsafeStructure,
    UnmappedFinding,
    KeyCollision,
    EncodedContent,
    Media,
    IncompleteTail,
}

impl Record {
    fn withhold(&mut self, reason: OmissionReason) {
        self.event = None;
        self.omissions.push(Omission {
            reason,
            field: (reason == OmissionReason::UnsafeTitle).then(|| "/title".into()),
        });
    }

    fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        serde_json::to_writer(&mut *writer, self)?;
        writer.write_all(b"\n")
    }
}

pub(super) struct Processed {
    pub bytes: TempPath,
    pub title_slug: Option<String>,
    pub omissions: Vec<OmissionReference>,
    pub incomplete: bool,
}

#[derive(Serialize)]
pub(super) struct OmissionReference {
    pub line: u64,
    #[serde(flatten)]
    pub omission: Omission,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ProcessError {
    #[error(transparent)]
    Scanner(#[from] ScannerError),
    #[error("could not read or spool export records")]
    Io(#[from] io::Error),
    #[error("record processing interrupted")]
    Interrupted,
}

pub(super) fn prepare(
    identity: Uuid,
    snapshot: &Snapshot,
    scanner: Option<&Scanner>,
    scratch: &Path,
    interrupted: &impl Fn() -> bool,
) -> Result<Processed, ProcessError> {
    let mut source = BufReader::new(File::open(&snapshot.bytes)?);
    let mut spool = NamedTempFile::new_in(scratch)?;
    let mut writer = BufWriter::new(spool.as_file_mut());
    let mut bytes = Vec::new();
    let mut batch = Vec::new();
    let mut batch_size = 0;
    let mut summary = Summary::default();
    let mut issues = snapshot.issues.iter().peekable();
    let mut line = 0;
    loop {
        if interrupted() {
            return Err(ProcessError::Interrupted);
        }
        bytes.clear();
        if source.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        line += 1;
        let issue = issues
            .next_if(|issue| issue.line == line)
            .map(|issue| issue.reason);
        if issue == Some(IssueReason::IncompleteTail) {
            break;
        }
        let record = build_record(
            SourceLine {
                session_id: identity,
                line,
            },
            &bytes,
            issue,
            scanner.is_some(),
        );
        batch_size += bytes.len();
        batch.push(record);
        if batch_size >= BATCH_BYTES || batch.len() >= BATCH_RECORDS {
            flush_batch(
                &mut writer,
                &mut batch,
                scanner,
                &mut summary,
                scratch,
                interrupted,
            )?;
            batch_size = 0;
        }
    }
    flush_batch(
        &mut writer,
        &mut batch,
        scanner,
        &mut summary,
        scratch,
        interrupted,
    )?;
    if let Some(issue) = snapshot
        .issues
        .last()
        .filter(|issue| issue.reason == IssueReason::IncompleteTail)
    {
        summary.omissions.push(OmissionReference {
            line: issue.line,
            omission: Omission {
                reason: OmissionReason::IncompleteTail,
                field: None,
            },
        });
    }
    writer.flush()?;
    drop(writer);
    Ok(Processed {
        bytes: spool.into_temp_path(),
        title_slug: summary.title_slug,
        omissions: summary.omissions,
        incomplete: summary.incomplete,
    })
}

fn build_record(
    source: SourceLine,
    bytes: &[u8],
    issue: Option<IssueReason>,
    sanitized: bool,
) -> Record {
    let event = strict_json::object(bytes).ok().map(Value::Object);
    let mut record = Record {
        export_version: 1,
        source,
        event,
        omissions: Vec::new(),
    };
    match (issue, record.event.as_ref()) {
        (Some(IssueReason::DuplicateKeys), _) => record.withhold(OmissionReason::DuplicateKeys),
        (_, None) => record.withhold(OmissionReason::MalformedEvent),
        (Some(IssueReason::InvalidDelegation), Some(event))
            if matches!(
                event["type"].as_str(),
                Some("subagent_start" | "subagent_result")
            ) =>
        {
            record.withhold(OmissionReason::InvalidDelegation)
        }
        (_, Some(event))
            if event["type"] == "meta"
                && !event["title"].is_null()
                && !event["title"].is_string() =>
        {
            record.withhold(OmissionReason::UnsafeTitle)
        }
        (_, Some(event)) if !displayable(event) => record.withhold(OmissionReason::MalformedEvent),
        _ => {}
    }
    if let Some(event) = &mut record.event {
        omit_payloads(event, sanitized, &mut record.omissions);
    }
    record
}

fn displayable(event: &Value) -> bool {
    if event["type"] != "message" {
        return true;
    }
    let message = &event["message"];
    match message["role"].as_str() {
        Some("user" | "system") => message["content"].is_string(),
        Some("assistant") => {
            message["content"].is_string()
                && (message["tool_calls"].is_null()
                    || message["tool_calls"].as_array().is_some_and(|calls| {
                        calls
                            .iter()
                            .all(|call| call["name"].is_string() && call.get("arguments").is_some())
                    }))
        }
        Some("tool_result") => message["name"].is_string() && message["result"].is_string(),
        Some("tool_error") => message["name"].is_string() && message["error"].is_string(),
        Some(_) => true,
        None => false,
    }
}

fn omit_payloads(event: &mut Value, sanitized: bool, omissions: &mut Vec<Omission>) {
    if event["type"] != "message" {
        return;
    }
    let Some(message) = event.get_mut("message").and_then(Value::as_object_mut) else {
        return;
    };
    let role = message.get("role").and_then(Value::as_str);
    if role == Some("tool_result") {
        let media = message
            .get("attachments")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| matches!(item["type"].as_str(), Some("image" | "file")))
            });
        let encoded_read = sanitized
            && message.get("name").and_then(Value::as_str) == Some("read")
            && message
                .get("result_metadata")
                .and_then(|meta| meta["encoding"].as_str())
                == Some("base64");
        if media || encoded_read {
            let reason = if media {
                OmissionReason::Media
            } else {
                OmissionReason::EncodedContent
            };
            let marker = if media {
                "[Media omitted.]"
            } else {
                ENCODED_MARKER
            };
            message.insert("result".into(), Value::from(marker));
            omissions.push(Omission {
                reason,
                field: Some("/message/result".into()),
            });
            if message.remove("content_sections").is_some() {
                omissions.push(Omission {
                    reason,
                    field: Some("/message/content_sections".into()),
                });
            }
        }
    } else if role == Some("assistant") && sanitized {
        if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for (index, call) in calls.iter_mut().enumerate() {
                if call["name"] == "write"
                    && call["arguments"]["encoding"] == "base64"
                    && call["arguments"]["content"].is_string()
                {
                    call["arguments"]["content"] = Value::from(ENCODED_MARKER);
                    omissions.push(Omission {
                        reason: OmissionReason::EncodedContent,
                        field: Some(format!("/message/tool_calls/{index}/arguments/content")),
                    });
                }
            }
        }
    }
}

#[derive(Default)]
struct Summary {
    omissions: Vec<OmissionReference>,
    incomplete: bool,
    title_slug: Option<String>,
}

fn flush_batch(
    writer: &mut impl Write,
    records: &mut Vec<Record>,
    scanner: Option<&Scanner>,
    summary: &mut Summary,
    scratch: &Path,
    interrupted: &impl Fn() -> bool,
) -> Result<(), ProcessError> {
    if let Some(scanner) = scanner {
        let mut input = ScanText::default();
        for value in records.iter().filter_map(|record| record.event.as_ref()) {
            input.record(value);
        }
        match scanner.scan(&input, scratch, interrupted)? {
            Findings::Unmapped => withhold_batch(records, OmissionReason::UnmappedFinding),
            Findings::Mapped(secrets) => {
                let mut collision = false;
                for record in records.iter_mut() {
                    if let Some(event) = record.event.take() {
                        let event_kind = display_kind(&event);
                        match scanner.redact(event, &secrets) {
                            Ok(event)
                                if display_kind(&event) == event_kind && displayable(&event) =>
                            {
                                record.event = Some(event);
                            }
                            Ok(_) => record.withhold(OmissionReason::UnsafeStructure),
                            Err(()) => {
                                record.withhold(OmissionReason::KeyCollision);
                                collision = true;
                            }
                        }
                    }
                }
                if collision {
                    withhold_batch(records, OmissionReason::KeyCollision);
                }
            }
        }
    }
    for record in records.drain(..) {
        if interrupted() {
            return Err(ProcessError::Interrupted);
        }
        summary.incomplete |= record.event.is_none();
        if record.source.line == 1 {
            summary.title_slug = record.event.as_ref().and_then(|event| {
                if event["type"] == "meta" {
                    super::layout::title_slug(event["title"].as_str()?)
                } else {
                    None
                }
            });
        }
        record.write(writer)?;
        summary.omissions.extend(
            record
                .omissions
                .into_iter()
                .map(|omission| OmissionReference {
                    line: record.source.line,
                    omission,
                }),
        );
    }
    Ok(())
}

fn withhold_batch(records: &mut [Record], reason: OmissionReason) {
    for record in records {
        if record.event.is_some() {
            record.withhold(reason);
        }
    }
}

fn display_kind(event: &Value) -> Option<&'static str> {
    match event["type"].as_str()? {
        "meta" => Some("meta"),
        "subagent_start" => Some("subagent_start"),
        "subagent_result" => Some("subagent_result"),
        "model_changed" => Some("model_changed"),
        "agent_type_changed" => Some("agent_type_changed"),
        "compaction" => Some("compaction"),
        "model_projection" => Some("model_projection"),
        "message" => match event["message"]["role"].as_str() {
            Some("user") if event["message"]["kind"] == "implicit_tool_note" => Some("implicit"),
            Some("user") => Some("user"),
            Some("system") => Some("system"),
            Some("assistant") => Some("assistant"),
            Some("tool_result") => Some("tool_result"),
            Some("tool_error") => Some("tool_error"),
            _ => Some("message"),
        },
        _ => None,
    }
}
