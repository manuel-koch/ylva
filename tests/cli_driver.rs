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

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use tempfile::TempDir;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

use ylva::{
    cli_driver::run_cli_to, default_registry, AgentCommand, AgentConfig, AgentLoop,
    AutoApproveTool, DeltaChunk, Engine, EngineConfig, EngineImpl, Message, MockEvent, ModelInfo,
    ProfileConfig, Provider, ProviderModelOption, Role, Sandbox, StreamEvent, ToolRegistry,
    ToolRegistryConfig, ToolSchema, UiUpdate,
};

/// A provider that emits `phase_one` on the first `chat()` call and
/// `phase_two` on every later call. Lets a test script a single tool call
/// followed by a text response (avoiding an infinite tool loop).
struct MockProviderWithScript {
    phase_one: Vec<MockEvent>,
    phase_two: Vec<MockEvent>,
    interval: Duration,
    calls: Arc<AtomicUsize>,
}

impl Provider for MockProviderWithScript {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let phase_one = self.phase_one.clone();
        let phase_two = self.phase_two.clone();
        let interval = self.interval;
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            let script = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                phase_one
            } else {
                phase_two
            };
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Thinking(text) => StreamEvent::Thinking(text),
                        MockEvent::Delta(delta) => StreamEvent::Delta(delta),
                    };
                    if tx.send(stream_event).await.is_err() {
                        return;
                    }
                    if index == last_index {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            return;
                        }
                        _ = tokio::time::sleep(interval) => {}
                    }
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ylva::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

fn tool_call(name: &str, args: serde_json::Value) -> MockEvent {
    MockEvent::Delta(DeltaChunk {
        tool_call_index: 0,
        tool_call_id: None,
        function_name: Some(name.to_string()),
        arguments_fragment: Some(args.to_string()),
    })
}

fn write_scratch(dir: &TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("scratch write");
    path.display().to_string()
}

struct SwitchableTestEngine {
    config: std::sync::RwLock<EngineConfig>,
    calls: AtomicUsize,
    child_calls: AtomicUsize,
    spawn_agent_type: String,
    chat_records: Mutex<Vec<ChatRecord>>,
    child_request_barrier: Option<Arc<ChildRequestBarrier>>,
}

struct ChildRequestBarrier {
    first_request_started: Notify,
    release_first_request: Notify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatRecord {
    model: String,
    is_child: bool,
}

impl SwitchableTestEngine {
    fn new(provider_connection: &str, model: &str) -> Arc<Self> {
        Self::new_with_spawn_agent_type(provider_connection, model, "default")
    }

    fn new_with_spawn_agent_type(
        provider_connection: &str,
        model: &str,
        spawn_agent_type: &str,
    ) -> Arc<Self> {
        Self::new_with_options(provider_connection, model, spawn_agent_type, None)
    }

    fn new_with_child_request_barrier(
        provider_connection: &str,
        model: &str,
    ) -> (Arc<Self>, Arc<ChildRequestBarrier>) {
        let barrier = Arc::new(ChildRequestBarrier {
            first_request_started: Notify::new(),
            release_first_request: Notify::new(),
        });
        let engine = Self::new_with_options(
            provider_connection,
            model,
            "default",
            Some(Arc::clone(&barrier)),
        );
        (engine, barrier)
    }

    fn new_with_options(
        provider_connection: &str,
        model: &str,
        spawn_agent_type: &str,
        child_request_barrier: Option<Arc<ChildRequestBarrier>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: std::sync::RwLock::new(EngineConfig {
                provider_connection: provider_connection.to_string(),
                model: model.to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
            }),
            calls: AtomicUsize::new(0),
            child_calls: AtomicUsize::new(0),
            spawn_agent_type: spawn_agent_type.to_string(),
            chat_records: Mutex::new(Vec::new()),
            child_request_barrier,
        })
    }

    async fn wait_for_child_chats(&self, expected_count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let records = self.chat_records();
                let child_chat_count = records.iter().filter(|record| record.is_child).count();
                if child_chat_count >= expected_count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "expected child chat request starts; records={:?}",
                self.chat_records()
            )
        });
    }

    fn chat_records(&self) -> Vec<ChatRecord> {
        self.chat_records
            .lock()
            .expect("test chat records lock")
            .clone()
    }

    fn child_chat_records(&self) -> Vec<ChatRecord> {
        self.chat_records()
            .into_iter()
            .filter(|record| record.is_child)
            .collect()
    }
}

