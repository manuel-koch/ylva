use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use ylva::{AgentCommand, AgentLoop, Message, MockProvider, UiUpdate};

#[tokio::test(start_paused = true)]
async fn multiple_steers_during_stream_complete_without_deadlock() {
    let provider = MockProvider::new(vec!["a-".into(), "b".into()], Duration::from_millis(10));
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider));

    agent
        .send_command(AgentCommand::Prompt("first".into()))
        .await;
    let handle = agent.run();

    // Wait until the first request streams, then queue several steers at once.
    while !matches!(ui_rx.recv().await, Some(UiUpdate::StreamStarted)) {}

    for steer in ["s1", "s2", "s3", "s4", "s5"] {
        agent
            .send_command(AgentCommand::Prompt(steer.to_string()))
            .await;
    }

    // Drive time forward until the first request plus the aggregated steer
    // request have completed (one StreamDone each: steers are injected as a
    // single fresh request). A deadlock shows up as budget exhaustion instead
    // of a hang.
    let expected_dones = 2;
    let mut stream_dones = 0;
    let mut budget = 200;
    while stream_dones < expected_dones {
        drain_ready(&mut ui_rx, &mut stream_dones);
        if stream_dones >= expected_dones {
            break;
        }
        budget -= 1;
        assert!(budget > 0, "agent loop deadlocked during concurrent steers");
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;
    }

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    let user_messages: Vec<&str> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    // "first" plus each of the five steers, all injected in order (the steer
    // queue is drained into a single aggregated fresh request).
    assert_eq!(user_messages, ["first", "s1", "s2", "s3", "s4", "s5"]);

    let assistant_count = context
        .messages()
        .iter()
        .filter(|message| matches!(message, Message::Assistant { .. }))
        .count();
    assert_eq!(assistant_count, expected_dones);
}

fn drain_ready(ui_rx: &mut mpsc::Receiver<UiUpdate>, stream_dones: &mut usize) {
    while let Ok(update) = ui_rx.try_recv() {
        if matches!(update, UiUpdate::StreamDone) {
            *stream_dones += 1;
        }
    }
}
