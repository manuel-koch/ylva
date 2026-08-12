use std::sync::Arc;
use std::time::Duration;

use ylva::agent::{AgentCommand, AgentLoop, UiUpdate};
use ylva::llm::{MockProvider, OpenAiProvider, Provider};

fn main() {
    let provider: Arc<dyn Provider> = select_provider();
    let (mut agent, mut ui_rx) = AgentLoop::new(provider);

    let prompt = "What makes the sky look blue?".to_string();
    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
    runtime.block_on(async {
        // Echo the user's turn before sending the request, so it appears even
        // while the provider is still connecting / retrying (the "Assistant:"
        // prefix is printed when the stream actually starts).
        println!("User: {prompt}");
        agent
            .send_command(AgentCommand::Prompt(prompt.clone()))
            .await;
        let handle = agent.run();

        let mut response_finished = false;
        while !response_finished {
            match ui_rx.recv().await {
                Some(UiUpdate::StreamStarted) => {
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

/// Default Ollama model when `OLLAMA_MODEL` is unset.
const DEFAULT_OLLAMA_MODEL: &str = "gemma4:e4b-mlx";
/// Default OpenRouter model when `OPENROUTER_MODEL` is unset.
const DEFAULT_OPENROUTER_MODEL: &str = "deepseek/deepseek-v4-flash-0731";

/// Select the Phase 2 demo provider from environment variables with
/// **Ollama-first** precedence. Falls back to the `MockProvider` when no
/// backend is configured (still headless; no CLI/config parsing in Phase 2).
fn select_provider() -> Arc<dyn Provider> {
    if let Ok(base_url) = std::env::var("OLLAMA_BASE_URL") {
        if !base_url.is_empty() {
            let model =
                std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
            let api_key = std::env::var("OLLAMA_API_KEY").ok();
            report_provider("ollama", Some(&base_url), Some(&model), api_key.is_some());
            return Arc::new(OpenAiProvider::new(base_url, model, api_key));
        }
    }
    if let Ok(base_url) = std::env::var("OPENROUTER_BASE_URL") {
        if !base_url.is_empty() {
            let model = std::env::var("OPENROUTER_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENROUTER_MODEL.to_string());
            let api_key = std::env::var("OPENROUTER_API_KEY").ok();
            report_provider(
                "openrouter",
                Some(&base_url),
                Some(&model),
                api_key.is_some(),
            );
            return Arc::new(OpenAiProvider::new(base_url, model, api_key));
        }
    }
    report_provider("mock", None, None, false);
    Arc::new(MockProvider::new(
        vec![
            "The sky looks blue ".to_string(),
            "because air molecules scatter ".to_string(),
            "blue light more than other colors.".to_string(),
        ],
        Duration::from_millis(60),
    ))
}

/// Print the selected provider wiring to stderr when `YLVA_VERBOSE` is set, so
/// a misconfigured backend (wrong `base_url`, model, or auth) is visible at
/// startup instead of surfacing as a confusing downstream error.
fn report_provider(kind: &str, base_url: Option<&str>, model: Option<&str>, has_api_key: bool) {
    if !verbose_enabled() {
        return;
    }
    let base = base_url.unwrap_or("-");
    let model = model.unwrap_or("-");
    let auth = if has_api_key { "set" } else { "unset" };
    eprintln!("[ylva] provider={kind} base_url={base} model={model} api_key={auth}");
}

fn verbose_enabled() -> bool {
    std::env::var("YLVA_VERBOSE").is_ok_and(|value| !value.is_empty())
}
