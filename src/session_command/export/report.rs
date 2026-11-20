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

//! Content-free export outcomes shared by the manifest and stderr.

use super::destination::publication::{PublishError, Publisher};
use super::destination::Destination;
use super::inventory::{Boundary, Edge, Inventory, Member, SourceIssue};
use super::records::{OmissionReason, OmissionReference, Processed};
use crate::exit_code::ExitCode;
use crate::hexadecimal::hex_digest;
use crate::home_relative_path::quote_path;
use crate::store::{ExportRepresentation, SnapshotError};
use serde::{Serialize, Serializer};
use std::io::{self, BufWriter, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Success,
    Failed,
    Interrupted,
}

impl Status {
    pub fn from_outcomes(failed: bool, interrupted: bool) -> Self {
        match (failed, interrupted) {
            (_, true) => Self::Interrupted,
            (true, false) => Self::Failed,
            (false, false) => Self::Success,
        }
    }

    pub fn exit_code(self) -> ExitCode {
        match self {
            Self::Success => ExitCode::Success,
            Self::Failed => ExitCode::GeneralError,
            Self::Interrupted => ExitCode::Interrupted,
        }
    }
}

#[derive(Default)]
pub(super) struct Outcomes<'a> {
    pub bundles: Vec<Bundle>,
    pub outputs: Vec<Output<'a>>,
}

