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
use crate::subagent_manager::FinishResultOrigin;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the whole-file schema, written into the `meta` header. Bumped
/// only for an incompatible semantic change.
pub const SCHEMA_VERSION: u32 = 2;

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
    /// A subagent run is about to start.
    SubagentStart {
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
    /// A subagent run finished.
    SubagentResult {
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
        /// The effective parameters the model runs with from here on. Empty
        /// for events written before they were recorded.
        #[serde(default, flatten)]
        parameters: ModelParameters,
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
        /// Estimated model-view tokens just before this compaction. Absent
        /// for events written before the effect was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens_before: Option<usize>,
        /// Estimated model-view tokens just after this compaction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens_after: Option<usize>,
    },
    ModelProjection {
        projection: Box<crate::agent::compaction::ModelProjectionSnapshot>,
    },
}

/// The effective, non-secret engine parameters recorded with every
/// [`StoreEvent::ModelChanged`], so a finished conversation's `/status` can be
/// derived from its transcript alone. Every field is optional because older
/// events lack them and a parameter may simply be unset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelParameters {
    /// The profile's `[engines.*]` key, when the engine has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    /// The effective context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<usize>,
}

impl ModelParameters {
    /// The parameters `engine` currently runs with.
    pub fn from_engine(engine: &dyn crate::llm::Engine) -> Self {
        let display = crate::llm::EngineDisplayInfo::from_engine(engine);
        let config = engine.config();
        Self {
            engine_name: Some(display.engine_name).filter(|name| !name.is_empty()),
            reasoning_effort: display.reasoning_effort,
            temperature: config
                .temperature
                .map(|temperature| temperature.to_string()),
            max_output_tokens: config.max_output_tokens,
            context_length: Some(engine.effective_context_length()),
        }
    }
}

/// Estimated model-view size around one compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionEffect {
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
}

impl CompactionEffect {
    /// Share of the tokens the compaction removed, in percent. Negative when
    /// the summary made the view larger.
    pub fn reduction_percent(self) -> f64 {
        if self.estimated_tokens_before == 0 {
            return 0.0;
        }
        (self.estimated_tokens_before as f64 - self.estimated_tokens_after as f64) * 100.0
            / self.estimated_tokens_before as f64
    }
}

/// One applied compaction as the [`StoreEvent::Compaction`] event persists it.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionRecord {
    /// First kept index in live model-context coordinates.
    pub boundary: usize,
    /// The same cut in persisted-transcript coordinates.
    pub transcript_boundary: Option<usize>,
    pub summary: String,
    pub effect: Option<CompactionEffect>,
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
    /// [`TITLE_MAX_CHARS`]. Root titles use a completed text reply; subagents
    /// can use their completed structured `agent_finish` result or error.
    /// Empty while no eligible outcome exists or generation has not succeeded.
    #[serde(default)]
    pub title: String,
    /// Set only for a subagent's own session file, naming the parent's
    /// `session_id` (mirrors the parent-side `SubagentStart`/`SubagentResult`
    /// events so the link is discoverable from either file). `None` for
    /// every top-level session and for `/clear` rollovers, which start a
    /// fresh session but are not a subagent run — omitted from the JSON line
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
    /// Set only for a subagent's own session file: the names of the tools it
    /// was registered with, so `/status` of the finished run can list them
    /// (and ignore calls to unknown tools). Omitted while unset, so existing
    /// headers keep their shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
}

/// Current UTC timestamp in RFC 3339 (`…Z`) shape, millisecond precision —
/// the one timestamp format written to the store, so `created_at`/
/// `changed_at`/`completed_at`/`timestamp` values sort lexicographically.
/// Millisecond (not second) precision so back-to-back events — e.g. a fast
/// subagent's `subagent_start`/`subagent_result` pair, or several tool-call turns in
/// one second — don't collide on the same value.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Joined subagent lifecycle metadata reconstructed from a parent session.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentRunRecord {
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
    /// Tool-call events of the run's own session transcript, recovered by
    /// [`crate::store::Store::read_subagent_runs`]. `None` when the transcript
    /// could not be read or nothing was recorded.
    pub tool_call_count: Option<usize>,
}