impl Engine for SwitchableTestEngine {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let is_child = tools.iter().any(|tool| tool.name == "agent_finish");
        let model = self.config().model;
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let child_call_index = is_child.then(|| self.child_calls.fetch_add(1, Ordering::SeqCst));
        self.chat_records
            .lock()
            .expect("test chat records lock")
            .push(ChatRecord { model, is_child });
        let spawn_agent_type = self.spawn_agent_type.clone();
        let child_request_barrier = if child_call_index == Some(0) {
            self.child_request_barrier.clone()
        } else {
            None
        };
        let event = if is_child && child_call_index == Some(0) && child_request_barrier.is_some() {
            StreamEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: None,
                function_name: Some("read".to_string()),
                arguments_fragment: Some(
                    serde_json::json!({
                        "path": "Cargo.toml",
                    })
                    .to_string(),
                ),
            })
        } else if is_child {
            StreamEvent::Text("child result".to_string())
        } else if call_index == 0 {
            StreamEvent::Delta(DeltaChunk {
                tool_call_index: 0,
                tool_call_id: None,
                function_name: Some("spawn_subagent".to_string()),
                arguments_fragment: Some(
                    serde_json::json!({
                        "label": "Inspect engine",
                        "task": "inspect the active engine",
                        "agent_type": spawn_agent_type,
                    })
                    .to_string(),
                ),
            })
        } else {
            StreamEvent::Text("root result".to_string())
        };
        Box::pin(async move {
            let (sender, receiver) = mpsc::channel(2);
            sender.send(event).await.expect("test stream receiver open");
            if let Some(barrier) = child_request_barrier {
                barrier.first_request_started.notify_one();
                barrier.release_first_request.notified().await;
            }
            sender
                .send(StreamEvent::Done)
                .await
                .expect("test stream receiver open");
            Ok(receiver)
        })
    }

    fn config(&self) -> EngineConfig {
        self.config.read().expect("test engine config lock").clone()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        let model = self.config().model;
        Box::pin(async move {
            Ok(vec![ModelInfo {
                id: model,
                context_length: None,
                reasoning_efforts: None,
            }])
        })
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.config.write().expect("test engine config lock").model = model.to_string();
        Ok(())
    }
}

#[tokio::test]
async fn run_cli_streams_user_and_assistant_without_history_dump() {
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![
            MockEvent::Text("hello ".into()),
            MockEvent::Text("world".into()),
        ],
        phase_two: Vec::new(),
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(ToolRegistry::new());
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(agent, ui_rx, registry, "greet me".to_string(), &mut output)
        .await
        .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("---[ User ]---"), "output: {output}");
    assert!(output.contains("greet me"), "output: {output}");
    assert!(output.contains("---[ Assistant ]---"), "output: {output}");
    assert!(output.contains("hello world"), "output: {output}");
    assert!(
        !output.contains("Conversation history"),
        "the CLI should not dump the persisted history at shutdown: {output}"
    );
    assert!(
        !output.contains("\"role\":\"user\"") && !output.contains("\"role\":\"assistant\""),
        "the CLI output should not contain a serialized message history: {output}"
    );
}

