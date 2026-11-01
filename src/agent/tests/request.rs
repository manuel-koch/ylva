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

/// Queues a hydration notice and exposes dispatch through a bounded channel.
struct HydrationNoticeEngine {
    inner: Arc<dyn Engine>,
    pending: Mutex<Vec<String>>,
    dispatched: mpsc::Sender<()>,
    fail_before_estimate: bool,
}

impl Engine for HydrationNoticeEngine {
    fn config(&self) -> EngineConfig {
        self.inner.config()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.inner.list_models()
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.inner.switch_model(model)
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        self.inner.chat(messages, tools, cancel)
    }

    fn chat_with_session_id_and_estimate<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
        _session_id: Option<Uuid>,
        on_estimate: Box<dyn FnOnce(usize, usize) -> BoxFuture<'static, ()> + Send>,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            self.pending.lock().expect("pending notices").push(
                "EdenAI model upstream/test-model reasoning effort \"medium\" is unverified."
                    .to_string(),
            );
            if !self.fail_before_estimate {
                on_estimate(123, 64_000).await;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled before dispatch");
                }
                self.dispatched.send(()).await.expect("dispatch observer");
            }
            anyhow::bail!("HTTP 422: backend rejected reasoning_effort medium");
        })
    }

    fn take_pending_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending.lock().expect("pending notices"))
    }
}

#[test]
fn hydration_notice_is_acknowledged_before_estimate_and_failed_network_dispatch() {
    crate::test_support::run_locked(async {
        let (dispatched, mut dispatches) = mpsc::channel(1);
        let engine = Arc::new(HydrationNoticeEngine {
            inner: test_engine(),
            pending: Mutex::new(Vec::new()),
            dispatched,
            fail_before_estimate: false,
        });
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let (sender, mut events) = mpsc::channel(4);
        let (updates, mut usage_updates) = mpsc::channel(4);
        start_request(
            &mut context,
            None,
            engine,
            Vec::new(),
            updates,
            sender,
            7,
            None,
            None,
        );

        let (generation, event) = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("preflight event arrives")
            .expect("event");
        assert_eq!(generation, 7);
        let AgentEvent::PreflightNotice {
            text,
            acknowledgement,
        } = event
        else {
            panic!("hydration notice must precede failure, got {event:?}")
        };
        assert!(text.contains("unverified"));
        assert!(
            dispatches.try_recv().is_err(),
            "no dispatch before acknowledgment"
        );
        assert!(
            usage_updates.try_recv().is_err(),
            "no estimate before acknowledgment"
        );
        acknowledgement
            .lock()
            .expect("acknowledgment")
            .take()
            .expect("sender")
            .send(())
            .expect("acknowledge notice");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), usage_updates.recv())
                .await
                .expect("estimate arrives"),
            Some(UiUpdate::ContextUsage {
                estimated_tokens: 123,
                context_length: 64_000,
                source: ContextUsageSource::SerializedRequest,
            })
        ));
        tokio::time::timeout(Duration::from_secs(2), dispatches.recv())
            .await
            .expect("dispatch follows acknowledgment")
            .expect("dispatch");
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await.expect("failure arrives"), Some((7, AgentEvent::StreamError(message)))
                if message.contains("HTTP 422") && message.contains("backend rejected"))
        );
    });
}

#[test]
fn hydration_notice_is_acknowledged_before_failure_without_an_estimate() {
    crate::test_support::run_locked(async {
        let (dispatched, mut dispatches) = mpsc::channel(1);
        let engine = Arc::new(HydrationNoticeEngine {
            inner: test_engine(),
            pending: Mutex::new(Vec::new()),
            dispatched,
            fail_before_estimate: true,
        });
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let (sender, mut events) = mpsc::channel(4);
        let (updates, mut usage_updates) = mpsc::channel(4);
        start_request(
            &mut context,
            None,
            engine,
            Vec::new(),
            updates,
            sender,
            9,
            None,
            None,
        );

        let (generation, event) = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("notice arrives")
            .expect("event");
        assert_eq!(generation, 9);
        let AgentEvent::PreflightNotice {
            text,
            acknowledgement,
        } = event
        else {
            panic!("queued notice must precede validation failure, got {event:?}")
        };
        assert!(text.contains("unverified"));
        assert!(
            events.try_recv().is_err(),
            "error waits for notice acknowledgment"
        );
        assert!(dispatches.try_recv().is_err(), "no network dispatch");
        assert!(usage_updates.try_recv().is_err(), "no serialized estimate");
        acknowledgement
            .lock()
            .expect("acknowledgment")
            .take()
            .expect("sender")
            .send(())
            .expect("acknowledge notice");
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await.expect("failure arrives"), Some((9, AgentEvent::StreamError(message)))
                if message.contains("HTTP 422") && message.contains("backend rejected"))
        );
    });
}

