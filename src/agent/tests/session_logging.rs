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

use super::*;
use tracing::Instrument;

async fn run_turn(agent: &AgentLoop, updates: &mut mpsc::Receiver<UiUpdate>, prompt: &str) {
    agent
        .send_command(AgentCommand::Prompt(prompt.into()))
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(updates.recv().await, Some(UiUpdate::StreamDone)) {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[test]
fn session_spans_roll_over_without_reparenting_inflight_work() {
    for format in [
        crate::logging::LogFormat::Text,
        crate::logging::LogFormat::Json,
    ] {
        let identities = Arc::new(Mutex::new(Vec::new()));
        let captured_identities = identities.clone();
        let logs = crate::test_support::capture_logs_with_format(format, async move {
            let directory = tempfile::tempdir().unwrap();
            // Prime shared Store callsites before refreshing interest and creating the real spans.
            let priming_store = crate::Store::create(
                &directory.path().join("prime"),
                directory.path(),
                Uuid::new_v4(),
                "mock",
                "mock",
                None,
            )
            .unwrap();
            priming_store.record_message(&Message::User {
                content: "Prime callsite registration".into(),
                kind: UserMessageKind::Explicit,
            });
            priming_store.flush().await;
            priming_store
                .set_title("Prime callsite registration".into())
                .await
                .unwrap();
            tracing::callsite::rebuild_interest_cache();
            let store = crate::Store::create(
                directory.path(),
                directory.path(),
                Uuid::new_v4(),
                "mock",
                "mock",
                None,
            )
            .unwrap();
            let original_id = store.session_id();
            let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
                Arc::new(MockProvider::new(
                    vec!["first".into(), "reply".into()],
                    Duration::from_millis(40),
                )),
                test_engine().config(),
            ));
            let (mut agent, mut updates) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
            agent = agent.with_store(store.clone());
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("PRIVATE FIRST PROMPT".into()))
                .await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(updates.recv().await, Some(UiUpdate::StreamStarted)) {}
            })
            .await
            .unwrap();
            let (release_old_work, old_work_released) = tokio::sync::oneshot::channel();
            let old_work = tokio::spawn(
                async move {
                    old_work_released.await.unwrap();
                    tracing::info!(stage = "old_task_finished", "immutable session ownership");
                }
                .instrument(store.span()),
            );
            agent.send_command(AgentCommand::ClearContext).await;
            let replacement = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let replacement = agent.store().unwrap();
                    if replacement.session_id() != original_id {
                        break replacement;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let new_id = replacement.session_id();
            run_turn(&agent, &mut updates, "PRIVATE SECOND PROMPT").await;
            replacement.set_title("PRIVATE TITLE".into()).await.unwrap();
            replacement.flush().await;
            release_old_work.send(()).unwrap();
            old_work.await.unwrap();
            captured_identities
                .lock()
                .unwrap()
                .extend([original_id, new_id]);
            drop(agent);
            handle.await.unwrap().unwrap();
            // Writer tasks are drained within the capturing runtime.
            drop(store);
            drop(replacement);
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        let identities = identities.lock().unwrap();
        assert!(!logs.contains("PRIVATE FIRST PROMPT"));
        assert!(!logs.contains("PRIVATE SECOND PROMPT"));
        assert!(!logs.contains("PRIVATE TITLE"));
        match format {
            crate::logging::LogFormat::Text => {
                let old_line = logs
                    .lines()
                    .find(|line| line.contains("old_task_finished"))
                    .unwrap();
                assert!(old_line.contains(&identities[0].to_string()), "{old_line}");
                assert!(!old_line.contains(&identities[1].to_string()), "{old_line}");
                let cancelled = logs
                    .lines()
                    .find(|line| line.contains("session request cancelled"))
                    .unwrap_or_else(|| panic!("missing cancellation event:\n{logs}"));
                assert!(
                    cancelled.contains(&identities[0].to_string()),
                    "{cancelled}"
                );
                let title_line = logs
                    .lines()
                    .find(|line| {
                        line.contains("rewriting session metadata")
                            && line.contains(&identities[1].to_string())
                    })
                    .unwrap();
                assert!(
                    title_line.contains(&identities[1].to_string()),
                    "{title_line}"
                );
            }
            crate::logging::LogFormat::Json => {
                let events: Vec<serde_json::Value> = logs
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let old = events
                    .iter()
                    .find(|event| event["fields"]["stage"] == "old_task_finished")
                    .unwrap();
                assert_eq!(old["spans"][0]["session_id"], identities[0].to_string());
                let cancelled = events
                    .iter()
                    .find(|event| event["fields"]["message"] == "session request cancelled")
                    .unwrap();
                assert_eq!(
                    cancelled["spans"][0]["session_id"],
                    identities[0].to_string()
                );
                assert_eq!(cancelled["spans"][1]["name"], "agent_stream");
                for identity in identities.iter() {
                    assert!(events.iter().any(|event| event["fields"]["message"]
                        == "agent command received"
                        && event["spans"][0]["session_id"] == identity.to_string()));
                    assert!(events.iter().any(|event| event["fields"]["message"]
                        == "session event persisted"
                        && event["spans"][0]["session_id"] == identity.to_string()));
                }
                let title = events
                    .iter()
                    .find(|event| {
                        event["fields"]["message"]
                            == "rewriting session metadata with generated title"
                            && event["spans"][0]["session_id"] == identities[1].to_string()
                    })
                    .unwrap();
                assert_eq!(title["spans"][0]["session_id"], identities[1].to_string());
            }
        }
    }
}
