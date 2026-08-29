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

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use ylva::llm::{MockProvider, OpenAiProvider, Provider};
use ylva::logging::{init_logging, LogConfig, LogFormat};
use ylva::{
    AgentConfig, AgentFactory, App, ConsoleApp, Engine, EngineConfig, EngineImpl, ProfileConfig,
    TuiApp, DEFAULT_CONTEXT_LENGTH, DEFAULT_TUI_TOOLS,
};

/// Fallback demo prompt used when neither `--prompt` nor piped stdin supplies
/// the initial user turn.
const DEFAULT_DEMO_PROMPT: &str = "What makes the sky look blue?";

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("{}", ylva::BUILD_INFO.render());
        return;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let log_config = match logging_from_args(&args) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    // The guard must outlive the whole run so buffered log events are flushed
    // on exit. Logging is opt-in: `init_logging` installs nothing and returns
    // `None` when no `--log-file` was given.
    let _log_guard = match init_logging(log_config) {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };

    let provider: Arc<dyn Provider> = select_provider();
    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");

    let engine_config = EngineConfig {
        provider: provider.name().to_string(),
        model: model_from_env(),
        temperature: None,
        context_length: DEFAULT_CONTEXT_LENGTH,
        api_base_url: None,
        api_key: None,
    };
    let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, engine_config));

    let profile = ProfileConfig {
        default_engine: "default".to_string(),
        compact_engine: None,
        agents: HashMap::from([(
            "default".to_string(),
            AgentConfig {
                engine: None,
                allowed_tools: None,
                timeout_secs: None,
                persona: None,
                can_spawn: true,
            },
        )]),
    };

    let auto_approve_edit = args.contains(&"--auto-approve-edit".to_string());
    let auto_approve_write = args.contains(&"--auto-approve-write".to_string());
    let use_tui = std::io::stdout().is_terminal() && !args.contains(&"--no-tui".to_string());

    let factory = Arc::new({
        let mut factory = AgentFactory::new(HashMap::from([(
            "default".to_string(),
            engine as Arc<dyn Engine>,
        )]));
        if use_tui {
            factory = factory.with_allowed_tools(DEFAULT_TUI_TOOLS);
        }
        if auto_approve_edit {
            factory = factory.with_auto_approve_edit();
        }
        if auto_approve_write {
            factory = factory.with_auto_approve_write();
        }
        factory
    });

    let result = runtime.block_on(async {
        if use_tui {
            // The TUI starts with an empty input box. An explicit `--prompt` (or
            // piped stdin) still auto-sends, but the hardcoded demo prompt is only
            // for the headless path — it must not pre-populate the interactive UI.
            let explicit_prompt = explicit_prompt(args);
            TuiApp::new(
                Arc::clone(&factory),
                profile,
                "default".to_string(),
                explicit_prompt,
            )
            .run()
            .await
        } else {
            let prompt = initial_prompt(args);
            ConsoleApp::new(
                Arc::clone(&factory),
                profile,
                "default".to_string(),
                prompt,
                Box::new(std::io::stdout()),
            )
            .run()
            .await
        }
    });
    match result {
        Ok(exit_code) => {
            if exit_code != 0 {
                std::process::exit(exit_code as i32);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}

/// The explicitly supplied initial user turn, if any: `--prompt <text>`, else
/// piped stdin read to EOF. Returns `None` when neither was given (or stdin is
/// a terminal / whitespace-only). Used by the TUI, which starts with an empty
/// input box unless the user supplied a turn.
fn explicit_prompt(args: Vec<String>) -> Option<String> {
    let prompt = prompt_from_arg(&args).or_else(prompt_from_stdin)?;
    let trimmed = prompt.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The initial user turn sent to the agent in headless mode, resolved by
/// precedence: `--prompt <text>`, else piped stdin read to EOF, else the
/// hardcoded demo prompt. stdin is only consumed when it is **not** a terminal
/// (something is piped in); no interactive prompt-read is attempted in the
/// CLI-only phase. The resolved prompt is trimmed (leading/trailing whitespace
/// removed) before it is echoed and sent.
fn initial_prompt(args: Vec<String>) -> String {
    explicit_prompt(args).unwrap_or_else(|| DEFAULT_DEMO_PROMPT.to_string())
}

/// Extract the value of the first `--prompt <text>` argument, if any.
fn prompt_from_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--prompt" {
            return iter.next().cloned();
        }
    }
    None
}