#[test]
fn discarded_hydration_notice_cancels_dispatch_before_the_estimate() {
    crate::test_support::run_locked(async {
        let (dispatched, mut dispatches) = mpsc::channel(1);
        let engine = Arc::new(HydrationNoticeEngine {
            inner: test_engine(),
            pending: Mutex::new(Vec::new()),
            dispatched,
            fail_before_estimate: false,
        });
        let mut context = Context::new();
        context.push_user("hello".to_string());
        let (sender, mut events) = mpsc::channel(4);
        let (updates, mut usage_updates) = mpsc::channel(4);
        let cancel = start_request(
            &mut context,
            None,
            engine,
            Vec::new(),
            updates,
            sender,
            11,
            None,
            None,
        );
        let (_, event) = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("notice arrives")
            .expect("event");
        let AgentEvent::PreflightNotice {
            acknowledgement, ..
        } = event
        else {
            panic!("notice must precede dispatch, got {event:?}")
        };
        drop(acknowledgement);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .expect("discarded request terminates"),
            Some((11, AgentEvent::StreamCancelled { .. }))
        ));
        assert!(cancel.is_cancelled());
        assert!(
            dispatches.try_recv().is_err(),
            "stale request never dispatches"
        );
        assert!(matches!(
            usage_updates.try_recv(),
            Ok(UiUpdate::StreamCancelled)
        ));
        assert!(
            usage_updates.try_recv().is_err(),
            "no serialized estimate for stale request"
        );
    });
}

#[test]
fn failed_request_publishes_hydration_notice_to_the_ui_before_the_error() {
    crate::test_support::run_locked(async {
        let (dispatched, _dispatches) = mpsc::channel(1);
        let engine = Arc::new(HydrationNoticeEngine {
            inner: test_engine(),
            pending: Mutex::new(Vec::new()),
            dispatched,
            fail_before_estimate: false,
        });
        let (mut agent, mut updates) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("hello".to_string()))
            .await;
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            let mut observed = Vec::new();
            while let Some(update) = updates.recv().await {
                let failed = matches!(update, UiUpdate::Error { .. });
                if matches!(update, UiUpdate::Notice { .. } | UiUpdate::Error { .. }) {
                    observed.push(update);
                }
                if failed {
                    break;
                }
            }
            observed
        })
        .await
        .expect("failure reaches the UI");
        assert!(
            matches!(observed.as_slice(), [
            UiUpdate::Notice { text, run_id: None }, UiUpdate::Error { message, .. }
        ] if text.contains("unverified") && message.contains("HTTP 422")),
            "notice then error: {observed:?}"
        );
        drop(agent);
        handle.await.expect("agent joins").expect("agent stops");
    });
}

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
            let update = tokio::time::timeout(Duration::from_millis(500), ui_rx.recv())
                .await
                .expect("the stream starts after initial updates")
                .expect("the UI channel stays open");
            if !matches!(update, UiUpdate::ContextUsage { .. }) {
                updates.push(update);
            }
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
            let registry = Arc::new(
                tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default())
                    .expect("valid sandbox fixture"),
            );
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
        let registry = Arc::new(
            tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default())
                .expect("valid sandbox fixture"),
        );
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

#[test]
fn fresh_loop_reports_baseline_context_usage_before_any_prompt() {
    crate::test_support::run_locked(async {
        let registry = Arc::new(
            tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default())
                .expect("valid sandbox fixture"),
        );
        let expected_tokens = {
            let engine = test_engine_with_context_length(64_000);
            let context = Context::new();
            context.estimated_model_tokens(&registry.schemas(), engine.as_ref())
        };
        let (mut agent_loop, mut ui_rx) =
            AgentLoop::new(test_engine_with_context_length(64_000), registry);
        let _handle = agent_loop.run();

        let baseline = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(UiUpdate::ContextUsage {
                    estimated_tokens,
                    context_length,
                    source,
                }) = ui_rx.recv().await
                {
                    break (estimated_tokens, context_length, source);
                }
            }
        })
        .await
        .expect("baseline estimate arrives without a prompt");

        assert_eq!(
            baseline,
            (expected_tokens, 64_000, ContextUsageSource::ModelView)
        );
        assert!(expected_tokens > 0, "tool schemas alone must be counted");
    });
}
