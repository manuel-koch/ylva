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

use std::sync::Arc;

use ylva::{
    AgentCommand, AgentLoop, EngineImpl, Message, MockProvider, ToolRegistry, UiUpdate,
    DEFAULT_SOUL, TOOL_PROTOCOL_SYSTEM_PROMPT,
};

#[tokio::test]
async fn empty_prompt_is_a_noop_and_final_context_stays_seeded() {
    let provider = MockProvider::new(Vec::new(), std::time::Duration::from_millis(10));
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::new(ToolRegistry::new()),
    );

    agent
        .send_command(AgentCommand::Prompt(String::new()))
        .await;
    let handle = agent.run();

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    // No request was started: history still contains only the seeded system
    // message, and no UI update was ever produced.
    assert_eq!(
        context.messages(),
        &[
            Message::System {
                content: DEFAULT_SOUL.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
        ]
    );
    assert!(!matches!(ui_rx.try_recv(), Ok(UiUpdate::StreamStarted)));
}
