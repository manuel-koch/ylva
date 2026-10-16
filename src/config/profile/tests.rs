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
use std::path::Path;

use crate::config::{profile_env_path, PROFILE_ENV_FILE_NAME};
use crate::highlight::ThemeName;

use super::*;
use crate::agent::compaction::{
    DEFAULT_COMPACTION_TIMEOUT_SECS, DEFAULT_HARD_COMPACTION_THRESHOLD,
    DEFAULT_SOFT_COMPACTION_THRESHOLD,
};

/// Write `contents` as `<root>/<profile_name>/config.toml`.
fn write_profile_config(root: &Path, profile_name: &str, contents: &str) {
    let path = super::super::profile_config_path(root, profile_name);
    std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("mkdir");
    std::fs::write(&path, contents).expect("write the profile config");
}

fn write_agent_type_file(root: &Path, relative_path: &str, contents: &str) {
    let path = root.join(relative_path);
    std::fs::create_dir_all(path.parent().expect("an agent file parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write the agent-type file");
}

// ── `network_request` rule-kind validation ────────────────────────────────

// ── `[web_fetch]` section ─────────────────────────────────────────────────

// ── `[web_search]` section ────────────────────────────────────────────────

mod backends;
mod loading;
mod profile_name;
mod sandbox;
mod validation;