#[tokio::test]
async fn child_spawn_after_root_model_switch_uses_the_root_active_engine() {
    let default_engine = SwitchableTestEngine::new("default-provider", "default-model");
    let alternate_engine = SwitchableTestEngine::new("alternate-provider", "alternate-model");
    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([
        ("default".to_string(), default_engine as Arc<dyn Engine>),
        ("alternate".to_string(), alternate_engine as Arc<dyn Engine>),
    ]))
    .with_auto_approve(AutoApproveTool::Spawn);
    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        agents: std::collections::HashMap::from([(
            "default".to_string(),
            AgentConfig {
                engine: None,
                description: None,
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                system_prompt: None,
                can_spawn: true,
                tools: std::collections::HashMap::new(),
            },
        )]),
        ..ProfileConfig::default()
    };
    let (mut root, mut ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the root agent");

    root.engine()
        .switch_model_option(&ProviderModelOption {
            provider_connection: "alternate-provider".to_string(),
            model: ModelInfo {
                id: "alternate-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
            },
            reasoning_effort: None,
        })
        .expect("root model switch succeeds");
    assert_eq!(
        root.engine().config().provider_connection,
        "alternate-provider"
    );
    assert_eq!(root.engine().config().model, "alternate-model");

    let handle = root.run();
    root.send_command(AgentCommand::Prompt("delegate the inspection".to_string()))
        .await;
    let configured_engine = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(update) = ui_rx.recv().await {
            match update {
                UiUpdate::ToolPending { id, name, .. } if name == "spawn_subagent" => {
                    root.send_command(AgentCommand::ApproveTool { id }).await;
                }
                UiUpdate::ChildAgentEngineConfigured { engine, .. } => return engine,
                _ => {}
            }
        }
        panic!("root UI channel closed before child engine configuration");
    })
    .await
    .expect("child engine configuration arrives");

    root.send_command(AgentCommand::Stop {
        reason: "end regression test".to_string(),
    })
    .await;
    drop(root);
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("root agent shuts down")
        .expect("root agent task joins")
        .expect("root agent returns a context");

    assert_eq!(
        configured_engine.provider_connection, "alternate-provider",
        "a child resolving the default engine must follow the root's active provider"
    );
    assert_eq!(
        configured_engine.model, "alternate-model",
        "a child resolving the default engine must follow the root's active model"
    );
}

#[tokio::test]
async fn running_child_uses_a_shared_engine_switch_on_its_next_request() {
    let (default_engine, child_request_barrier) =
        SwitchableTestEngine::new_with_child_request_barrier("default-provider", "default-model");
    let alternate_engine = SwitchableTestEngine::new("alternate-provider", "alternate-model");
    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([
        (
            "default".to_string(),
            default_engine.clone() as Arc<dyn Engine>,
        ),
        (
            "alternate".to_string(),
            alternate_engine.clone() as Arc<dyn Engine>,
        ),
    ]))
    .with_auto_approve(AutoApproveTool::Spawn);
    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        agents: std::collections::HashMap::from([(
            "default".to_string(),
            AgentConfig {
                engine: None,
                description: None,
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                system_prompt: None,
                can_spawn: true,
                tools: std::collections::HashMap::new(),
            },
        )]),
        ..ProfileConfig::default()
    };
    let (mut root, mut ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the root agent");

    let handle = root.run();
    root.send_command(AgentCommand::Prompt("delegate the inspection".to_string()))
        .await;
    let (run_id, configured_engine) = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(update) = ui_rx.recv().await {
            if let UiUpdate::ChildAgentEngineConfigured { run_id, engine } = update {
                return (run_id, engine);
            }
        }
        panic!("root UI channel closed before child engine configuration");
    })
    .await
    .expect("child engine configuration arrives");
    assert_eq!(configured_engine.model, "default-model");

    default_engine.wait_for_child_chats(1).await;
    child_request_barrier.first_request_started.notified().await;
    root.engine()
        .switch_model_option(&ProviderModelOption {
            provider_connection: "alternate-provider".to_string(),
            model: ModelInfo {
                id: "alternate-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
            },
            reasoning_effort: None,
        })
        .expect("root model switch succeeds");
    root.send_command(AgentCommand::ForChild {
        run_id,
        command: Box::new(AgentCommand::Prompt(
            "continue after the model switch".to_string(),
        )),
    })
    .await;
    child_request_barrier.release_first_request.notify_one();

    alternate_engine.wait_for_child_chats(1).await;
    assert_eq!(
        alternate_engine.child_chat_records(),
        vec![ChatRecord {
            model: "alternate-model".to_string(),
            is_child: true,
        }]
    );

    root.send_command(AgentCommand::Stop {
        reason: "end shared-engine test".to_string(),
    })
    .await;
    drop(root);
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("root agent shuts down")
        .expect("root agent task joins")
        .expect("root agent returns a context");
}

