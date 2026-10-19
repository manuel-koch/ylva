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

//! Headless (non-terminal) runner: the original `println!`-based host for
//! `AgentLoop`. This is the path used when stdout is not a terminal.

mod console_app;
mod invocation_render;
mod output;
mod run;
mod title_driver;

#[cfg(test)]
mod tests;

pub use console_app::ConsoleApp;
pub use output::CliOutputOptions;
pub use run::{run_cli, run_cli_to, run_cli_to_with_options};