/// Build the logging configuration from the CLI arguments, parsing
/// `--log-file <path>`, `--log-level <level>`, and `--log-format <format>`.
///
/// Missing flags fall back to the `LogConfig` defaults (no file, `info` level,
/// `text` format). An unknown `--log-level` or `--log-format` value is an
/// error. Values are matched case-insensitively (`"iNFo"`, `"TEXT"` are
/// accepted); the canonical forms are lowercase. All other arguments are
/// ignored here.
fn logging_from_args(args: &[String]) -> Result<LogConfig, String> {
    let mut log_file = None;
    let mut log_level = None;
    let mut log_format = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--log-file" => {
                log_file = Some(
                    iter.next()
                        .ok_or("--log-file requires a path value")?
                        .into(),
                );
            }
            "--log-level" => {
                log_level = Some(
                    iter.next()
                        .ok_or("--log-level requires a value")?
                        .to_string(),
                );
            }
            "--log-format" => {
                log_format = Some(
                    iter.next()
                        .ok_or("--log-format requires a value")?
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    let log_level = match log_level.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => tracing::Level::INFO,
        Some("trace") => tracing::Level::TRACE,
        Some("debug") => tracing::Level::DEBUG,
        Some("info") => tracing::Level::INFO,
        Some("warn") => tracing::Level::WARN,
        Some("error") => tracing::Level::ERROR,
        Some(other) => {
            return Err(format!(
                "invalid --log-level '{other}': expected trace, debug, info, warn, or error"
            ));
        }
    };
    let log_format = match log_format
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => LogFormat::Text,
        Some("text") => LogFormat::Text,
        Some("json") => LogFormat::Json,
        Some(other) => {
            return Err(format!(
                "invalid --log-format '{other}': expected text or json"
            ));
        }
    };
    Ok(LogConfig {
        log_file,
        log_level,
        log_format,
    })
}

/// Read all of stdin when it is not a terminal. Returns `None` when stdin is a
/// terminal (no piped input to consume) or contains only whitespace.
fn prompt_from_stdin() -> Option<String> {
    use std::io::Read;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer).ok()?;
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

/// Default Ollama model when `OLLAMA_MODEL` is unset.
const DEFAULT_OLLAMA_MODEL: &str = "gemma4:e4b-mlx";
/// Default OpenRouter model when `OPENROUTER_MODEL` is unset.
const DEFAULT_OPENROUTER_MODEL: &str = "deepseek/deepseek-v4-flash-0731";

