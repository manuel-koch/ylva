use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Spawns an artificial token-generator task.
///
/// The task emits `rate_per_sec` text chunks per second over a **bounded**
/// `mpsc::Sender<String>`. If the TUI falls behind, `send().await` throttles the
/// generator (backpressure), which keeps memory flat even under overload.
///
/// The live queue depth is exposed through an atomic counter that the producer
/// increments on send and the consumer decrements on receive, so the draw loop
/// can report steady-state channel depth.
#[allow(clippy::too_many_arguments)]
pub fn spawn_generator(
    tx: mpsc::Sender<String>,
    rate_per_sec: usize,
    cancel: CancellationToken,
    queue_depth: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let rate = rate_per_sec.max(1) as u64;
        let step_nanos = 1_000_000_000u64 / rate;
        let mut interval = tokio::time::interval(Duration::from_nanos(step_nanos));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sequence: u64 = 0;

        // Skip the immediate first tick so tokens are spread over the run.
        interval.tick().await;

        'generator: loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    sequence += 1;
                    let chunk = format!("tok{s:04} the quick brown fox jumps over the lazy dog", s = sequence);
                    queue_depth.fetch_add(1, Ordering::SeqCst);
                    // Send is itself select!ed against cancel so a full channel
                    // (backpressure) cannot deadlock cancellation/steering.
                    let mut send = Box::pin(tx.send(chunk));
                    let outcome = tokio::select! {
                        res = &mut send => res,
                        _ = cancel.cancelled() => {
                            queue_depth.fetch_sub(1, Ordering::SeqCst);
                            break 'generator;
                        }
                    };
                    match outcome {
                        Ok(_) => {}
                        Err(_) => {
                            // Consumer dropped the channel; stop generating.
                            queue_depth.fetch_sub(1, Ordering::SeqCst);
                            break 'generator;
                        }
                    }
                }
            }
        }
    })
}
