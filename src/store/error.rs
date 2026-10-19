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

use std::path::PathBuf;

use uuid::Uuid;

use crate::home_relative_path::display_path;

/// Errors surfaced by the store. Distinct variants let callers tell a lock
/// conflict, a missing session, and a plain I/O failure apart instead of
/// pattern-matching on an opaque error string.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid session representation at {}: {reason}", display_path(path))]
    InvalidRepresentation { path: PathBuf, reason: String },
    #[error("session maintenance and archive restoration require Unix")]
    UnsupportedMaintenance,
    #[error("session '{session_id}' not found at {}", display_path(path))]
    SessionNotFound { session_id: Uuid, path: PathBuf },
    #[error("session '{session_id}' is open in another Ylva process")]
    SessionLocked { session_id: Uuid },
    #[error("cannot resolve the home directory for ~/.ylva")]
    HomeDirectoryNotFound,
    #[error("store I/O error at {}: {source}", display_path(path))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("session '{session_id}' writer is no longer available")]
    WriterClosed { session_id: Uuid },
    #[error(
        "session '{session_id}' is a subagent transcript at {}; only top-level sessions can be \
         cloned",
        display_path(path)
    )]
    SessionIsSubagent { session_id: Uuid, path: PathBuf },
    #[error(
        "session '{session_id}' at {} has no readable metadata header; it cannot be cloned",
        display_path(path)
    )]
    MissingSessionMeta { session_id: Uuid, path: PathBuf },
}