#[derive(Serialize)]
pub(super) struct Bundle {
    root_session_id: Uuid,
    #[serde(serialize_with = "serialize_path")]
    directory: Option<PathBuf>,
    #[serde(serialize_with = "serialize_path")]
    destination: Option<PathBuf>,
    stdout: bool,
    status: BundleStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum BundleStatus {
    Published,
    Partial,
    Failed,
    Unattempted,
    Uncertain,
}

impl Bundle {
    pub fn new(root: Uuid, directory: Option<&Path>, outputs: &[Output<'_>]) -> Self {
        let publication_count = outputs
            .iter()
            .filter(|output| output.publication_status == PublicationStatus::Published)
            .count();
        let mut status = BundleStatus::Unattempted;
        if publication_count == outputs.len() {
            status = BundleStatus::Published;
        } else if publication_count != 0 {
            status = BundleStatus::Partial;
        } else if outputs
            .iter()
            .any(|output| output.publication_status == PublicationStatus::Uncertain)
        {
            status = BundleStatus::Uncertain;
        } else if outputs.iter().any(Output::failed) || !outputs.iter().any(Output::has_target) {
            status = BundleStatus::Failed;
        }
        let standalone = outputs.first().filter(|_| directory.is_none());
        Self {
            root_session_id: root,
            directory: directory.map(Path::to_path_buf),
            destination: standalone.and_then(|output| output.destination.clone()),
            stdout: standalone.is_some_and(|output| output.stdout),
            status,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Output<'a> {
    root_session_id: Uuid,
    session_id: Uuid,
    number: Option<usize>,
    #[serde(serialize_with = "serialize_path")]
    destination: Option<PathBuf>,
    stdout: bool,
    publication_status: PublicationStatus,
    content_completeness: Option<Completeness>,
    output_sha256: Option<String>,
    scanner_version: Option<&'a str>,
    omission_count: usize,
    omissions: &'a [OmissionReference],
    #[serde(skip)]
    error: Option<PublishError>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublicationStatus {
    Published,
    Failed,
    Unattempted,
    Uncertain,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Completeness {
    Complete,
    Incomplete,
}

impl<'a> Output<'a> {
    pub fn new(
        root: Uuid,
        member: Member,
        processed: Option<&'a Processed>,
        scanner_version: Option<&'a str>,
    ) -> Self {
        let omissions = processed.map_or(&[][..], |data| data.omissions.as_slice());
        Self {
            root_session_id: root,
            session_id: member.identity,
            number: member.number,
            destination: None,
            stdout: false,
            publication_status: PublicationStatus::Unattempted,
            content_completeness: None,
            output_sha256: None,
            scanner_version,
            omission_count: omissions.len(),
            omissions,
            error: None,
        }
    }

    pub fn set_target(&mut self, destination: Option<&Destination>, processed: Option<&Processed>) {
        self.destination = destination.map(Destination::path);
        self.stdout = destination.is_none();
        let incomplete = processed.is_some_and(|data| {
            data.incomplete
                || data.omissions.last().is_some_and(|reference| {
                    reference.omission.reason == OmissionReason::IncompleteTail
                })
        });
        self.content_completeness = Some(if incomplete {
            Completeness::Incomplete
        } else {
            Completeness::Complete
        });
    }

    pub fn finish(&mut self, result: Result<String, PublishError>) {
        match result {
            Ok(digest) => {
                self.publication_status = PublicationStatus::Published;
                self.output_sha256 = Some(digest);
            }
            Err(error) => {
                self.publication_status = match error {
                    PublishError::Failed(_) => PublicationStatus::Failed,
                    PublishError::Uncertain(_) => PublicationStatus::Uncertain,
                };
                self.error = Some(error);
            }
        }
    }

    pub fn failed(&self) -> bool {
        matches!(
            self.publication_status,
            PublicationStatus::Failed | PublicationStatus::Uncertain
        )
    }

    fn has_target(&self) -> bool {
        self.stdout || self.destination.is_some()
    }

    pub fn report(&self, diagnostics: &mut dyn Write) -> io::Result<()> {
        let identity = self.session_id;
        let root = self.root_session_id;
        let target = match &self.destination {
            Some(path) => quote_path(path),
            None if self.stdout => "stdout".into(),
            None => "no staged transcript".into(),
        };
        match &self.error {
            Some(error) => {
                let status = match self.publication_status {
                    PublicationStatus::Uncertain => "uncertain",
                    _ if self.stdout => "stdout failed (may contain a partial payload)",
                    _ => "failed",
                };
                writeln!(
                    diagnostics,
                    "error: session {identity} for root {root}: {status} {target}: {error}"
                )
            }
            None if self.publication_status == PublicationStatus::Published => {
                let completeness =
                    if matches!(self.content_completeness, Some(Completeness::Incomplete)) {
                        "incomplete"
                    } else {
                        "complete"
                    };
                writeln!(
                    diagnostics,
                    "published session {identity} for root {root}: {target} ({completeness} content, {} omission(s))",
                    self.omission_count
                )
            }
            None => {
                writeln!(
                    diagnostics,
                    "session {identity} for root {root}: unattempted ({target})"
                )
            }
        }
    }
}

#[derive(Serialize)]
pub(super) struct Manifest<'a> {
    version: u32,
    source_home: ManifestPath<'a>,
    project_key: &'a str,
    launch_directory: ManifestPath<'a>,
    selected_ids: &'a [Uuid],
    sessions: Vec<Session<'a>>,
    bundles: &'a [Bundle],
    outputs: &'a [Output<'a>],
    edges: Vec<&'a Edge>,
    status: Status,
}

#[derive(Serialize)]
struct Session<'a> {
    session_id: Uuid,
    representation: Option<ExportRepresentation>,
    source_paths: Option<SourcePaths<'a>>,
    availability: Availability,
    boundary: Option<&'a Boundary>,
    discovery: Discovery,
    issues: &'a [SourceIssue],
}

#[derive(Serialize)]
struct SourcePaths<'a> {
    plain: Option<ManifestPath<'a>>,
    gzip: Option<ManifestPath<'a>>,
    sidecar: Option<ManifestPath<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Available,
    Missing,
    Unstable,
    Corrupt,
    IoError,
    Interrupted,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Discovery {
    Complete,
    Incomplete,
    NotRequested,
}

impl<'a> Manifest<'a> {
    pub fn new(
        source_home: &'a Path,
        project_key: &'a str,
        launch_directory: &'a Path,
        inventory: &'a Inventory,
        outcomes: &'a Outcomes<'a>,
        recursive: bool,
        status: Status,
    ) -> Self {
        let incomplete = recursive.then(|| inventory.incomplete_discovery());
        let mut edges = Vec::new();
        let sessions = inventory
            .order
            .iter()
            .map(|&session_id| {
                let discovery = match &incomplete {
                    None => Discovery::NotRequested,
                    Some(incomplete) if incomplete.contains(&session_id) => Discovery::Incomplete,
                    Some(_) => Discovery::Complete,
                };
                match &inventory.sources[&session_id] {
                    Ok(snapshot) => {
                        edges.extend(&snapshot.edges);
                        let source = &snapshot.capture;
                        Session {
                            session_id,
                            representation: Some(source.representation),
                            source_paths: Some(SourcePaths {
                                plain: source.plain_path.as_deref().map(ManifestPath::from),
                                gzip: source.gzip_path.as_deref().map(ManifestPath::from),
                                sidecar: source.sidecar_path.as_deref().map(ManifestPath::from),
                            }),
                            availability: Availability::Available,
                            boundary: Some(&snapshot.boundary),
                            discovery,
                            issues: &snapshot.issues,
                        }
                    }
                    Err(error) => Session {
                        session_id,
                        representation: None,
                        source_paths: None,
                        availability: match error {
                            SnapshotError::Missing => Availability::Missing,
                            SnapshotError::Unstable => Availability::Unstable,
                            SnapshotError::Corrupt(_) => Availability::Corrupt,
                            SnapshotError::Io(_) => Availability::IoError,
                            SnapshotError::Interrupted => Availability::Interrupted,
                        },
                        boundary: None,
                        discovery,
                        issues: &[],
                    },
                }
            })
            .collect();
        Self {
            version: 1,
            source_home: source_home.into(),
            project_key,
            launch_directory: launch_directory.into(),
            selected_ids: &inventory.roots,
            sessions,
            bundles: &outcomes.bundles,
            outputs: &outcomes.outputs,
            edges,
            status,
        }
    }

    pub fn publish(
        &self,
        publisher: &mut Publisher<'_>,
        destination: &Destination,
        scratch: &Path,
        interrupted: &impl Fn() -> bool,
    ) -> Result<String, PublishError> {
        let mut spool = NamedTempFile::new_in(scratch)?;
        {
            let mut writer = BufWriter::new(spool.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, self).map_err(io::Error::from)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        spool.as_file_mut().rewind()?;
        // Allow a best-effort report after interruption without waiting
        // indefinitely for a competing exporter to unlock the parent.
        let deadline = Instant::now() + Duration::from_secs(5);
        publisher.standalone(destination, spool.as_file_mut(), &|| {
            interrupted() && Instant::now() >= deadline
        })
    }
}

pub(super) fn manifest_outcome(
    diagnostics: &mut dyn Write,
    path: &Path,
    result: &Result<String, PublishError>,
) -> io::Result<()> {
    let path = quote_path(path);
    match result {
        Ok(_) => writeln!(diagnostics, "published export manifest {path}"),
        Err(PublishError::Failed(error)) => {
            writeln!(diagnostics, "error: export manifest failed {path}: {error}")
        }
        Err(error @ PublishError::Uncertain(_)) => {
            writeln!(
                diagnostics,
                "error: export manifest uncertain {path}: {error}"
            )
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ManifestPath<'a> {
    Utf8(&'a str),
    UnixBytes { unix_bytes_hex: String },
}

impl<'a> From<&'a Path> for ManifestPath<'a> {
    fn from(path: &'a Path) -> Self {
        match path.to_str() {
            Some(path) => Self::Utf8(path),
            None => Self::UnixBytes {
                unix_bytes_hex: hex_digest(path.as_os_str().as_bytes()),
            },
        }
    }
}

fn serialize_path<S: Serializer>(path: &Option<PathBuf>, serializer: S) -> Result<S::Ok, S::Error> {
    path.as_deref()
        .map(ManifestPath::from)
        .serialize(serializer)
}
