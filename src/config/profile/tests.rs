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
mod sandbox;
mod validation;
