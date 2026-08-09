mod client;
mod mock_server;
mod verify;

use std::sync::Arc;
use std::time::{Duration, Instant};

use client::{BreakReason, CancelMode, DropSubCase, EndpointConfig, RunSummary};
use verify::{AwaitCloseOutcome, VerifyFailure};

const DEFAULT_FRAME_CAP: usize = 300;
const DEFAULT_MODEL: &str = "some-model";
/// The user message sent to the streaming endpoint for every run.
const PROMPT: &str = "How can you help me?";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointKind {
    Mock,
    OpenRouter,
    Ollama,
}

#[derive(Debug)]
struct Options {
    endpoint: EndpointKind,
    cancel_mode: CancelMode,
    cancel_at: f64,
    iterations: usize,
    sweep_fractions: Vec<f64>,
    frame_cap: Option<usize>,
    base_url: Option<String>,
    model: Option<String>,
    race_sweep: bool,
    race_iterations: usize,
    race_offset_events: usize,
    print_events: usize,
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        endpoint: EndpointKind::Mock,
        cancel_mode: CancelMode::Token,
        cancel_at: 0.5,
        iterations: 5,
        sweep_fractions: vec![0.1, 0.3, 0.5, 0.7, 0.9],
        frame_cap: Some(DEFAULT_FRAME_CAP),
        base_url: None,
        model: None,
        race_sweep: true,
        race_iterations: 300,
        race_offset_events: 6,
        print_events: 0,
    };

    let require_value = |name: &str, value: Option<String>| -> Result<String, String> {
        value.ok_or_else(|| format!("{name} needs a value"))
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => {
                let value = require_value("--endpoint", args.next())?;
                options.endpoint = match value.as_str() {
                    "mock" => EndpointKind::Mock,
                    "openrouter" => EndpointKind::OpenRouter,
                    "ollama" => EndpointKind::Ollama,
                    other => {
                        return Err(format!(
                            "unknown --endpoint: {other} (mock|openrouter|ollama)"
                        ));
                    }
                };
            }
            "--cancel-mode" => {
                let value = require_value("--cancel-mode", args.next())?;
                options.cancel_mode = match value.as_str() {
                    "drop" => CancelMode::Drop(DropSubCase::AdapterOnly),
                    "driver" => CancelMode::Drop(DropSubCase::AdapterOnly),
                    "token" => CancelMode::Token,
                    other => return Err(format!("unknown --cancel-mode: {other} (drop|token)")),
                };
            }
            "--drop-subcase" => {
                let value = require_value("--drop-subcase", args.next())?;
                options.cancel_mode = match value.as_str() {
                    "adapter-only" => CancelMode::Drop(DropSubCase::AdapterOnly),
                    "adapter-response" => CancelMode::Drop(DropSubCase::AdapterAndResponse),
                    other => {
                        return Err(format!(
                            "unknown --drop-subcase: {other} (adapter-only|adapter-response)"
                        ));
                    }
                };
            }
            "--cancel-at" => {
                let value = require_value("--cancel-at", args.next())?;
                options.cancel_at = value.parse().map_err(|_| {
                    format!(
                        "--cancel-at needs a number (fraction 0..1 or event offset), got '{value}'"
                    )
                })?;
            }
            "--iterations" => {
                let value = require_value("--iterations", args.next())?;
                options.iterations = value
                    .parse()
                    .map_err(|_| format!("--iterations needs a count, got '{value}'"))?;
            }
            "--sweep-fractions" => {
                let value = require_value("--sweep-fractions", args.next())?;
                options.sweep_fractions = value
                    .split(',')
                    .filter_map(|part| part.trim().parse::<f64>().ok())
                    .collect();
                if options.sweep_fractions.is_empty() {
                    return Err(format!(
                        "--sweep-fractions needs a comma-separated list of fractions, got '{value}'"
                    ));
                }
            }
            "--frame-cap" => {
                let value = require_value("--frame-cap", args.next())?;
                let frame_cap: usize = value
                    .parse()
                    .map_err(|_| format!("--frame-cap needs a count, got '{value}'"))?;
                options.frame_cap = (frame_cap > 0).then_some(frame_cap);
            }
            "--base-url" => {
                options.base_url = Some(require_value("--base-url", args.next())?);
            }
            "--model" => {
                options.model = Some(require_value("--model", args.next())?);
            }
            "--race-sweep" => options.race_sweep = true,
            "--no-race-sweep" => options.race_sweep = false,
            "--race-iterations" => {
                let value = require_value("--race-iterations", args.next())?;
                options.race_iterations = value
                    .parse()
                    .map_err(|_| format!("--race-iterations needs a count, got '{value}'"))?;
            }
            "--race-offset-events" => {
                let value = require_value("--race-offset-events", args.next())?;
                options.race_offset_events = value
                    .parse()
                    .map_err(|_| format!("--race-offset-events needs a count, got '{value}'"))?;
            }
            "--print-events" => {
                let value = require_value("--print-events", args.next())?;
                options.print_events = value
                    .parse()
                    .map_err(|_| format!("--print-events needs a count, got '{value}'"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(options)
}

/// Result of one cancel-triggered iteration.
struct IterationResult {
    label: String,
    break_reason: &'static str,
    events: usize,
    accumulated: String,
    received: Vec<String>,
    events_after_trigger: usize,
    close: AwaitCloseOutcome,
}

/// Resolve the cancel offset for a given `--cancel-at` value. Values `0 < x <= 1`
/// are fractions of the frame budget; values `> 1` are absolute event offsets.
fn resolve_offset(value: f64, frame_cap: Option<usize>) -> usize {
    if value <= 1.0 {
        return client::event_offset(value, frame_cap.unwrap_or(DEFAULT_FRAME_CAP));
    }
    match frame_cap {
        Some(cap) => (value as usize).clamp(1, cap.saturating_sub(1)),
        None => value as usize,
    }
}

/// One scenario step for the mock server: connect and cancel mid-stream.
async fn run_iteration(
    endpoint: &EndpointConfig,
    cancel_mode: CancelMode,
    offset_events: usize,
    live: Option<&Arc<std::sync::atomic::AtomicUsize>>,
    port: u16,
    label: String,
) -> IterationResult {
    eprintln!(
        "  {label}: connecting to {} (cancel at event {offset_events}) ...",
        endpoint.completions_url()
    );
    let summary: RunSummary = match cancel_mode {
        CancelMode::Drop(sub_case) => {
            client::stream_and_drop(endpoint, PROMPT, sub_case, offset_events)
                .await
                .unwrap_or_else(|message| RunSummary {
                    events: 0,
                    accumulated_text: String::new(),
                    received: Vec::new(),
                    break_reason: BreakReason::Error(message.to_string()),
                    triggered_at: Some(Instant::now()),
                    events_after_trigger: 0,
                })
        }
        CancelMode::Token => client::stream_with_token(endpoint, PROMPT, offset_events)
            .await
            .unwrap_or_else(|message| RunSummary {
                events: 0,
                accumulated_text: String::new(),
                received: Vec::new(),
                break_reason: BreakReason::Error(message.to_string()),
                triggered_at: Some(Instant::now()),
                events_after_trigger: 0,
            }),
    };

    let close = match summary.triggered_at {
        Some(triggered_at) => verify::await_close(triggered_at, live, port).await,
        // Never triggered (stream ended or errored early): report a timeout so
        // the failure is surfaced rather than silently passing.
        None => AwaitCloseOutcome {
            close_latency: verify::CLOSE_AWAIT_TIMEOUT,
            server_released: false,
            sockets_clean: false,
            timed_out: true,
        },
    };

    let break_reason = match &summary.break_reason {
        BreakReason::Cancelled => "cancelled",
        BreakReason::Dropped => "dropped",
        BreakReason::StreamEnded => "stream-ended",
        BreakReason::Error(_) => "error",
    };

    IterationResult {
        label,
        break_reason,
        events: summary.events,
        accumulated: summary.accumulated_text,
        received: summary.received,
        events_after_trigger: summary.events_after_trigger,
        close,
    }
}

fn format_latency(value: &Duration) -> String {
    format!("{:.2}ms", value.as_secs_f64() * 1000.0)
}

fn p90(sorted: &[Duration]) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = ((sorted.len() as f64 * 0.9) as usize).min(sorted.len() - 1);
    sorted[index]
}

async fn run(options: Options) -> i32 {
    let socket_tools_available = verify::socket_tools_available();
    if !socket_tools_available {
        eprintln!(
            "warning: neither `lsof` nor `netstat` is available; socket checks will be skipped"
        );
    }

    let server = match options.endpoint {
        EndpointKind::Mock => Some(
            mock_server::MockServer::start(options.frame_cap)
                .await
                .expect("mock server failed to start"),
        ),
        _ => None,
    };

    let endpoint = match options.endpoint {
        EndpointKind::Mock => {
            let addr = server.as_ref().expect("mock server present").addr;
            EndpointConfig {
                base_url: format!("http://{addr}"),
                model: options.model.unwrap_or_else(|| DEFAULT_MODEL.into()),
                api_key: None,
            }
        }
        EndpointKind::OpenRouter => {
            let key = std::env::var("OPENROUTER_API_KEY")
                .expect("OPENROUTER_API_KEY must be set for --endpoint openrouter");
            EndpointConfig {
                base_url: options
                    .base_url
                    .unwrap_or_else(|| "https://openrouter.ai/api/v1".into()),
                model: options.model.unwrap_or_else(|| DEFAULT_MODEL.into()),
                api_key: Some(key),
            }
        }
        EndpointKind::Ollama => EndpointConfig {
            base_url: options
                .base_url
                .unwrap_or_else(|| "http://localhost:11434".into()),
            model: options.model.unwrap_or_else(|| DEFAULT_MODEL.into()),
            api_key: None,
        },
    };

    // Port used for OS socket inspection. Mock: the loopback listen port
    // (which our outbound socket targets). Remote: the remote TCP port.
    let socket_port = match options.endpoint {
        EndpointKind::Mock => server.as_ref().expect("mock present").addr.port(),
        EndpointKind::OpenRouter => 443,
        EndpointKind::Ollama => {
            let parsed = url::Url::parse(&endpoint.base_url).map(|u| u.port().unwrap_or(80));
            parsed.unwrap_or(80)
        }
    };

    let live_counter = server.as_ref().map(mock_server::MockServer::live_counter);
    let live_ref = live_counter.as_ref();

    eprintln!("--- Spike 2: HTTP Stream Cancellation ---");
    eprintln!(
        "endpoint={:?} cancel-mode={:?} frame-cap={:?} socket-port={socket_port}",
        options.endpoint, options.cancel_mode, options.frame_cap
    );
    eprintln!(
        "base-url={} model={} prompt={PROMPT:?}",
        endpoint.base_url, endpoint.model
    );
    eprintln!(
        "note: close latency for remote endpoints reflects socket-table disappearance (no server counter);\n      single runs stream until the cancel offset, so real models take time in proportion to their token rate."
    );

    let baseline_tasks = verify::alive_tasks();
    let mut all_failures: Vec<VerifyFailure> = Vec::new();

    // ---- 1. Single-offset repeated run (--cancel-at / --iterations) ----
    let mut single_results: Vec<Duration> = Vec::new();
    let offset = resolve_offset(options.cancel_at, options.frame_cap);
    eprintln!(
        "--- single-offset run ({} iterations, cancel at event {offset}) ---",
        options.iterations,
    );
    for iteration in 0..options.iterations {
        let result = run_iteration(
            &endpoint,
            options.cancel_mode,
            offset,
            live_ref,
            socket_port,
            format!("single[{iteration}]"),
        )
        .await;
        verify::accumulate(&result, &mut all_failures, socket_tools_available);
        eprintln!(
            "  {} reason={} events={} close={} accumulated={:?}",
            result.label,
            result.break_reason,
            result.events,
            format_latency(&result.close.close_latency),
            snippet(&result.accumulated),
        );
        if options.print_events > 0 && !result.received.is_empty() {
            let shown = options.print_events.min(result.received.len());
            eprintln!(
                "    received (showing {shown} of {}):",
                result.received.len()
            );
            for (index, data) in result.received.iter().take(shown).enumerate() {
                eprintln!("      [{index}] {data}");
            }
        }
        single_results.push(result.close.close_latency);
    }
    let mut single_sorted = single_results.clone();
    single_sorted.sort();
    eprintln!(
        "single-offset (offset={offset}, n={}): mean {} | p90 {}",
        options.iterations,
        format_latency(&mean(&single_results)),
        format_latency(&p90(&single_sorted)),
    );

    // ---- 2. Sweep across fractions ----
    eprintln!(
        "--- sweep across fractions ({} iterations each) ---",
        options.iterations
    );
    let mut sweep_results: Vec<Duration> = Vec::new();
    for fraction in &options.sweep_fractions {
        let sweep_offset =
            client::event_offset(*fraction, options.frame_cap.unwrap_or(DEFAULT_FRAME_CAP));
        for iteration in 0..options.iterations {
            let result = run_iteration(
                &endpoint,
                options.cancel_mode,
                sweep_offset,
                live_ref,
                socket_port,
                format!("sweep@{fraction}[{iteration}]"),
            )
            .await;
            verify::accumulate(&result, &mut all_failures, socket_tools_available);
            eprintln!(
                "  {} done reason={} events={} close={}",
                result.label,
                result.break_reason,
                result.events,
                format_latency(&result.close.close_latency),
            );
            sweep_results.push(result.close.close_latency);
        }
    }
    let mut sweep_sorted = sweep_results.clone();
    sweep_sorted.sort();
    eprintln!(
        "sweep (fractions={:?}): mean {} | p90 {}",
        options.sweep_fractions,
        format_latency(&mean(&sweep_results)),
        format_latency(&p90(&sweep_sorted)),
    );

    // ---- 3. Direct-drop vs token-abort comparison table ----
    if let EndpointKind::Mock = options.endpoint {
        eprintln!("--- direct-drop vs token-abort comparison ---");
        let mut table_rows: Vec<(String, String, String, String)> = Vec::new();
        for (mode_label, mode) in [
            ("token", CancelMode::Token),
            (
                "drop:adapter-only",
                CancelMode::Drop(DropSubCase::AdapterOnly),
            ),
            (
                "drop:adapter+response",
                CancelMode::Drop(DropSubCase::AdapterAndResponse),
            ),
        ] {
            let compare_offset =
                client::event_offset(0.5, options.frame_cap.unwrap_or(DEFAULT_FRAME_CAP));
            let mut latencies: Vec<Duration> = Vec::new();
            let mut closed = 0usize;
            for iteration in 0..3 {
                let result = run_iteration(
                    &endpoint,
                    mode,
                    compare_offset,
                    live_ref,
                    socket_port,
                    format!("compare:{mode_label}[{iteration}]"),
                )
                .await;
                verify::accumulate(&result, &mut all_failures, socket_tools_available);
                latencies.push(result.close.close_latency);
                if !result.close.timed_out && result.close.sockets_clean {
                    closed += 1;
                }
            }
            let mut sorted = latencies.clone();
            sorted.sort();
            table_rows.push((
                mode_label.to_string(),
                format_latency(&mean(&latencies)),
                format_latency(&p90(&sorted)),
                format!("{closed}/{}", latencies.len()),
            ));
        }
        for (label, mean, p90, closed) in &table_rows {
            eprintln!("  {label:<22} mean {mean} | p90 {p90} | closed {closed}");
        }
    }

    // ---- 4. Token race sweep (hundreds of iterations) ----
    if options.race_sweep && matches!(options.cancel_mode, CancelMode::Token) {
        eprintln!("--- token race sweep ({}) ---", options.race_iterations);
        let race_offset = options.race_offset_events.min(
            options
                .frame_cap
                .unwrap_or(DEFAULT_FRAME_CAP)
                .saturating_sub(1),
        );
        let mut race_violations = 0usize;
        for iteration in 0..options.race_iterations {
            let result = run_iteration(
                &endpoint,
                CancelMode::Token,
                race_offset,
                live_ref,
                socket_port,
                format!("race[{iteration}]"),
            )
            .await;
            verify::accumulate(&result, &mut all_failures, socket_tools_available);
            // Tolerant grace window: at most one in-flight chunk may be delivered
            // after the cancel is observed before the stream stops.
            if result.events_after_trigger > 1 {
                race_violations += 1;
            }
        }
        eprintln!(
            "  race runs={} violations(>1 post-cancel events)= {race_violations}",
            options.race_iterations
        );
    }

    let final_tasks = verify::alive_tasks();

    if let Some(server) = &server {
        eprintln!(
            "mock server: cumulative connections = {}, live now = {}",
            server.cumulative_connections(),
            server.live_connections(),
        );
    }

    // Stop the mock accept loop so the runtime can shut down promptly.
    if let Some(server) = server {
        server.abort();
    }

    // ---- Criterion summary + exit code ----
    let closed_promptly = !all_failures
        .iter()
        .any(|failure| matches!(failure, VerifyFailure::LingeringConnection));
    let zero_leaked = final_tasks <= baseline_tasks;
    let race_clean = !all_failures
        .iter()
        .any(|failure| matches!(failure, VerifyFailure::CancelRace));

    let ok = closed_promptly && zero_leaked && race_clean;

    eprintln!("--- pass/fail ---");
    eprintln!(
        "[{}] no lingering connection (adapter/response drop closes the socket)",
        if closed_promptly { "PASS" } else { "FAIL" }
    );
    eprintln!(
        "[{}] zero leaked tasks (baseline={baseline_tasks} -> final={final_tasks})",
        if zero_leaked { "PASS" } else { "FAIL" }
    );
    eprintln!(
        "[{}] no CancellationToken race (<=1 in-flight chunk after cancel)",
        if race_clean { "PASS" } else { "FAIL" }
    );

    if ok { 0 } else { 1 }
}

fn mean(values: &[Duration]) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let total: f64 = values.iter().map(|value| value.as_secs_f64()).sum();
    Duration::from_secs_f64(total / values.len() as f64)
}

fn snippet(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= 48 {
        trimmed.to_string()
    } else {
        format!("{}…", trimmed.chars().take(48).collect::<String>())
    }
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let code = runtime.block_on(run(options));
    // Secondary clean-shutdown signal: waits up to 5s for tasks to finish (see
    // docs/blueprint.md §Spike 2). A hang here would block exit and fail a CI timeout.
    runtime.shutdown_timeout(Duration::from_secs(5));

    std::process::exit(code);
}