/// Return the model name that matches the active provider's env vars.
///
/// Mirrors the precedence of [`select_provider`]: Ollama first, then
/// OpenRouter, then an empty string for the `MockProvider` fallback.
///
/// TODO: this duplicates provider-specific knowledge that should live on the
/// provider itself (see `docs/ideas/resolving-provider.md`). Once a
/// `ProviderRegistry` exists, replace both `select_provider` and
/// `model_from_env` with a single `ProviderRegistry::resolve_provider` call
/// that returns a fully-configured provider (including model).
fn model_from_env() -> String {
    if let Ok(base_url) = std::env::var("OLLAMA_BASE_URL") {
        if !base_url.is_empty() {
            return std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
        }
    }
    if let Ok(base_url) = std::env::var("OPENROUTER_BASE_URL") {
        if !base_url.is_empty() {
            return std::env::var("OPENROUTER_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENROUTER_MODEL.to_string());
        }
    }
    String::new()
}

/// Select the Phase 2 demo provider from environment variables with
/// **Ollama-first** precedence. Falls back to the `MockProvider` when no
/// backend is configured (still headless; no CLI/config parsing in Phase 2).
///
/// The chosen wiring is reported as a `debug!` log event (visible when the
/// user enables file logging at `--log-level debug`), so a misconfigured
/// backend (wrong `base_url`, model, or auth) is traceable at startup instead
/// of surfacing only as a confusing downstream error. Only the *presence* of an
/// API key is logged — `"set"`/`"unset"` — never the key value itself.
fn select_provider() -> Arc<dyn Provider> {
    if let Ok(base_url) = std::env::var("OLLAMA_BASE_URL") {
        if !base_url.is_empty() {
            let model =
                std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
            let api_key = std::env::var("OLLAMA_API_KEY").ok();
            tracing::debug!(
                provider = "ollama",
                base_url,
                model,
                api_key = if api_key.is_some() { "set" } else { "unset" },
                "provider selected"
            );
            return Arc::new(OpenAiProvider::new(
                "ollama".to_string(),
                base_url,
                model,
                api_key,
            ));
        }
    }
    if let Ok(base_url) = std::env::var("OPENROUTER_BASE_URL") {
        if !base_url.is_empty() {
            let model = std::env::var("OPENROUTER_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENROUTER_MODEL.to_string());
            let api_key = std::env::var("OPENROUTER_API_KEY").ok();
            tracing::debug!(
                provider = "openrouter",
                base_url,
                model,
                api_key = if api_key.is_some() { "set" } else { "unset" },
                "provider selected"
            );
            return Arc::new(OpenAiProvider::new(
                "openrouter".to_string(),
                base_url,
                model,
                api_key,
            ));
        }
    }
    tracing::debug!(
        provider = "mock",
        base_url = "-",
        model = "-",
        api_key = "unset",
        "provider selected"
    );
    Arc::new(MockProvider::new(
        vec![
            "The sky looks blue ".to_string(),
            "because air molecules scatter ".to_string(),
            "blue light more than other colors.".to_string(),
        ],
        Duration::from_millis(60),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_from_args_accepts_mixed_case_values() {
        let args = vec![
            "--log-level".to_string(),
            "iNFo".to_string(),
            "--log-format".to_string(),
            "Text".to_string(),
        ];
        let config = logging_from_args(&args).expect("mixed-case values accepted");
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn logging_from_args_accepts_all_levels_case_insensitively() {
        for (raw, level) in [
            ("Trace", tracing::Level::TRACE),
            ("DEBUG", tracing::Level::DEBUG),
            ("InFo", tracing::Level::INFO),
            ("Warn", tracing::Level::WARN),
            ("Error", tracing::Level::ERROR),
        ] {
            let config = logging_from_args(&["--log-level".to_string(), raw.to_string()])
                .expect("valid level");
            assert_eq!(config.log_level, level, "for input {raw:?}");
        }
    }

    #[test]
    fn logging_from_args_accepts_json_case_insensitively() {
        let config = logging_from_args(&["--log-format".to_string(), "JSON".to_string()])
            .expect("valid format");
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn logging_from_args_rejects_unknown_values() {
        let bad_level = logging_from_args(&["--log-level".to_string(), "verbose".to_string()]);
        assert!(bad_level.is_err());
        let bad_format = logging_from_args(&["--log-format".to_string(), "xml".to_string()]);
        assert!(bad_format.is_err());
    }

    #[test]
    fn prompt_from_arg_reads_value_after_flag() {
        assert_eq!(
            prompt_from_arg(&["--prompt".to_string(), "hello".to_string()]),
            Some("hello".to_string())
        );
        assert_eq!(prompt_from_arg(&["--log-file".to_string()]), None);
    }

    #[test]
    fn initial_prompt_prefers_arg_over_default() {
        assert_eq!(
            initial_prompt(vec!["--prompt".to_string(), "  hi  ".to_string()]),
            "hi"
        );
    }

    #[test]
    fn explicit_prompt_is_none_without_user_turn() {
        assert_eq!(explicit_prompt(Vec::new()), None);
        assert_eq!(explicit_prompt(vec!["--log-file".to_string()]), None);
    }

    #[test]
    fn explicit_prompt_returns_trimmed_arg() {
        assert_eq!(
            explicit_prompt(vec!["--prompt".to_string(), "  hi  ".to_string()]),
            Some("hi".to_string())
        );
    }
}
