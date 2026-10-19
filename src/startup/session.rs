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

//! Session-related startup steps: root agent type, store opening and the
//! resume hint.

use crate::agent_factory::{select_root_agent_type, RootAgentTypeSelection};
use crate::ProfileConfig;

/// Resolve the root agent type for this run: `--agent` first, else the type
/// stored on the resumed session, else `default`. Every candidate is checked
/// against the loaded profile, so a typo'd flag and a stale stored name are
/// both startup errors instead of a silent fallback to `default`.
pub(super) fn resolve_root_agent_type(
    explicit_agent_type: Option<&str>,
    conversation_id: Option<uuid::Uuid>,
    profile: &ProfileConfig,
) -> Result<RootAgentTypeSelection, String> {
    let stored_agent_type = match conversation_id {
        Some(conversation_id) => read_stored_agent_type(conversation_id)?,
        None => None,
    };
    select_root_agent_type(
        explicit_agent_type,
        stored_agent_type.as_deref(),
        conversation_id.is_some(),
        profile,
    )
}

/// The root agent type a session last stored — the last
/// `agent_type_changed` record wins over the `meta` header, and `None` means
/// the session predates both (callers fall back to `default`).
///
/// A session id with no file is `None` here rather than an error:
/// [`crate::Store::resume`] reports the missing session with its own
/// dedicated message later.
fn read_stored_agent_type(session_id: uuid::Uuid) -> Result<Option<String>, String> {
    let chats_root = crate::default_chats_root()
        .map_err(|error| format!("cannot resolve Ylva home: {error}"))?;
    let cwd = std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
    let path = crate::session_path(&chats_root, &crate::project_key(&cwd), session_id);
    if crate::Store::read_session_meta(&path)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(None);
    }
    crate::Store::read_latest_agent_type(&path).map_err(|error| error.to_string())
}

/// Open the session file this run appends to.
///
/// With `--conversation-id <uuid>`, the existing session is re-attached for
/// append; an unknown id is a hard error, never a silent fresh-session
/// fallback. With `--clone` as well, that same id is the copy *source*: the
/// run gets a brand-new session replaying the source's event stream, and the
/// source file is left untouched. Without the flag, a brand-new session is
/// started — Ylva never auto-resumes and never shows a startup picker.
///
/// Fails when the Ylva home cannot be resolved or the session file cannot be
/// created, resumed, or cloned. Persistence is mandatory for Ylva
/// sessions; there is no transient, non-persisted mode.
pub(super) fn open_store(
    conversation_id: Option<uuid::Uuid>,
    clone_session: bool,
    initial_engine_config: Option<&crate::EngineConfig>,
    agent_type: &str,
) -> Result<crate::Store, String> {
    let chats_root = crate::default_chats_root()
        .map_err(|error| format!("cannot resolve Ylva home: {error}"))?;
    let cwd = std::env::current_dir().map_err(|error| format!("cannot resolve cwd: {error}"))?;
    let store = match (conversation_id, clone_session) {
        (Some(source_id), true) => {
            crate::Store::clone_session(&chats_root, &cwd, source_id, Some(agent_type))
                .map_err(|error| error.to_string())?
        }
        (Some(conversation_id), false) => crate::Store::resume(&chats_root, &cwd, conversation_id)
            .map_err(|error| error.to_string())?,
        (None, true) => {
            // Clap rejects `--clone` without `--conversation-id`; keep the
            // invariant visible here rather than silently starting fresh.
            return Err("--clone requires --conversation-id <uuid>".to_string());
        }
        (None, false) => {
            let (provider_connection, model) = initial_engine_config
                .map(|config| (config.provider_connection.as_str(), config.model.as_str()))
                .unwrap_or(("", ""));
            crate::Store::create(
                &chats_root,
                &cwd,
                uuid::Uuid::new_v4(),
                provider_connection,
                model,
                Some(agent_type),
            )
            .map_err(|error| error.to_string())?
        }
    };
    tracing::info!(
        session_id = %store.session_id(),
        path = %store.path().display(),
        resumed = conversation_id.is_some() && !clone_session,
        cloned_from = ?conversation_id.filter(|_| clone_session),
        "session store opened"
    );
    Ok(store)
}

/// Print the session id and a copy-pasteable command that resumes it.
pub(super) fn print_resume_hint(
    snapshot: &crate::app::SessionExitSnapshot,
    explicit_profile_name: Option<&str>,
) {
    eprintln!(
        "{}",
        resume_hint_text(snapshot.session_id, &snapshot.title, explicit_profile_name)
    );
}

/// Pure core of [`print_resume_hint`], kept separate so its exact wording is
/// unit-testable without capturing stderr.
///
/// `--profile <name>` is included only when it was given explicitly on this
/// run: the implicit default profile needs no flag to resume with the same
/// configuration, and echoing a name the user never typed would be
/// misleading.
fn resume_hint_text(
    session_id: uuid::Uuid,
    title: &str,
    explicit_profile_name: Option<&str>,
) -> String {
    let mut hint = format!("\nSession: {session_id}");
    if !title.is_empty() {
        hint.push_str(&format!("\nTitle:   {}", crate::app::single_line(title)));
    }
    hint.push_str(&format!(
        "\nResume with: ylva --conversation-id {session_id}"
    ));
    if let Some(profile_name) = explicit_profile_name {
        hint.push_str(&format!(" --profile {profile_name}"));
    }
    hint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_hint_text_without_a_profile_names_only_the_conversation_id() {
        let session_id = uuid::Uuid::nil();
        let hint = resume_hint_text(session_id, "", None);
        assert_eq!(
            hint,
            format!("\nSession: {session_id}\nResume with: ylva --conversation-id {session_id}")
        );
    }

    #[test]
    fn resume_hint_text_with_an_explicit_profile_includes_it() {
        let session_id = uuid::Uuid::nil();
        let hint = resume_hint_text(session_id, "", Some("work"));
        assert_eq!(
            hint,
            format!(
                "\nSession: {session_id}\nResume with: ylva --conversation-id {session_id} --profile work"
            )
        );
    }

    #[test]
    fn resume_hint_includes_a_safe_single_line_title() {
        let session_id = uuid::Uuid::nil();
        assert_eq!(
            resume_hint_text(session_id, "One\nTwo\u{1b}", Some("work")),
            format!("\nSession: {session_id}\nTitle:   One Two \nResume with: ylva --conversation-id {session_id} --profile work")
        );
    }
}
