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

//! Build provenance of the compiled binary, captured at compile time by
//! [`build.rs`](crate root `build.rs`) and surfaced on demand by the CLI
//! (`--version`) and, in Phase 4, the TUI's "About" dialog. This struct is the
//! single source of truth (SSOT) for `version`, `git_commit`,
//! `build_timestamp_utc`, and `profile`.
//!
//! The values are read from compile-time env vars injected by `build.rs` via
//! `cargo:rustc-env`:
//!
//! - `YLVA_GIT_COMMIT` — short (8-char) SHA-1 of `HEAD` at compile time, or
//!   `"unknown"` when not built in a git checkout.
//! - `YLVA_BUILD_TIMESTAMP_UTC` — UTC compile time, `%Y-%m-%dT%H:%M:%SZ`.
//! - `YLVA_PROFILE` — `debug` or `release`.
//!
//! `[env!]` is used so a missing value is a compile error rather than a silent
//! `""`; only the git-commit fallback is legitimate.
//!
//! **TUI contract (Phase 4):** when the TUI lands (blueprint §7 Phase 4), a
//! dedicated keybinding will render [`BUILD_INFO`] in an "About" popup. The
//! contract here is final; the TUI keybinding itself is Phase 4 work.

/// Provenance of the compiled binary. All fields are compile-time constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    /// Package version from `Cargo.toml` (`CARGO_PKG_VERSION`).
    pub version: &'static str,
    /// Short (8-char) SHA-1 of `HEAD` at compile time, or `"unknown"`.
    pub git_commit: &'static str,
    /// UTC compile time in `%Y-%m-%dT%H:%M:%SZ` shape.
    pub build_timestamp_utc: &'static str,
    /// Build profile: `debug` or `release`.
    pub profile: &'static str,
}

/// The provenance of this specific build, as captured at compile time. Use this
/// in any const context; a plain function could not be used in `const` code.
pub const BUILD_INFO: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    git_commit: env!("YLVA_GIT_COMMIT"),
    build_timestamp_utc: env!("YLVA_BUILD_TIMESTAMP_UTC"),
    profile: env!("YLVA_PROFILE"),
};

impl BuildInfo {
    /// Canonical multi-line representation: a one-line header followed by one
    /// field per line in the stable order `version` / `commit` / `built` /
    /// `profile`. Field labels and alignment are stable for scripts to parse.
    pub fn render(&self) -> String {
        format!(
            "Ylva - Agent-Harness\nVersion: v{}\nCommit:  {}\nBuilt:   {}\nProfile: {}",
            self.version, self.git_commit, self.build_timestamp_utc, self.profile
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BuildInfo {
        BuildInfo {
            version: "9.9.9",
            git_commit: "01234567",
            build_timestamp_utc: "2026-08-13T16:45:00Z",
            profile: "debug",
        }
    }

    #[test]
    fn render_emits_stable_field_order_and_labels() {
        let rendered = fixture().render();
        assert_eq!(
            rendered,
            "Ylva - Agent-Harness\nVersion: v9.9.9\nCommit:  01234567\nBuilt:   2026-08-13T16:45:00Z\nProfile: debug"
        );
        assert!(rendered.starts_with("Ylva - Agent-Harness\n"));
        assert!(rendered.contains("\nVersion: v"));
        assert!(rendered.contains("\nCommit:  "));
        assert!(rendered.contains("\nBuilt:   "));
        assert!(rendered.contains("\nProfile: "));
    }

    #[test]
    fn fixture_is_copy_and_eq() {
        let left = fixture();
        let right = fixture();
        assert_eq!(left, right);
        let copy = left;
        assert_eq!(copy, right);
    }
}
