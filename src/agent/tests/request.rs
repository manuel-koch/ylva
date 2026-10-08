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

#[test]
fn request_estimate_updates_footer_with_the_sent_request_window() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(1);
        send_context_usage_estimate(
            &ui_tx,
            46_977,
            65_536,
            None,
            ContextUsageSource::SerializedRequest,
        )
        .await;

        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::ContextUsage {
                estimated_tokens: 46_977,
                context_length: 65_536,
                source: ContextUsageSource::SerializedRequest,
            })
        ));
    });
}

#[test]
fn engine_notices_are_forwarded_before_the_stream_starts() {
    crate::test_support::run_locked(async {
        let engine: Arc<dyn Engine> = Arc::new(NoticeEngine {
                inner: EngineImpl::new(
                    Arc::new(MockProvider::new(
                        vec!["hi".to_string()],
                        Duration::from_millis(0),
                    )),
                    EngineConfig {
                        provider_connection: "mock".to_string(),
                        model: "test-model".to_string(),
                        temperature: None,
                        context_length: None,
                        api_base_url: None,
                        api_key: None,
                        reasoning: None,

                        compact_target_ratio: 0.5,
                        soft_compaction_threshold: 0.7,
                        hard_compaction_threshold: 0.9,
},
                ),
                pending: std::sync::Mutex::new(vec![
                    "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default.".to_string(),
                ]),
            });
        let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("hello".to_string()))
            .await;

        let mut updates = Vec::new();
        while !updates
            .iter()
            .any(|update| matches!(update, UiUpdate::StreamStarted))
        {
            updates.push(
                tokio::time::timeout(Duration::from_millis(500), ui_rx.recv())
                    .await
                    .expect("the stream starts after initial updates")
                    .expect("the UI channel stays open"),
            );
        }
        assert!(
            matches!(updates.first(), Some(UiUpdate::Notice { run_id: None, .. }))
                && matches!(updates.first(), Some(UiUpdate::Notice { text, .. })
                        if text.contains("does not advertise reasoning effort")),
            "the engine notice must be forwarded before the stream: {updates:?}"
        );

        drop(agent);
        handle
            .await
            .expect("agent task should not panic")
            .expect("agent should stop cleanly after the channel closes");
    });
}

#[test]
fn accumulator_assembles_single_fragment_call() {
    let mut acc = ToolCallAccumulator {
        id: Uuid::new_v4(),
        name: None,
        arguments: String::new(),
    };
    acc.name = Some("read".to_string());
    acc.arguments.push_str(r#"{"path":"a.txt"}"#);
    let call = acc.into_tool_call_checked().expect("valid arguments");
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments, serde_json::json!({ "path": "a.txt" }));
}

