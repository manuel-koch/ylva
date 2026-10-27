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

//! External stop signals for the CLI and the TUI.
//!
//! SIGINT and SIGTERM both ask the run to stop. SIGHUP is not a stop
//! request: the TUI restores the terminal and exits on it (see
//! `tui_driver::terminal`); the CLI does not handle it.

use tokio::signal::unix::{signal, Signal, SignalKind};

/// Receives SIGINT and SIGTERM. Install it before the run starts, so an early
/// signal does not fall through to the default action (process termination).
pub(crate) struct StopSignals {
    interrupt: Signal,
    terminate: Signal,
}

impl StopSignals {
    /// Register both handlers. Must be called inside a Tokio runtime.
    pub(crate) fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Resolve with `Some(())` for each SIGINT or SIGTERM, or with `None` once
    /// the signal streams end.
    pub(crate) async fn recv(&mut self) -> Option<()> {
        tokio::select! {
            received = self.interrupt.recv() => received,
            received = self.terminate.recv() => received,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::{kill, Signal as NixSignal};
    use nix::unistd::Pid;
    use std::time::Duration;

    async fn assert_receives(nix_signal: NixSignal) {
        let mut stop_signals = StopSignals::install().expect("install handlers");
        kill(Pid::this(), nix_signal).expect("send signal to this process");
        let received = tokio::time::timeout(Duration::from_secs(5), stop_signals.recv())
            .await
            .expect("signal was not delivered");
        assert_eq!(received, Some(()));
    }

    #[tokio::test]
    async fn sigint_is_received() {
        assert_receives(NixSignal::SIGINT).await;
    }

    #[tokio::test]
    async fn sigterm_is_received() {
        assert_receives(NixSignal::SIGTERM).await;
    }
}
