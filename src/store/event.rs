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

//! Persisted event shapes and version/timestamp contracts.

use crate::agent::Message;
use crate::child_agent_manager::FinishResultOrigin;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the whole-file schema, written into the `meta` header. Bumped
/// only for a semantic break that needs an upgrader.
pub const SCHEMA_VERSION: u32 = 1;

/// Version stamped on every individual event line, so an old reader can take
/// an event-type-specific path even without the header.
pub const EVENT_VERSION: u32 = 1;

/// Maximum number of characters kept in a generated session `meta` title.
pub const TITLE_MAX_CHARS: usize = 80;

/// One persisted JSONL line.
///
/// Serialized with `#[serde(tag = "type")]` and **never**
/// `#[serde(deny_unknown_fields)]`, so a reader built against an older or
/// newer schema still loads the file: unknown fields are ignored and unknown
/// `type` values fail only that one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreEvent {
    /// The session's version/metadata header — always the first line.
    Meta(SessionMeta),
    /// One committed conversation message.
    Message {
        #[serde(default, alias = "v")]
        version: u32,
        /// The instant this message became part of the transcript (commit
        /// time — for a user message effectively "sent", for an assistant
        /// message "response fully assembled", since `Context::commit` is
        /// the single point every message passes through). Empty for lines
        /// written before this field existed; `#[serde(default)]` reads
        /// those back as `""` rather than failing.
        #[serde(default)]
        timestamp: String,
        message: Message,
    },
    /// A child-agent run is about to start.
    ChildStart {
        #[serde(default, alias = "v")]
        version: u32,
        created_at: String,
        id: Uuid,
        parent_session: Uuid,
        #[serde(default)]
        call_id: Uuid,
        agent_type: String,
        task: String,
        #[serde(default)]
        label: String,
    },
    /// A child-agent run finished.
    ChildResult {
        #[serde(default, alias = "v")]
        version: u32,
        completed_at: String,
        id: Uuid,
        parent_session: Uuid,
        #[serde(default)]
        call_id: Uuid,
        status: String,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default)]
        origin: FinishResultOrigin,
    },
    /// The provider-connection/model actually serving this session changed — written
    /// once on **every process start** (a fresh session or a resume alike,
    /// since resuming under a different `--profile`/config can resolve to a
    /// different provider or model than last time) and again after every
    /// successful `/models` switch mid-run. `meta.provider_connection`/
    /// `meta.model` only
    /// capture the pair resolved at creation; this event is the
    /// authoritative, replayable history of every value that has served the
    /// session since, including that first one.
    ModelChanged {
        #[serde(default, alias = "v")]
        version: u32,
        changed_at: String,
        provider_connection: String,
        model: String,
    },
    /// The root agent type serving this session changed — written when an
    /// explicit `--agent <type>` override on a resumed session differs from
    /// the type the session was already using. `meta.agent_type` only
    /// captures the type resolved at creation, so — exactly like
    /// [`StoreEvent::ModelChanged`] for the model — this event is the
    /// authoritative, replayable history of every later selection, and the
    /// last one wins when a later run resolves the session's type.
    AgentTypeChanged {
        #[serde(default, alias = "v")]
        version: u32,
        changed_at: String,
        agent_type: String,
    },
    /// The model-facing context was compacted through `boundary`: transcript
    /// messages `[..boundary]` are represented from here on by `summary`,
    /// while every one of them stays in this append-only file.
    ///
    /// Appended, never rewritten: a later event with a larger boundary
    /// supersedes an earlier one, so replay keeps the latest snapshot and the
    /// verbatim tail after its boundary.
    Compaction {
        #[serde(default, alias = "v")]
        version: u32,
        compacted_at: String,
        /// First transcript index kept verbatim as the tail, counted in the
        /// live model context (the rebuilt leading system prompts included).
        boundary: usize,
        /// The same cut expressed in the **persisted transcript**: the leading
        /// system prompts, which are rebuilt on every run and never written to
        /// this file, are excluded. The TUI positions its compaction marker
        /// with this value because it replays the file without building a
        /// model context. `None` for events written before the marker
        /// existed — the TUI then shows no marker for that snapshot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_boundary: Option<usize>,
        summary: String,
    },
    ModelProjection {
        projection: Box<crate::agent::compaction::ModelProjectionSnapshot>,
    },
}

/// The `meta` header record written as the first line of every session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, alias = "v")]
    pub version: u32,
    pub created_at: String,
    pub schema_version: u32,
    pub app_version: String,
    /// `project_key` of the launch directory — see [`super::project_key`].
    pub project_key: String,
    /// The provider-connection/model resolved for this session at creation time.
    /// Mirrors the very first [`StoreEvent::ModelChanged`] record; kept here
    /// too so a session's initial backend is readable from the header alone,
    /// without scanning the transcript. Any later change (a `/models`
    /// switch, or a different resolution on a fresh process start) is
    /// tracked only by `model_changed` — these two fields are never rewritten
    /// after creation.
    /// Named provider connection.
    pub provider_connection: String,
    pub model: String,
    pub session_id: Uuid,
    /// Utility-engine-generated one-line title, truncated to
    /// [`TITLE_MAX_CHARS`]. Empty while title generation is pending or when
    /// the session has no completed turn.
    #[serde(default)]
    pub title: String,
    /// Set only for a child-agent's own session file, naming the parent's
    /// `session_id` (mirrors the parent-side `ChildStart`/`ChildResult`
    /// events so the link is discoverable from either file). `None` for
    /// every top-level session and for `/clear` rollovers, which start a
    /// fresh session but are not a child run — omitted from the JSON line
    /// entirely rather than serialized as `null`, so existing top-level
    /// sessions' header shape is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<Uuid>,
    /// The root agent type resolved for this session at creation time (or
    /// when `/clear` started its replacement session). `None` for sessions
    /// written before this field existed — those read back as the `default`
    /// agent type. Later selections are tracked only by
    /// [`StoreEvent::AgentTypeChanged`]; this field is never rewritten after
    /// creation, and it is omitted from the JSON line while unset so existing
    /// headers keep their shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The session this one was created from by `--clone`. `None` for every
    /// session that was not cloned — omitted from the JSON line rather than
    /// serialized as `null`, so existing headers keep their shape. The
    /// source file is never rewritten, so this link is one-way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<Uuid>,
}

/// Current UTC timestamp in RFC 3339 (`…Z`) shape, millisecond precision —
/// the one timestamp format written to the store, so `created_at`/
/// `changed_at`/`completed_at`/`timestamp` values sort lexicographically.
/// Millisecond (not second) precision so back-to-back events — e.g. a fast
/// child's `child_start`/`child_result` pair, or several tool-call turns in
/// one second — don't collide on the same value.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Joined child lifecycle metadata reconstructed from a parent session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildRunRecord {
    pub run_id: Uuid,
    pub call_id: Uuid,
    pub parent_session: Uuid,
    pub agent_type: String,
    pub task: String,
    pub label: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub status: Option<String>,
    pub result: Option<String>,
    pub sources: Vec<String>,
    pub origin: FinishResultOrigin,
    pub duration: Option<std::time::Duration>,
}