#[tokio::test]
async fn child_with_explicit_engine_override_keeps_its_named_engine() {
    let default_engine = SwitchableTestEngine::new_with_spawn_agent_type(
        "default-provider",
        "default-model",
        "research",
    );
    let alternate_engine = SwitchableTestEngine::new_with_spawn_agent_type(
        "alternate-provider",
        "alternate-model",
        "research",
    );
    let specialized_engine = SwitchableTestEngine::new("special-provider", "special-model");
    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([
        (
            "default".to_string(),
            default_engine.clone() as Arc<dyn Engine>,
        ),
        (
            "alternate".to_string(),
            alternate_engine.clone() as Arc<dyn Engine>,
        ),
        (
            "specialized".to_string(),
            specialized_engine.clone() as Arc<dyn Engine>,
        ),
    ]))
    .with_auto_approve(AutoApproveTool::Spawn);
    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        agents: std::collections::HashMap::from([
            (
                "default".to_string(),
                AgentConfig {
                    engine: None,
                    description: None,
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    system_prompt: None,
                    can_spawn: true,
                    tools: std::collections::HashMap::new(),
                },
            ),
            (
                "research".to_string(),
                AgentConfig {
                    engine: Some("specialized".to_string()),
                    description: Some("Handles specialized tasks.".to_string()),
                    allowed_tools: None,
                    timeout_secs: None,
                    persona: None,
                    system_prompt: None,
                    can_spawn: false,
                    tools: std::collections::HashMap::new(),
                },
            ),
        ]),
        ..ProfileConfig::default()
    };
    let (mut root, mut ui_rx) = factory
        .create(&profile, "default", Role::Root)
        .expect("factory creates the root agent");

    root.engine()
        .switch_model_option(&ProviderModelOption {
            provider_connection: "alternate-provider".to_string(),
            model: ModelInfo {
                id: "alternate-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
            },
            reasoning_effort: None,
        })
        .expect("root model switch succeeds");

    let handle = root.run();
    root.send_command(AgentCommand::Prompt("delegate the research".to_string()))
        .await;
    let configured_engine = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(update) = ui_rx.recv().await {
            if let UiUpdate::ChildAgentEngineConfigured { engine, .. } = update {
                return engine;
            }
        }
        panic!("root UI channel closed before child engine configuration");
    })
    .await
    .expect("child engine configuration arrives");

    specialized_engine.wait_for_child_chats(1).await;
    assert_eq!(configured_engine.engine_name, "specialized");
    assert_eq!(configured_engine.provider_connection, "special-provider");
    assert_eq!(configured_engine.model, "special-model");
    assert_eq!(
        specialized_engine.chat_records(),
        vec![ChatRecord {
            model: "special-model".to_string(),
            is_child: true,
        }]
    );

    root.send_command(AgentCommand::Stop {
        reason: "end explicit-engine test".to_string(),
    })
    .await;
    drop(root);
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("root agent shuts down")
        .expect("root agent task joins")
        .expect("root agent returns a context");
}

