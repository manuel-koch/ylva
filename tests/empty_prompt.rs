use std::sync::Arc;

use ylva::{
    AgentCommand, AgentLoop, Message, MockProvider, ToolRegistry, UiUpdate, DEFAULT_SYSTEM_PROMPT,
    TOOL_PROTOCOL_SYSTEM_PROMPT,
};

#[tokio::test]
async fn empty_prompt_is_a_noop_and_final_context_stays_seeded() {
    let provider = MockProvider::new(Vec::new(), std::time::Duration::from_millis(10));
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::new(ToolRegistry::new()));

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
                content: DEFAULT_SYSTEM_PROMPT.to_string(),
            },
            Message::System {
                content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
            },
        ]
    );
    assert!(!matches!(ui_rx.try_recv(), Ok(UiUpdate::StreamStarted)));
}
