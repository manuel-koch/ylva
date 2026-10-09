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

//! The global subscriber installation contract runs alone in this test process.

use tempfile::TempDir;
use ylva::{init_logging, LogConfig, LogFormat};

/// `init_logging` installs the process-global `tracing` subscriber, which can
/// only happen once per test process. This test therefore exercises the real
/// entry point end-to-end: it must be the only test in this crate that calls
/// `init_logging`, so it can rely on the global default.
#[test]
fn init_logging_writes_selected_format_to_log_file() {
    let dir = TempDir::new().expect("tempdir");
    let text_path = dir.path().join("ylva.log");
    let guard = init_logging(LogConfig {
        log_file: Some(text_path.clone()),
        log_level: tracing::Level::DEBUG,
        log_format: LogFormat::Text,
    })
    .expect("log file opens");
    tracing::debug!(provider = "mock", "text format to log file");
    drop(guard);

    let text = std::fs::read_to_string(&text_path).expect("read text log");
    assert!(
        text.contains("text format to log file"),
        "text log was: {text}"
    );
    assert!(text.contains("provider=\"mock\""), "text log was: {text}");
}
