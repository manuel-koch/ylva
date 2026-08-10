use std::sync::Arc;
use std::time::Duration;

use ylva::agent::{AgentCommand, AgentLoop, UiUpdate};
use ylva::llm::MockProvider;

fn main() {
    let mock_provider = MockProvider::new(
        vec![
            "The sky looks blue ".to_string(),
            "because air molecules scatter ".to_string(),
            "blue light more than other colors.".to_string(),
        ],
        Duration::from_millis(60),
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(mock_provider));

    let prompt = "What makes the sky look blue?".to_string();
    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
    runtime.block_on(async {
        agent
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        let handle = agent.run();

        let mut response_finished = false;
        while !response_finished {
            match ui_rx.recv().await {
                Some(UiUpdate::StreamStarted) => {
                    println!("User: {prompt}");
                    print!("Assistant: ");
                }
                Some(UiUpdate::AppendText(text)) => print!("{text}"),
                Some(UiUpdate::StreamDone) => {
                    println!();
                    response_finished = true;
                }
                Some(UiUpdate::StreamCancelled) => {
                    println!(" [interrupted]");
                    response_finished = true;
                }
                Some(UiUpdate::Error(error)) => {
                    eprintln!();
                    eprintln!("[error] {error}");
                    response_finished = true;
                }
                None => response_finished = true,
            }
        }

        // Dropping the AgentLoop closes cmd_tx, which makes the outer loop
        // return the final committed history from `handle`.
        drop(agent);

        let final_context = handle
            .await
            .expect("agent loop task panicked")
            .expect("agent loop returned an error");
        println!(
            "\nFinal conversation history ({} messages):",
            final_context.messages().len()
        );
        for message in final_context.messages() {
            println!("{message:?}");
        }
    });
}
