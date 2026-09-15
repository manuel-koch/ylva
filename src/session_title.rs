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

//! Best-effort, out-of-band session-title generation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{Message, StreamEvent, UserMessageKind};
use crate::agent_factory::AgentFactory;
use crate::config::ProfileConfig;
use crate::llm::engine::Engine;
use crate::store::{Store, TITLE_MAX_CHARS};
use tracing::Instrument;

/// A title that was persisted and is ready for a TUI to display.
pub(crate) struct SessionTitleUpdate {
    pub(crate) session_id: uuid::Uuid,
    pub(crate) title: String,
}

/// Driver-owned guard that permits one title-generation attempt per session.
#[derive(Clone, Default)]
pub(crate) struct TitleAttempt {
    attempted_sessions: Arc<Mutex<HashSet<uuid::Uuid>>>,
}

impl TitleAttempt {
    pub(crate) fn trigger(
        &self,
        store: Store,
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let prompt = store.title_prompt()?;
        let utility_engine_name = profile.utility_engine_name().to_string();
        let Some(engine) = factory.engine(&utility_engine_name) else {
            tracing::warn!(
                session_id = %store.session_id(),
                stage = "skipped",
                reason = "utility_engine_missing",
                engine = %utility_engine_name,
                "utility engine for session title is not configured"
            );
            return None;
        };
        self.spawn_attempt(store, engine, prompt, Some(utility_engine_name), None)
    }

    pub(crate) fn trigger_with_engine(
        &self,
        store: Store,
        engine: Arc<dyn Engine>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let prompt = store.title_prompt()?;
        self.spawn_attempt(store, engine, prompt, None, None)
    }

    pub(crate) fn trigger_with_title_updates(
        &self,
        store: Store,
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
        title_update_tx: mpsc::UnboundedSender<SessionTitleUpdate>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let prompt = store.title_prompt()?;
        let utility_engine_name = profile.utility_engine_name().to_string();
        let Some(engine) = factory.engine(&utility_engine_name) else {
            tracing::warn!(
                session_id = %store.session_id(),
                stage = "skipped",
                reason = "utility_engine_missing",
                engine = %utility_engine_name,
                "utility engine for session title is not configured"
            );
            return None;
        };
        self.spawn_attempt(
            store,
            engine,
            prompt,
            Some(utility_engine_name),
            Some(title_update_tx),
        )
    }