#[tokio::test]
async fn run_cli_renders_thinking_before_response_in_order() {
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![
            MockEvent::Thinking("pondering ".into()),
            MockEvent::Thinking("some more".into()),
            MockEvent::Text("here's ".into()),
            MockEvent::Text("the answer".into()),
        ],
        phase_two: Vec::new(),
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(ToolRegistry::new());
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(agent, ui_rx, registry, "explain".to_string(), &mut output)
        .await
        .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    let thinking_header_at = output
        .find("---[ Assistant Thinking ]---")
        .expect("has thinking header");
    let thinking_text_at = output
        .find("pondering some more")
        .expect("has thinking text");
    let footer_at = output
        .find("[thought for")
        .expect("has thought-duration footer");
    let response_header_at = output
        .find("---[ Assistant ]---")
        .expect("has response header");
    let response_text_at = output.find("here's the answer").expect("has response text");
    assert!(
        thinking_header_at < thinking_text_at
            && thinking_text_at < footer_at
            && footer_at < response_header_at
            && response_header_at < response_text_at,
        "output not in expected order: {output}"
    );
}

#[tokio::test]
async fn run_cli_closes_thinking_section_before_tool_call() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scratch(&dir, "doc.md", "file body");
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![
            MockEvent::Thinking("checking the file".into()),
            tool_call("read", serde_json::json!({ "path": path })),
        ],
        phase_two: vec![MockEvent::Text("ok".into())],
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(ylva::Sandbox::with_root(
        dir.path().to_path_buf(),
    )));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(
        agent,
        ui_rx,
        registry,
        "read the file".to_string(),
        &mut output,
    )
    .await
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    let thinking_at = output.find("checking the file").expect("has thinking text");
    let footer_at = output
        .find("[thought for")
        .expect("has thought-duration footer");
    let tool_at = output.find("[tool] Read").expect("has tool line");
    assert!(
        thinking_at < footer_at && footer_at < tool_at,
        "output not in expected order: {output}"
    );
    assert!(
        output[..tool_at].ends_with("\n\n"),
        "tool output should have a spacer line: {output}"
    );
    // The follow-up round's response ("ok") reuses the plain "Assistant"
    // header, not a separate "Response" label.
    let response_header_at = output
        .rfind("---[ Assistant ]---")
        .expect("has response header for follow-up round");
    let response_text_at = output.find("\nok").expect("has follow-up response text");
    assert!(
        tool_at < response_header_at && response_header_at < response_text_at,
        "output not in expected order: {output}"
    );
}

#[tokio::test]
async fn run_cli_auto_approves_read_and_runs_it() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scratch(&dir, "doc.md", "file body");
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![tool_call("read", serde_json::json!({ "path": path }))],
        phase_two: vec![MockEvent::Text("ok".into())],
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    // Use the tempdir as the workspace root so the file is inside the sandbox.
    // Sandbox::with_root canonicalizes the path so symlink-backed temp dirs
    // (e.g. /var/folders → /private/var/folders on macOS) compare correctly.
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(ylva::Sandbox::with_root(
        dir.path().to_path_buf(),
    )));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(
        agent,
        ui_rx,
        registry,
        "read the file".to_string(),
        &mut output,
    )
    .await
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    // read needs no approval → approved and executed.
    assert!(output.contains("[tool] Read"), "output: {output}");
    assert!(
        output.contains("\n\n[tool] Read"),
        "tool output should have a spacer line: {output}"
    );
    assert!(output.contains("done"), "output: {output}");
}

#[tokio::test]
async fn run_cli_denies_write_and_fed_back_as_error() {
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![tool_call(
            "write",
            serde_json::json!({ "path": "/tmp/out.txt", "content": "x" }),
        )],
        phase_two: vec![MockEvent::Text("ok".into())],
        interval: Duration::from_millis(1),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let registry = Arc::new(default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        ToolRegistryConfig::default(),
    ));
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));
    run_cli_to(
        agent,
        ui_rx,
        registry,
        "write a file".to_string(),
        &mut output,
    )
    .await
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("denied"), "output: {output}");
    // error line is suppressed when the denial was already shown at ToolPending
}

