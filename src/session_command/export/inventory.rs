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

use crate::hexadecimal::hex_digest;
use crate::store::{ExportCapture, ExportReader, SnapshotError};
use crate::strict_json::{self, ObjectError};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use tempfile::{NamedTempFile, TempPath};
use uuid::Uuid;

pub(super) struct Inventory {
    pub roots: Vec<Uuid>,
    pub sources: HashMap<Uuid, Result<Snapshot, SnapshotError>>,
    pub order: Vec<Uuid>,
}

pub(super) struct Snapshot {
    pub bytes: TempPath,
    pub capture: ExportCapture,
    pub title_slug: Option<String>,
    pub boundary: Boundary,
    pub edges: Vec<Edge>,
    pub issues: Vec<SourceIssue>,
}

struct Discovery {
    boundary: Boundary,
    edges: Vec<Edge>,
    issues: Vec<SourceIssue>,
    title_slug: Option<String>,
}

#[derive(Default, Serialize)]
pub(super) struct Boundary {
    pub logical_bytes: u64,
    pub logical_sha256: String,
    pub event_prefix_bytes: u64,
    pub event_prefix_sha256: String,
    pub header_sha256_without_title: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(super) struct SourceIssue {
    pub line: u64,
    pub reason: IssueReason,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum IssueReason {
    IncompleteTail,
    MalformedEvent,
    DuplicateKeys,
    LegacyDelegation,
    InvalidDelegation,
}

impl IssueReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::IncompleteTail => "incomplete_tail",
            Self::MalformedEvent => "malformed_event",
            Self::DuplicateKeys => "duplicate_keys",
            Self::LegacyDelegation => "legacy_delegation",
            Self::InvalidDelegation => "invalid_delegation",
        }
    }

    pub fn affects_discovery(self) -> bool {
        self != Self::IncompleteTail
    }
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct Edge {
    pub parent_session_id: Uuid,
    pub child_session_id: Uuid,
    pub line: u64,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EdgeKind {
    #[serde(rename = "subagent_start")]
    Start,
    #[serde(rename = "subagent_result")]
    Result,
    #[serde(rename = "subagent_accepted")]
    Admission,
}

pub(super) struct Bundle {
    pub root: Uuid,
    pub members: Vec<Member>,
    pub numbers: HashMap<Uuid, Option<usize>>,
}

#[derive(Clone, Copy)]
pub(super) struct Member {
    pub identity: Uuid,
    pub number: Option<usize>,
}

impl Member {
    pub fn filename(self, extension: &str) -> String {
        match self.number {
            Some(number) => format!("subagent-{number}.{extension}"),
            None => format!("session.{extension}"),
        }
    }
}

pub(super) fn distinct_roots(ids: &[Uuid]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

impl Inventory {
    pub fn capture(
        roots: Vec<Uuid>,
        recursive: bool,
        reader: &Result<ExportReader, SnapshotError>,
        scratch: &Path,
        interrupted: &impl Fn() -> bool,
    ) -> Self {
        let mut pending = VecDeque::from(roots.clone());
        let mut sources = HashMap::new();
        let mut order = Vec::new();
        while let Some(identity) = pending.pop_front() {
            if interrupted() {
                break;
            }
            if sources.contains_key(&identity) {
                continue;
            }
            let source = match reader {
                Ok(reader) => capture_source(identity, reader, scratch, interrupted),
                Err(SnapshotError::Corrupt(reason)) => Err(SnapshotError::Corrupt(reason)),
                Err(error) => Err(SnapshotError::Io(io::Error::other(error.to_string()))),
            };
            if recursive {
                if let Ok(snapshot) = &source {
                    pending.extend(snapshot.edges.iter().map(|edge| edge.child_session_id));
                }
            }
            order.push(identity);
            sources.insert(identity, source);
        }
        Self {
            roots,
            sources,
            order,
        }
    }

    pub fn snapshot(&self, identity: Uuid) -> Option<&Snapshot> {
        self.sources.get(&identity)?.as_ref().ok()
    }

    pub fn incomplete_discovery(&self) -> HashSet<Uuid> {
        let mut parents: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        let mut incomplete = HashSet::new();
        for &identity in &self.order {
            if let Some(snapshot) = self.snapshot(identity) {
                for edge in &snapshot.edges {
                    parents
                        .entry(edge.child_session_id)
                        .or_default()
                        .push(identity);
                }
                if snapshot
                    .issues
                    .iter()
                    .any(|issue| issue.reason.affects_discovery())
                {
                    incomplete.insert(identity);
                }
            } else {
                incomplete.insert(identity);
            }
        }
        let mut pending: Vec<_> = incomplete.iter().copied().collect();
        while let Some(identity) = pending.pop() {
            if let Some(parents) = parents.remove(&identity) {
                pending.extend(
                    parents
                        .into_iter()
                        .filter(|parent| incomplete.insert(*parent)),
                );
            }
        }
        incomplete
    }

    pub fn bundles(&self, recursive: bool) -> Vec<Bundle> {
        self.roots
            .iter()
            .map(|&root| {
                let mut members = vec![Member {
                    identity: root,
                    number: None,
                }];
                let mut numbers = HashMap::from([(root, None)]);
                let mut parent_index = 0;
                while let Some(parent) = members.get(parent_index) {
                    if let Some(snapshot) = self.snapshot(parent.identity) {
                        for edge in &snapshot.edges {
                            let number = Some(numbers.len());
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                numbers.entry(edge.child_session_id)
                            {
                                entry.insert(number);
                                if recursive {
                                    members.push(Member {
                                        identity: edge.child_session_id,
                                        number,
                                    });
                                }
                            }
                        }
                    }
                    parent_index += 1;
                }
                Bundle {
                    root,
                    members,
                    numbers,
                }
            })
            .collect()
    }
}

fn capture_source(
    identity: Uuid,
    reader: &ExportReader,
    scratch: &Path,
    interrupted: &impl Fn() -> bool,
) -> Result<Snapshot, SnapshotError> {
    let mut spool = NamedTempFile::new_in(scratch)?;
    let capture = reader.capture(identity, spool.as_file_mut(), interrupted)?;
    let Discovery {
        boundary,
        edges,
        issues,
        title_slug,
    } = discover(identity, spool.as_file_mut(), interrupted)?;
    Ok(Snapshot {
        bytes: spool.into_temp_path(),
        capture,
        title_slug,
        boundary,
        edges,
        issues,
    })
}

fn discover(
    identity: Uuid,
    source: &mut File,
    interrupted: &impl Fn() -> bool,
) -> Result<Discovery, SnapshotError> {
    let mut boundary = Boundary::default();
    let mut logical = Sha256::new();
    let mut prefix = Sha256::new();
    let mut edges = Vec::new();
    let mut issues = Vec::new();
    let mut title_slug = None;
    let mut source = BufReader::new(source);
    let mut bytes = Vec::new();
    let mut line = 0;
    loop {
        if interrupted() {
            return Err(SnapshotError::Interrupted);
        }
        bytes.clear();
        if source.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        line += 1;
        logical.update(&bytes);
        boundary.logical_bytes += bytes.len() as u64;
        let parsed = strict_json::object(&bytes);
        let header = line == 1
            && parsed
                .as_ref()
                .is_ok_and(|fields| text(fields, "type") == Some("meta"));
        let problem = match parsed {
            Ok(mut fields) if header => {
                title_slug = fields
                    .remove("title")
                    .and_then(|title| super::layout::title_slug(title.as_str()?));
                let mut header = Value::Object(fields);
                header.sort_all_objects();
                boundary.header_sha256_without_title =
                    Some(hex_digest(&Sha256::digest(header.to_string())));
                None
            }
            Ok(fields) => match delegation(&fields) {
                Ok(Some((child, kind))) => {
                    edges.push(Edge {
                        parent_session_id: identity,
                        child_session_id: child,
                        line,
                        kind,
                    });
                    None
                }
                Ok(None) => None,
                Err(reason) => Some(reason),
            },
            Err(ObjectError::DuplicateKeys) => Some(IssueReason::DuplicateKeys),
            Err(ObjectError::Malformed) if bytes.last() != Some(&b'\n') => {
                Some(IssueReason::IncompleteTail)
            }
            Err(ObjectError::Malformed) => Some(IssueReason::MalformedEvent),
        };
        if !header && problem != Some(IssueReason::IncompleteTail) {
            prefix.update(&bytes);
            boundary.event_prefix_bytes += bytes.len() as u64;
        }
        if let Some(reason) = problem {
            issues.push(SourceIssue { line, reason });
        }
    }
    boundary.logical_sha256 = hex_digest(&logical.finalize());
    boundary.event_prefix_sha256 = hex_digest(&prefix.finalize());
    Ok(Discovery {
        boundary,
        edges,
        issues,
        title_slug,
    })
}

fn text<'a>(fields: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    fields.get(name)?.as_str()
}

fn uuid(fields: &Map<String, Value>, name: &str) -> Option<Uuid> {
    Uuid::parse_str(text(fields, name)?).ok()
}

fn delegation(fields: &Map<String, Value>) -> Result<Option<(Uuid, EdgeKind)>, IssueReason> {
    let invalid = IssueReason::InvalidDelegation;
    let (kind, timestamp, payload) = match text(fields, "type") {
        Some("child_start" | "child_result") => return Err(IssueReason::LegacyDelegation),
        Some("subagent_start") => (EdgeKind::Start, "created_at", "task"),
        Some("subagent_result") => (EdgeKind::Result, "completed_at", "status"),
        Some("message") => return admission(fields),
        _ => return Ok(None),
    };
    if uuid(fields, "parent_session").is_none()
        || fields
            .get("call_id")
            .is_some_and(|_| uuid(fields, "call_id").is_none())
        || text(fields, timestamp).is_none()
        || text(fields, payload).is_none()
        || (kind == EdgeKind::Start && text(fields, "agent_type").is_none())
        || (kind == EdgeKind::Result
            && fields
                .get("result")
                .is_some_and(|value| !value.is_null() && !value.is_string()))
    {
        return Err(invalid);
    }
    Ok(Some((uuid(fields, "id").ok_or(invalid)?, kind)))
}

fn admission(fields: &Map<String, Value>) -> Result<Option<(Uuid, EdgeKind)>, IssueReason> {
    let Some(message) = fields.get("message").and_then(Value::as_object) else {
        return Ok(None);
    };
    if text(message, "role") != Some("tool_result")
        || text(message, "name") != Some("spawn_subagent")
    {
        return Ok(None);
    }
    let Some(result) = text(message, "result") else {
        return Err(IssueReason::InvalidDelegation);
    };
    let admission = match strict_json::object(result.as_bytes()) {
        Ok(fields) => fields,
        Err(ObjectError::DuplicateKeys) => return Err(IssueReason::DuplicateKeys),
        Err(ObjectError::Malformed) if result.trim_start().starts_with('{') => {
            return Err(IssueReason::InvalidDelegation)
        }
        Err(ObjectError::Malformed) => return Ok(None),
    };
    if text(&admission, "event") != Some("subagent_accepted") {
        return Ok(None);
    }
    if !matches!(text(&admission, "status"), Some("running" | "queued"))
        || admission.get("tool_call_id").is_some_and(|_| {
            let call_id = uuid(&admission, "tool_call_id");
            call_id.is_none() || call_id != uuid(message, "id")
        })
    {
        return Err(IssueReason::InvalidDelegation);
    }
    Ok(Some((
        uuid(&admission, "run_id").ok_or(IssueReason::InvalidDelegation)?,
        EdgeKind::Admission,
    )))
}
