use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod app;
mod events;
mod stream;
mod ui;

use app::{AgentCommand, App};

const RING_CAPACITY: usize = 1024;
const TOKEN_CHANNEL_CAPACITY: usize = 1024;
/// Maximum tokens drained per frame draw (Resolution 2 batching).
const BATCH_DRAIN_CAP: usize = 256;
/// How long after start the auto steering `Prompt` fires (seconds).
const STEER_AFTER_SECS: u64 = 3;

struct Options {
    duration_secs: u64,
    rate: usize,
}

fn parse_args() -> Options {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        duration_secs: 60,
        rate: 1000,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--duration" => {
                options.duration_secs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--duration needs a seconds value");
            }
            "--rate" => {
                options.rate = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--rate needs a msgs/sec value");
            }
            other => panic!("unknown argument: {other} (use --duration <secs> and --rate <n>)"),
        }
    }
    options
}

fn main() -> io::Result<()> {
    let options = parse_args();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(run(options))
}

async fn run(options: Options) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_tui(&mut terminal, options).await;
    ratatui::restore();
    result
}

async fn run_tui(terminal: &mut ratatui::DefaultTerminal, options: Options) -> io::Result<()> {
    let mut app = App::new(RING_CAPACITY);
    let queue_depth = Arc::new(AtomicUsize::new(0));

    let (token_tx, mut token_rx) = mpsc::channel::<String>(TOKEN_CHANNEL_CAPACITY);
    let mut event_rx = events::spawn_event_pump();

    let mut last_draw = Instant::now();
    let run_start = Instant::now();

    // Initial generator.
    let initial_cancel = CancellationToken::new();
    let tx = token_tx.clone();
    let qd = queue_depth.clone();
    let mut current_handle: Option<tokio::task::JoinHandle<()>> = Some(stream::spawn_generator(
        tx,
        options.rate,
        initial_cancel.clone(),
        qd,
    ));
    let mut current_cancel: Option<CancellationToken> = Some(initial_cancel);

    // Auto-steering demonstration: fire one Prompt shortly after start.
    let steer = tokio::time::sleep(std::time::Duration::from_secs(STEER_AFTER_SECS));
    tokio::pin!(steer);

    let mut quit = false;

    while !quit && run_start.elapsed().as_secs() < options.duration_secs {
        let mut action: Option<AgentCommand> = None;
        let mut redraw;

        tokio::select! {
            Some(chunk) = token_rx.recv() => {
                queue_depth.fetch_sub(1, Ordering::SeqCst);
                app.ring.push(chunk);
                let mut drained = 1;
                while drained < BATCH_DRAIN_CAP {
                    match token_rx.try_recv() {
                        Ok(more) => {
                            queue_depth.fetch_sub(1, Ordering::SeqCst);
                            app.ring.push(more);
                            drained += 1;
                        }
                        Err(_) => break,
                    }
                }
                app.record_queue_depth(queue_depth.load(Ordering::SeqCst));
                redraw = true;
            }
            Some(event) = event_rx.recv() => {
                if events::is_quit_event(&event) {
                    quit = true;
                } else {
                    let echo_char = match &event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                            KeyCode::Char(ch) => Some(ch),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(cmd) = events::handle_input_event(event, &mut app.input) {
                        action = Some(cmd);
                    } else if let Some(ch) = echo_char {
                        app.record_key(ch.to_string());
                    }
                }
                redraw = true;
            }
            _ = &mut steer => {                action = Some(AgentCommand::Prompt(format!("auto-steer@{STEER_AFTER_SECS}s")));
                redraw = true;
            }
        }

        if let Some(cmd) = action {
            match cmd {
                AgentCommand::Stop => {
                    if let Some(cancel) = current_cancel.take() {
                        cancel.cancel();
                    }
                    if let Some(handle) = current_handle.take() {
                        let _ = handle.await;
                    }
                    app.streaming = false;
                    app.record_key("Stop".into());
                    redraw = true;
                }
                AgentCommand::Prompt(text) => {
                    // Cancel the old stream and await its completion.
                    if let Some(cancel) = current_cancel.take() {
                        cancel.cancel();
                    }
                    if let Some(handle) = current_handle.take() {
                        let _ = handle.await;
                    }
                    // Flush/replace the buffer marker and start a fresh generator.
                    app.streaming = true;
                    app.ring.push(if text.is_empty() {
                        ">>> (steer)".to_string()
                    } else {
                        format!(">>> {text}")
                    });
                    let cancel = CancellationToken::new();
                    current_cancel = Some(cancel.clone());
                    let tx = token_tx.clone();
                    let qd = queue_depth.clone();
                    current_handle = Some(stream::spawn_generator(tx, options.rate, cancel, qd));
                    app.record_key(format!("Prompt:{text}"));
                    redraw = true;
                }
            }
        }

        if redraw {
            let now = Instant::now();
            let frame_millis = now.duration_since(last_draw).as_secs_f64() * 1000.0;
            last_draw = now;
            app.frame_time.record(frame_millis);
            terminal.draw(|frame| ui::draw(frame, &app, options.duration_secs))?;
        }
    }

    // Clean shutdown: cancel any in-flight generator.
    if let Some(cancel) = current_cancel.take() {
        cancel.cancel();
    }
    if let Some(handle) = current_handle.take() {
        let _ = handle.await;
    }

    eprintln!("--- Spike 1 summary ---");
    eprintln!(
        "frames: {} ({} >17ms) | frame avg {:.2}ms min {}ms max {}ms",
        app.frame_time.total_frames(),
        app.frame_time.frames_over_17ms(),
        app.frame_time.average_millis(),
        app.frame_time
            .min_millis()
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "-".into()),
        app.frame_time
            .max_millis()
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "-".into()),
    );
    eprintln!(
        "frame histogram [<8 {} |8-16 {} |16-33 {} |>33 {}]",
        app.frame_time.histogram()[0],
        app.frame_time.histogram()[1],
        app.frame_time.histogram()[2],
        app.frame_time.histogram()[3],
    );
    eprintln!(
        "queue: peak depth {} (target <= ~20) | ring: {} entries, peak {:.2} KB",
        app.peak_queue_depth,
        app.ring.len(),
        app.ring.peak_bytes() as f64 / 1024.0,
    );
    eprintln!("last keys: {:?}", app.last_keys);
    Ok(())
}