#[test]
fn accumulator_concatenates_fragmented_arguments_across_indices() {
    let mut acc_0 = ToolCallAccumulator {
        id: Uuid::new_v4(),
        name: Some("read".into()),
        arguments: String::new(),
    };
    let mut acc_1 = ToolCallAccumulator {
        id: Uuid::new_v4(),
        name: Some("glob".into()),
        arguments: String::new(),
    };
    acc_0.arguments.push_str(r#"{"path""#);
    acc_1.arguments.push_str(r#"{"pattern""#);
    acc_0.arguments.push_str(r#":"a.txt"}"#);
    acc_1.arguments.push_str(r#":"*.rs"}"#);
    let call_0 = acc_0.into_tool_call_checked().expect("a.txt parses");
    let call_1 = acc_1.into_tool_call_checked().expect("glob parses");
    assert_eq!(call_0.arguments, serde_json::json!({ "path": "a.txt" }));
    assert_eq!(call_1.arguments, serde_json::json!({ "pattern": "*.rs" }));
}

#[test]
fn accumulator_reports_incomplete_json_error_with_fragment() {
    let acc = ToolCallAccumulator {
        id: Uuid::new_v4(),
        name: Some("read".into()),
        arguments: r#"{"path":"unterminated"#.to_string(),
    };
    let err = acc.into_tool_call_checked().expect_err("incomplete JSON");
    assert_eq!(err, r#"{"path":"unterminated"#);
}

#[test]
fn accumulator_empty_arguments_yields_null() {
    let acc = ToolCallAccumulator {
        id: Uuid::new_v4(),
        name: Some("glob".into()),
        arguments: String::new(),
    };
    let call = acc.into_tool_call_checked().expect("empty is valid");
    assert_eq!(call.arguments, Value::Null);
}

#[test]
fn context_rejections_retry_once_before_deltas_with_a_smaller_projection() {
    crate::test_support::run_locked(async {
        for (failures, context_rejection, emit_partial, expected_requests) in [
            (1, true, false, 2),
            (2, true, false, 2),
            (1, false, false, 1),
            (1, true, true, 1),
        ] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let prompt = sized_prompt();
            let mut configuration = test_engine().config();
            configuration.context_length = Some(first_turn_window(&prompt, 0.78));
            let engine = Arc::new(EngineImpl::new(
                Arc::new(RejectingRequestProvider {
                    requests: requests.clone(),
                    failures,
                    context_rejection,
                    emit_partial,
                }),
                configuration,
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let (mut agent, mut updates) = AgentLoop::new(engine, registry);
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt(prompt.clone()))
                .await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(update) = updates.recv().await {
                    if matches!(update, UiUpdate::Error { .. })
                        || matches!(update, UiUpdate::AppendText(ref text) if text == "complete")
                    {
                        break;
                    }
                }
            })
            .await
            .expect("request terminates");
            {
                let recorded = requests.lock().expect("requests");
                assert_eq!(recorded.len(), expected_requests);
                if expected_requests == 2 {
                    let estimate = |request: &[Message]| {
                        request
                            .iter()
                            .map(|message| {
                                estimate_message_tokens(
                                    crate::llm::TokenEstimator::Tiktoken,
                                    message,
                                )
                            })
                            .sum::<usize>()
                    };
                    assert!(estimate(&recorded[1]) < estimate(&recorded[0]));
                    assert!(recorded[1].iter().any(|message| matches!(message,
                        Message::User { kind: UserMessageKind::Explicit, content }
                            if content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
                    )));
                }
            }
            drop(agent);
            let context = handle.await.expect("joins").expect("context");
            assert!(context.messages().iter().any(|message| matches!(message,
                Message::User { kind: UserMessageKind::Explicit, content } if content == &prompt
            )));
        }
    });
}

#[test]
fn preflight_notice_precedes_dispatch_and_a_discarded_notice_prevents_dispatch() {
    crate::test_support::run_locked(async {
        for (acknowledge, cancel_before_ack) in [(false, false), (true, false), (true, true)] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mut configuration = test_engine().config();
            configuration.context_length = Some(10_000);
            let engine = Arc::new(EngineImpl::new(
                Arc::new(RejectingRequestProvider {
                    requests: requests.clone(),
                    failures: 0,
                    context_rejection: false,
                    emit_partial: false,
                }),
                configuration,
            ));
            let mut context = Context::new();
            context.push_user("task ".repeat(10_000));
            let (sender, mut events) = mpsc::channel(4);
            let (updates, mut updates_receiver) = mpsc::channel(10);
            let cancel = start_request(
                &mut context,
                None,
                engine,
                Vec::new(),
                updates,
                sender.clone(),
                7,
                None,
                None,
            );
            drop(sender);
            let (generation, event) = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("notice arrives")
                .expect("notice");
            assert_eq!(generation, 7);
            assert!(
                requests.lock().expect("requests").is_empty(),
                "notice is processed before provider contact"
            );
            let AgentEvent::PreflightNotice {
                text,
                acknowledgement,
            } = event
            else {
                panic!("notice first")
            };
            assert!(text.contains("lossily"));
            if cancel_before_ack {
                cancel.cancel();
            }
            if acknowledge {
                acknowledgement
                    .lock()
                    .expect("acknowledgement")
                    .take()
                    .expect("sender")
                    .send(())
                    .expect("ack");
                let completed = tokio::time::timeout(Duration::from_secs(1), events.recv())
                    .await
                    .expect("request completes");
                if cancel_before_ack {
                    assert!(matches!(
                        completed,
                        Some((7, AgentEvent::StreamCancelled { .. }))
                    ));
                    assert!(matches!(
                        updates_receiver.recv().await,
                        Some(UiUpdate::StreamCancelled)
                    ));
                    assert!(requests.lock().expect("requests").is_empty());
                } else {
                    assert!(matches!(
                        completed,
                        Some((7, AgentEvent::StreamDone { .. }))
                    ));
                    assert_eq!(requests.lock().expect("requests").len(), 1);
                }
            } else {
                drop(acknowledgement);
                assert!(tokio::time::timeout(Duration::from_secs(1), events.recv())
                    .await
                    .expect("stale request exits")
                    .is_none());
                assert!(
                    requests.lock().expect("requests").is_empty(),
                    "superseded request never dispatches"
                );
            }
        }
    });
}

#[test]
fn irreducible_request_overhead_fails_visibly_without_contacting_the_provider() {
    crate::test_support::run_locked(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut configuration = test_engine().config();
        configuration.context_length = Some(100);
        let engine = Arc::new(EngineImpl::new(
            Arc::new(RejectingRequestProvider {
                requests: requests.clone(),
                failures: 0,
                context_rejection: false,
                emit_partial: false,
            }),
            configuration,
        ));
        let registry = Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        ));
        let (mut agent, mut updates) = AgentLoop::new(engine, registry);
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("retain this request".to_string()))
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(UiUpdate::Error { message, .. }) = updates.recv().await {
                    break message;
                }
            }
        })
        .await
        .expect("visible failure");
        assert!(message.contains("larger context window"));
        assert!(requests.lock().expect("requests").is_empty());
        drop(agent);
        handle.await.expect("joins").expect("context");
    });
}
