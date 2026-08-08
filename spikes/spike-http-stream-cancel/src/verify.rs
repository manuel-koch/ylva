use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How long we keep polling for a connection/socket to disappear before calling
/// the close "slow/lingering".
pub const CLOSE_AWAIT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long we keep confirming socket cleanliness after the server releases.
const SOCKET_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct SocketToolState {
    lsof: bool,
    netstat: bool,
}

fn socket_tool_state() -> &'static SocketToolState {
    static STATE: OnceLock<SocketToolState> = OnceLock::new();
    STATE.get_or_init(|| {
        let lsof = probe("lsof", &["-v"]);
        let netstat = probe("netstat", &["-an"]);
        if !lsof && !netstat {
            eprintln!(
                "warning: neither `lsof` nor `netstat` is available; socket checks will be skipped"
            );
        }
        SocketToolState { lsof, netstat }
    })
}

fn probe(tool: &str, args: &[&str]) -> bool {
    Command::new(tool)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// True when at least one OS socket-inspection tool is available.
pub fn socket_tools_available() -> bool {
    let state = socket_tool_state();
    state.lsof || state.netstat
}

/// Result of waiting for a connection to close after a cancel trigger.
#[derive(Debug)]
pub struct AwaitCloseOutcome {
    /// Elapsed time from the cancel trigger until close was observed.
    pub close_latency: Duration,
    /// True when the server-side live-connection counter returned to zero.
    pub server_released: bool,
    /// True when no `ESTABLISHED`/`CLOSE_WAIT` socket entry remains for the port.
    pub sockets_clean: bool,
    /// True when the wait hit [`CLOSE_AWAIT_TIMEOUT`] before closing fully.
    pub timed_out: bool,
}

/// A criterion violation discovered by [`accumulate`].
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyFailure {
    /// A connection/socket lingered after a cancel or drop was triggered.
    LingeringConnection,
    /// The stream delivered more than the tolerated in-flight chunk after cancel.
    CancelRace,
    /// A run never triggered the canceller (stream ended or errored early).
    NeverTriggered,
}

/// Apply the acceptance-checks for one iteration against the failure list.
///
/// Tolerates `socket_tools_available == false` by not failing on the socket
/// check (the tool is out of the spike's control). The `events_after_trigger`
/// field comes from the client and drives the race assertion.
pub fn accumulate(
    result: &crate::IterationResult,
    failures: &mut Vec<VerifyFailure>,
    socket_tools_available: bool,
) {
    if result.events_after_trigger > 1 {
        failures.push(VerifyFailure::CancelRace);
    }
    if !result.close.server_released {
        failures.push(VerifyFailure::LingeringConnection);
    }
    if socket_tools_available && !result.close.sockets_clean {
        failures.push(VerifyFailure::LingeringConnection);
    }
    if result.close.timed_out {
        failures.push(VerifyFailure::NeverTriggered);
    }
}

/// Number of live (alive) tasks on the current tokio runtime.
pub fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

/// Inspect the socket table (`lsof` then `netstat`) for a given port and return
/// every line that shows a live (ESTABLISHED / CLOSE_WAIT) connection for it.
/// Absent tools are skipped silently (the availability warning is emitted once).
pub fn lingering_socket_lines(port: u16) -> Vec<String> {
    let state = socket_tool_state();
    let mut findings = Vec::new();

    if state.lsof
        && let Ok(output) = Command::new("lsof")
            .args(["-i", &format!(":{port}")])
            .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if is_lingering_line(line) {
                findings.push(format!("lsof: {line}"));
            }
        }
    }

    if state.netstat
        && let Ok(output) = Command::new("netstat").args(["-an"]).output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains(&format!(":{port}")) && is_lingering_line(line) {
                findings.push(format!("netstat: {line}"));
            }
        }
    }

    findings
}

/// True when a socket-table line represents a live (ESTABLISHED / CLOSE_WAIT)
/// connection rather than a closed/listening one.
fn is_lingering_line(line: &str) -> bool {
    line.contains("ESTABLISHED") || line.contains("CLOSE_WAIT")
}

/// Poll until the server reports zero live connections and no lingering socket
/// entry remains for `port`, or until the timeouts are hit.
///
/// For a local mock the **close latency** is measured from the server-side
/// live-connection counter (an atomic read, so it reflects the real TCP close
/// with sub-millisecond precision); socket cleanliness is then confirmed with a
/// separate bounded settle check. For a remote endpoint (no live counter) the
/// close latency is instead measured by polling the socket table for
/// disappearance.
pub async fn await_close(
    triggered_at: Instant,
    live_counter: Option<&Arc<AtomicUsize>>,
    port: u16,
) -> AwaitCloseOutcome {
    match live_counter {
        Some(counter) => {
            let close_latency;
            let mut server_released = false;
            loop {
                if counter.load(Ordering::SeqCst) == 0 {
                    server_released = true;
                    close_latency = triggered_at.elapsed();
                    break;
                }
                if triggered_at.elapsed() >= CLOSE_AWAIT_TIMEOUT {
                    return AwaitCloseOutcome {
                        close_latency: triggered_at.elapsed(),
                        server_released,
                        sockets_clean: false,
                        timed_out: true,
                    };
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            let sockets_clean = await_socket_clean(port, SOCKET_SETTLE_TIMEOUT).await;
            AwaitCloseOutcome {
                close_latency,
                server_released,
                sockets_clean,
                timed_out: false,
            }
        }
        None => {
            let mut sockets_clean = lingering_socket_lines(port).is_empty();
            let mut close_latency = Duration::ZERO;
            while !sockets_clean {
                close_latency = triggered_at.elapsed();
                sockets_clean = lingering_socket_lines(port).is_empty();
                if close_latency >= CLOSE_AWAIT_TIMEOUT {
                    return AwaitCloseOutcome {
                        close_latency,
                        server_released: true,
                        sockets_clean,
                        timed_out: true,
                    };
                }
                tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
            }
            AwaitCloseOutcome {
                close_latency,
                server_released: true,
                sockets_clean,
                timed_out: false,
            }
        }
    }
}

/// Poll the socket table until no lingering entry remains or `timeout` elapses.
/// Returns true when the socket table reports clean.
async fn await_socket_clean(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    let mut sockets_clean = lingering_socket_lines(port).is_empty();
    while !sockets_clean && start.elapsed() < timeout {
        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
        sockets_clean = lingering_socket_lines(port).is_empty();
    }
    sockets_clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lingering_line_detection_is_state_based() {
        assert!(is_lingering_line(
            "tcp4 0 0 127.0.0.1:8080 127.0.0.1:9012 ESTABLISHED"
        ));
        assert!(is_lingering_line("tcp4 ... CLOSE_WAIT ..."));
        assert!(!is_lingering_line("tcp4 ... LISTEN ..."));
        assert!(!is_lingering_line("tcp4 ... TIME_WAIT ..."));
    }

    #[tokio::test]
    async fn active_tasks_counts_spawned_tasks() {
        let before = alive_tasks();
        let handle = tokio::spawn(async {});
        assert!(alive_tasks() > before);
        handle.await.expect("spawned task completes");
    }
}