#[tokio::test]
async fn run_cli_waits_for_spawned_child_before_ending_conversation() {
    use tokio_util::sync::CancellationToken;
    use ylva::child_agent_manager::SpawnFn;
    use ylva::{ChildAgentManager, FinishResult};

    // Regression test: `spawn_subagent` resolves the parent's own turn
    // (`StreamDone`) immediately, well before the child actually finishes.
    // `run_cli_to` must not tear down the `AgentLoop` at that first
    // `StreamDone` while a spawned child is still outstanding — otherwise
    // the child's eventual `agent_finish` result is silently discarded.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(MockProviderWithScript {
        phase_one: vec![tool_call(
            "spawn_subagent",
            serde_json::json!({ "label": "Do it", "task": "do it" }),
        )],
        phase_two: vec![MockEvent::Text("delegated, wrapping up".into())],
        interval: Duration::from_millis(1),
        calls: Arc::clone(&calls),
    });
    let registry = Arc::new(default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
            ..ToolRegistryConfig::default()
        },
    ));
    let mut output = Vec::new();
    let engine = Arc::new(EngineImpl::new(provider, common::test_engine_config()));
    let (mut agent, ui_rx) = AgentLoop::new(engine, Arc::clone(&registry));

    let (finish_tx, finish_rx) = mpsc::channel(1);
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::channel(8);
    // Finishes well after the parent's own turn ends, to reproduce the race
    // the fix addresses.
    let spawn_fn: SpawnFn = Box::new(move |request, cancel: CancellationToken, _ui_tx| {
        let finish_tx = finish_tx.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    let _ = finish_tx
                        .send(FinishResult {
                            run_id: request.run_id,
                            call_id: request.call_id,
                            origin: ylva::child_agent_manager::FinishResultOrigin::Agent,
                            status: "success".to_string(),
                            result: Some("child result payload".to_string()),
                            error: None,
                            sources: vec![],
                        })
                        .await;
                }
            }
        });
        ylva::child_agent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
    agent = agent.with_child_agent_manager(manager, finish_rx, subagent_ui_rx);

    run_cli_to(
        agent,
        ui_rx,
        registry,
        "delegate it".to_string(),
        &mut output,
    )
    .await
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(
        output.contains("child result payload"),
        "expected the child's result to be printed before the CLI exits, output: {output}"
    );
    // Regression check for the forced re-prompt: the mock provider is only
    // called again once (call #2, the reprompt right after `spawn_subagent`
    // resolves) if the root never reacts to the child's finish. A third call
    // proves `AgentLoop` re-prompted the root LLM after the child's result
    // landed in context (see the `finish_rx` arm's `mode == Idle` check).
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "expected a third provider call: the reprompt after the child's finish result, output: {output}"
    );
}

/// A provider that returns a `spawn_subagent` tool call on its first `chat()`
/// call, and plain text embedding the 1-based call index on every
/// subsequent call (`"auto-reply-<n>"`). Used to distinguish, purely from
/// printed output, which of several interleaved provider calls (shared by
/// the root and its child, since [`ylva::AgentFactory`] resolves both
/// against the same named engine) produced a given piece of text.
struct CallCountingProvider {
    calls: Arc<AtomicUsize>,
}

