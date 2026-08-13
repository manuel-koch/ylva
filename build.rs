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

// Captures build provenance at compile time and injects it into the binary via
// `cargo:rustc-env`. The binary reads the values with `env!` (see
// `src/build_info.rs`). The env var names are the contract:
//
//   YLVA_GIT_COMMIT         Short (8-char) SHA-1 of HEAD, or "unknown"
//   YLVA_BUILD_TIMESTAMP_UTC UTC compile time as "%Y-%m-%dT%H:%M:%SZ"
//   YLVA_PROFILE             Cargo build profile ("debug"/"release")

use std::env;
use std::process::Command;
use time::macros::format_description;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let git_commit = git_short_commit_hash();
    println!("cargo:rustc-env=YLVA_GIT_COMMIT={git_commit}");

    let utc_format = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    let build_timestamp_utc = time::OffsetDateTime::now_utc()
        .format(&utc_format)
        .expect("UTC timestamp formats with the fixed description");
    println!("cargo:rustc-env=YLVA_BUILD_TIMESTAMP_UTC={build_timestamp_utc}");

    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=YLVA_PROFILE={profile}");
}

/// Resolve the short (8-char) SHA-1 of `HEAD`, falling back to the literal
/// `"unknown"` when `git` is unavailable or the working directory is not a git
/// checkout (e.g. a `cargo install` from a source tarball). A build script
/// must not panic here — a panicking build script fails the whole `cargo build`.
fn git_short_commit_hash() -> String {
    match Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => "unknown".to_string(),
    }
}