    fn spawn_attempt(
        &self,
        store: Store,
        engine: Arc<dyn Engine>,
        prompt: String,
        utility_engine_name: Option<String>,
        title_update_tx: Option<mpsc::UnboundedSender<SessionTitleUpdate>>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let session_id = store.session_id();
        {
            let mut attempted_sessions = self
                .attempted_sessions
                .lock()
                .expect("title attempt mutex is not poisoned");
            if !attempted_sessions.insert(session_id) {
                tracing::debug!(
                    session_id = %session_id,
                    stage = "skipped",
                    reason = "attempt_already_started",
                    "session title generation skipped"
                );
                return None;
            }
        }
        let engine_config = engine.config();
        let utility_engine_name = utility_engine_name.unwrap_or_else(|| "<direct>".to_string());
        let prompt_chars = prompt.chars().count();
        tracing::info!(
            session_id = %session_id,
            stage = "scheduled",
            utility_engine = %utility_engine_name,
            provider_connection = %engine_config.provider_connection,
            model = %engine_config.model,
            prompt_chars,
            "session title generation scheduled"
        );
        let span = tracing::info_span!(
            "session_title",
            session_id = %session_id,
            utility_engine = %utility_engine_name,
            provider_connection = %engine_config.provider_connection,
            model = %engine_config.model,
        );
        Some(tokio::spawn(
            async move {
                tracing::info!(
                    stage = "request_started",
                    prompt_chars,
                    "session title utility request started"
                );
                match generate_title(engine, prompt).await {
                    Ok(title) => {
                        let title_chars = title.chars().count();
                        tracing::info!(
                            stage = "persist_started",
                            title_chars,
                            "persisting generated session title"
                        );
                        if let Err(error) = store.set_title(title).await {
                            tracing::warn!(
                                stage = "persist_failed",
                                %error,
                                "failed to persist generated session title"
                            );
                        } else {
                            tracing::info!(
                                stage = "persisted",
                                title_chars,
                                "session title persisted"
                            );
                            if let Some(title_update_tx) = title_update_tx {
                                let title = store.title();
                                if title_update_tx
                                    .send(SessionTitleUpdate { session_id, title })
                                    .is_err()
                                {
                                    tracing::debug!(
                                        stage = "update_dropped",
                                        "persisted session title has no active TUI receiver"
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            stage = "generation_failed",
                            %error,
                            "session title generation failed"
                        );
                    }
                }
            }
            .instrument(span),
        ))
    }
}

async fn generate_title(engine: Arc<dyn Engine>, prompt: String) -> anyhow::Result<String> {
    let mut stream = match engine
        .chat(
            vec![Message::User {
                content: prompt,
                kind: UserMessageKind::Explicit,
            }],
            Vec::new(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(
                stage = "request_failed",
                %error,
                "session title utility request failed"
            );
            return Err(error);
        }
    };
    let mut response = String::new();
    let termination = loop {
        match stream.recv().await {
            Some(StreamEvent::Text(text)) => response.push_str(&text),
            Some(StreamEvent::Done) => break "done",
            Some(StreamEvent::Cancelled) => {
                tracing::warn!(
                    stage = "response_cancelled",
                    "session title utility response was cancelled"
                );
                anyhow::bail!("utility title stream was cancelled");
            }
            Some(StreamEvent::Error(error)) => {
                tracing::warn!(
                    stage = "response_failed",
                    %error,
                    "session title utility response failed"
                );
                return Err(error);
            }
            Some(StreamEvent::Thinking(_) | StreamEvent::Delta(_)) => {}
            None => break "channel_closed",
        }
    };
    let response_chars = response.chars().count();
    tracing::info!(
        stage = "response_received",
        response_chars,
        termination,
        "session title utility response received"
    );
    let Some(title) = normalize_title(&response) else {
        tracing::warn!(
            stage = "normalization_failed",
            response_chars,
            "session title response was empty after normalization"
        );
        anyhow::bail!("utility engine returned an empty title");
    };
    tracing::info!(
        stage = "normalized",
        response_chars,
        title_chars = title.chars().count(),
        "session title response normalized"
    );
    Ok(title)
}

/// Normalize a utility response into the one-line metadata title shape.
pub(crate) fn normalize_title(response: &str) -> Option<String> {
    let first_non_empty_line = response
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut title = first_non_empty_line.to_string();
    loop {
        let stripped = strip_matching_surrounding_quotes(&title);
        if stripped == title {
            break;
        }
        title = stripped.trim().to_string();
    }
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(TITLE_MAX_CHARS).collect())
}

fn strip_matching_surrounding_quotes(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() < 2 {
        return value.to_string();
    }
    let matching_quote = matches!(
        (characters[0], characters[characters.len() - 1]),
        ('"', '"') | ('\'', '\'') | ('`', '`') | ('“', '”') | ('‘', '’')
    );
    if matching_quote {
        characters[1..characters.len() - 1].iter().collect()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::time::Duration;
    use tokio::sync::mpsc;

    use crate::agent::AssistantStatus;
    use crate::llm::engine::{EngineConfig, EngineImpl};
    use crate::llm::MockProvider;
    use crate::llm::{ModelInfo, ToolSchema};

    type RecordedRequest = (Vec<Message>, Vec<ToolSchema>);

    struct RecordingEngine {
        request: Arc<Mutex<Option<RecordedRequest>>>,
        response: String,
    }

    impl Engine for RecordingEngine {
        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            tools: Vec<ToolSchema>,
            cancel: CancellationToken,
        ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            *self.request.lock().expect("request mutex is not poisoned") = Some((messages, tools));
            let response = self.response.clone();
            Box::pin(async move {
                assert!(!cancel.is_cancelled());
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(StreamEvent::Text(response))
                    .await
                    .expect("receiver is alive");
                sender
                    .send(StreamEvent::Done)
                    .await
                    .expect("receiver is alive");
                Ok(receiver)
            })
        }

        fn config(&self) -> EngineConfig {
            EngineConfig {
                provider_connection: "recording".to_string(),
                model: "title".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
            }
        }

        fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn switch_model(&self, _model: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn normalize_title_uses_first_non_empty_line_and_strips_quotes() {
        assert_eq!(
            normalize_title("\n  \"A useful title\"\nExplanation"),
            Some("A useful title".to_string())
        );
        assert_eq!(
            normalize_title("“Nested title”\n"),
            Some("Nested title".to_string())
        );
    }

    #[test]
    fn normalize_title_bounds_unicode_by_characters() {
        let response = "é".repeat(TITLE_MAX_CHARS + 5);
        assert_eq!(
            normalize_title(&response)
                .expect("non-empty response")
                .chars()
                .count(),
            TITLE_MAX_CHARS
        );
    }

    #[test]
    fn normalize_title_rejects_only_empty_output() {
        assert_eq!(normalize_title(" \n\t "), None);
        assert_eq!(normalize_title("\"\""), None);
    }

    #[tokio::test]
    async fn utility_generation_forwards_one_message_and_no_tools() {
        let request = Arc::new(Mutex::new(None));
        let engine: Arc<dyn Engine> = Arc::new(RecordingEngine {
            request: Arc::clone(&request),
            response: "A title".to_string(),
        });
        let prompt = "<user_message>\nquestion\n</user_message>".to_string();

        assert_eq!(
            generate_title(engine, prompt.clone())
                .await
                .expect("title generated"),
            "A title"
        );
        let (messages, tools) = request
            .lock()
            .expect("request mutex is not poisoned")
            .take()
            .expect("utility request was recorded");
        assert!(tools.is_empty());
        assert_eq!(
            messages,
            vec![Message::User {
                content: prompt,
                kind: UserMessageKind::Explicit,
            }]
        );
    }

    #[tokio::test]
    async fn empty_utility_generation_is_an_error_without_a_fallback_title() {
        let engine: Arc<dyn Engine> = Arc::new(RecordingEngine {
            request: Arc::new(Mutex::new(None)),
            response: "\n ".to_string(),
        });

        assert!(generate_title(engine, "prompt".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn utility_title_generation_is_detached_and_guarded_per_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = Store::create(
            &chats_root,
            temp.path(),
            uuid::Uuid::new_v4(),
            "mock",
            "title-model",
        )
        .expect("store created");
        store.record_message(&Message::User {
            content: "Explain the sky".to_string(),
            kind: UserMessageKind::Explicit,
        });
        store.record_message(&Message::Assistant {
            content: "Light scatters in the atmosphere.".to_string(),
            status: AssistantStatus::Complete,
            tool_calls: Vec::new(),
        });
        store.flush().await;

        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(
                vec!["\"Atmospheric Scattering\"\nExplanation".to_string()],
                Duration::ZERO,
            )),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "title-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
            },
        ));
        let attempt = TitleAttempt::default();
        attempt.trigger_with_engine(store.clone(), engine.clone());
        attempt.trigger_with_engine(store.clone(), engine);

        let title = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let summaries =
                    Store::list_sessions(&chats_root, store.project_key()).expect("list succeeds");
                if let Some(summary) = summaries
                    .first()
                    .filter(|summary| !summary.title.is_empty())
                {
                    break summary.title.clone();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("utility title completes");
        assert_eq!(title, "Atmospheric Scattering");
    }
}