impl Provider for CallCountingProvider {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                if call_index == 1 {
                    let _ = tx
                        .send(StreamEvent::Delta(DeltaChunk {
                            tool_call_index: 0,
                            tool_call_id: None,
                            function_name: Some("spawn_subagent".to_string()),
                            arguments_fragment: Some(
                                serde_json::json!({
                                    "label": "Do it",
                                    "task": "do it",
                                })
                                .to_string(),
                            ),
                        }))
                        .await;
                } else {
                    let _ = tx
                        .send(StreamEvent::Text(format!("auto-reply-{call_index}")))
                        .await;
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ylva::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[tokio::test]
async fn console_app_reacts_to_child_finish_through_real_agent_factory_and_run_child() {
    // Regression/end-to-end test: unlike
    // `run_cli_waits_for_spawned_child_before_ending_conversation` (which
    // wires a hand-rolled `SpawnFn`/`ChildAgentManager` directly onto
    // `AgentLoop`), this test goes through the *real* production path:
    // `ylva::AgentFactory::create` (the same call `ConsoleApp::run` makes)
    // wiring a real `Role::Child` agent run via `run_child`
    // (`src/agent_factory.rs`), including its `forward_task`/`child_loop`
    // drain-on-drop cleanup. It exercises the "child never called
    // `agent_finish`" unstructured-fallback path, since the mock child's
    // reply here is plain text with no tool call.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CallCountingProvider {
        calls: Arc::clone(&calls),
    });
    let engine: Arc<dyn ylva::Engine> =
        Arc::new(EngineImpl::new(provider, common::test_engine_config()));

    let profile = ylva::ProfileConfig {
        default_engine: "default".to_string(),
        compact_engine: None,
        agents: std::collections::HashMap::from([(
            "default".to_string(),
            ylva::AgentConfig {
                engine: None,
                description: None,
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                system_prompt: None,
                can_spawn: true,
                tools: std::collections::HashMap::new(),
            },
        )]),
        ..ylva::ProfileConfig::default()
    };

    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([(
        "default".to_string(),
        engine,
    )]))
    .with_auto_approve(AutoApproveTool::Spawn);

    let (agent, ui_rx) = factory
        .create(&profile, "default", ylva::Role::Root)
        .expect("factory creates root agent");

    let registry = Arc::new(default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        ToolRegistryConfig::default(),
    ));

    let mut output = Vec::new();
    run_cli_to(
        agent,
        ui_rx,
        registry,
        "delegate it".to_string(),
        &mut output,
    )
    .await
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    // Exactly 4 provider calls are causally required: #1 the root's
    // `spawn_subagent` tool call; #2 the root's immediate reprompt once
    // `spawn_subagent` resolves with `{"status":"running"}`; #3 the child's
    // own (sole) turn, which never calls `agent_finish` so `run_child`
    // sends an "unstructured" fallback `FinishResult`; #4 the root's
    // reprompt once that fallback result lands in its context via the
    // `finish_rx` arm. #4 can only be dispatched after #3 completes, so it
    // is always the last call made, whichever of #2/#3 the scheduler
    // happens to run first.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "expected the root to react to the child's result with a 4th provider call, output: {output}"
    );
    assert!(
        output.contains("auto-reply-4"),
        "expected the root's post-finish reprompt reply to be printed, output: {output}"
    );
}

/// A provider shared by a root agent and its spawned child (both resolved
/// against the same named engine by `AgentFactory`), that tells the two
/// apart by whether `agent_finish` appears in the tool schema list handed to
/// `chat()` — only a `Role::Child` registry has that tool (see
/// `AgentFactory::create`). Each side gets its own independent call counter,
/// so the root and child scripts don't need to reason about which of them
/// the scheduler happens to invoke first.
struct RootAndChildProvider {
    root_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
}

impl Provider for RootAndChildProvider {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let is_child = tools.iter().any(|tool| tool.name == "agent_finish");
        let call_index = if is_child {
            self.child_calls.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.root_calls.fetch_add(1, Ordering::SeqCst) + 1
        };
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let event = match (is_child, call_index) {
                    (false, 1) => StreamEvent::Delta(DeltaChunk {
                        tool_call_index: 0,
                        tool_call_id: None,
                        function_name: Some("spawn_subagent".to_string()),
                        arguments_fragment: Some(
                            serde_json::json!({ "label": "Run a command", "task": "run echo hi" })
                                .to_string(),
                        ),
                    }),
                    (true, 1) => StreamEvent::Delta(DeltaChunk {
                        tool_call_index: 0,
                        tool_call_id: None,
                        function_name: Some("bash".to_string()),
                        arguments_fragment: Some(
                            serde_json::json!({ "command": "echo hi" }).to_string(),
                        ),
                    }),
                    (true, _) => StreamEvent::Delta(DeltaChunk {
                        tool_call_index: 0,
                        tool_call_id: None,
                        function_name: Some("agent_finish".to_string()),
                        arguments_fragment: Some(
                            serde_json::json!({
                                "status": "success",
                                "result": "child result payload",
                            })
                            .to_string(),
                        ),
                    }),
                    (false, _) => StreamEvent::Text(format!("root-reply-{call_index}")),
                };
                let _ = tx.send(event).await;
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ylva::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[tokio::test]
async fn spawned_child_bash_approval_is_routed_to_the_child_not_the_root() {
    // Regression/end-to-end test for the original hang: a spawned child
    // agent's own tool call (`bash`, `UserApprovalRequired`) must resolve
    // against *that child's* pending queue when the driver answers it, not
    // be silently dropped or (mis)applied to the root's queue — the latter
    // is exactly how the original bug hung forever, since the child never
    // saw an answer to its own `ToolPending` and the root had nothing
    // pending to match the stray command against.
    //
    // `cli_driver::run_cli_to` always auto-denies `UserApprovalRequired`
    // tools ("not supported in CLI mode"), but critically it must route
    // that denial back to the *child* (via `AgentCommand::ForChild`,
    // resolved through `ChildAgentManager::running_command_sender`) using
    // the `run_id` stamped on the child's `UiUpdate::ToolPending`. If that
    // routing were broken, the child would stay parked at `ToolPending`
    // forever and the whole run would hang, reproducing the original bug.
    let root_calls = Arc::new(AtomicUsize::new(0));
    let child_calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(RootAndChildProvider {
        root_calls: Arc::clone(&root_calls),
        child_calls: Arc::clone(&child_calls),
    });
    let engine: Arc<dyn ylva::Engine> =
        Arc::new(EngineImpl::new(provider, common::test_engine_config()));

    let profile = ylva::ProfileConfig {
        default_engine: "default".to_string(),
        compact_engine: None,
        agents: std::collections::HashMap::from([(
            "default".to_string(),
            ylva::AgentConfig {
                engine: None,
                description: None,
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                system_prompt: None,
                can_spawn: true,
                tools: std::collections::HashMap::new(),
            },
        )]),
        ..ylva::ProfileConfig::default()
    };

    let factory = ylva::AgentFactory::new(std::collections::HashMap::from([(
        "default".to_string(),
        engine,
    )]))
    .with_auto_approve(AutoApproveTool::Spawn);

    let (agent, ui_rx) = factory
        .create(&profile, "default", ylva::Role::Root)
        .expect("factory creates root agent");

    let registry = Arc::new(default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new())),
        ToolRegistryConfig::default(),
    ));

    let mut output = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        run_cli_to(
            agent,
            ui_rx,
            registry,
            "delegate a command".to_string(),
            &mut output,
        ),
    )
    .await
    .expect("run must not hang: the child's bash approval must be routed to it, not dropped")
    .expect("headless run succeeds");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(
        output.contains("not supported in CLI mode"),
        "expected the child's bash call to surface an auto-denial, output: {output}"
    );
    assert!(
        output.contains("child result payload"),
        "expected the child to still report its result after the denied bash call \
         (proving it wasn't stuck waiting for an approval that never reached it), \
         output: {output}"
    );
    assert_eq!(
        child_calls.load(Ordering::SeqCst),
        2,
        "expected exactly 2 child provider calls: the bash call, then agent_finish \
         after the denial is fed back, output: {output}"
    );
}
