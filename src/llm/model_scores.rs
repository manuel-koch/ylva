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

//! User-maintained model scores from `model-scores.toml` in the Ylva home.
//!
//! A score is a unitless, user-chosen rating shown in the `/models` picker.
//! It is not a measurement: Ylva neither validates its scale nor converts it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// File name of the scores file inside the Ylva home.
pub const MODEL_SCORES_FILE_NAME: &str = "model-scores.toml";

/// Lifetime of a loaded scores file, matching the `/models` catalog cache.
const MODEL_SCORES_CACHE_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// The rating of one model: a single value or one value per reasoning effort.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelScore {
    Plain(f64),
    PerEffort {
        default: Option<f64>,
        /// Lowercased effort name to value, ordered by name.
        efforts: BTreeMap<String, f64>,
    },
}

// Sound because the loader only admits finite values (no NaN).
impl Eq for ModelScore {}

impl ModelScore {
    /// Smallest and largest value over all values including `default`.
    pub fn range(&self) -> (f64, f64) {
        let values: Vec<f64> = match self {
            Self::Plain(value) => vec![*value],
            Self::PerEffort { default, efforts } => {
                default.iter().chain(efforts.values()).copied().collect()
            }
        };
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (minimum, maximum)
    }

    /// The value for `effort`, falling back to `default`; `None` when neither exists.
    pub fn for_effort(&self, effort: Option<&str>) -> Option<f64> {
        match self {
            Self::Plain(value) => Some(*value),
            Self::PerEffort { default, efforts } => effort
                .and_then(|effort| efforts.get(&effort.to_lowercase()).copied())
                .or(*default),
        }
    }

    /// List-row text: the value, or `minimum–maximum` when values differ.
    pub fn summary(&self) -> String {
        let (minimum, maximum) = self.range();
        if minimum == maximum {
            format_score_value(minimum)
        } else {
            format!(
                "{}–{}",
                format_score_value(minimum),
                format_score_value(maximum)
            )
        }
    }
}

/// Format a score without trailing zeros.
pub fn format_score_value(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Scores keyed by normalized model name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelScores {
    entries: BTreeMap<String, ModelScore>,
}

impl ModelScores {
    /// The score for a catalog model id, matched exactly on the normalized name.
    pub fn lookup(&self, model_id: &str) -> Option<&ModelScore> {
        self.entries.get(&normalize_model_name(model_id))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Outcome of reading the scores file; `problems` are human-readable and
/// never fatal unless `--strict-config` is set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelScoreLoad {
    pub scores: ModelScores,
    pub problems: Vec<String>,
}

/// Normalize a model name for score matching.
///
/// Lowercases; drops the vendor prefix up to the last `/`, a `:suffix`
/// routing tag, an `@…` suffix, a leading dotted `vendor.` prefix (letters
/// only, as in Bedrock ids such as `anthropic.claude-…`) and a trailing `-` plus 4, 6 or 8 digits or an
/// ISO date; treats `.` and `-` between digits as equal.
pub fn normalize_model_name(name: &str) -> String {
    static TRAILING_DATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"-(\d{4}-\d{2}-\d{2}|\d{8}|\d{6}|\d{4})$").expect("valid regex")
    });
    static DIGIT_DOT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(\d)\.(\d)").expect("valid regex"));
    static DOTTED_VENDOR: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[a-z]+\.([a-z])").expect("valid regex"));
    let lowered = name.trim().to_lowercase();
    let without_vendor = lowered.rsplit('/').next().unwrap_or(&lowered);
    let without_tag = without_vendor.split(':').next().unwrap_or(without_vendor);
    let without_suffix = without_tag.split('@').next().unwrap_or(without_tag);
    let without_dotted_vendor = DOTTED_VENDOR.replace(without_suffix, "$1");
    let without_date = TRAILING_DATE.replace(&without_dotted_vendor, "");
    // Applied twice so overlapping matches such as `1.2.3` are fully replaced.
    let once = DIGIT_DOT.replace_all(&without_date, "$1-$2").into_owned();
    DIGIT_DOT.replace_all(&once, "$1-$2").into_owned()
}

fn parse_score_number(value: &toml::Value) -> Option<f64> {
    let number = match value {
        toml::Value::Integer(integer) => *integer as f64,
        toml::Value::Float(float) => *float,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn parse_score(value: &toml::Value) -> Result<ModelScore, String> {
    if let Some(number) = parse_score_number(value) {
        return Ok(ModelScore::Plain(number));
    }
    let toml::Value::Table(table) = value else {
        return Err("expected a finite number or a table of effort to number".to_string());
    };
    if table.is_empty() {
        return Err("empty effort table".to_string());
    }
    let mut default = None;
    let mut efforts = BTreeMap::new();
    for (effort, effort_value) in table {
        let number = parse_score_number(effort_value)
            .ok_or_else(|| format!("effort `{effort}` is not a finite number"))?;
        if effort == "default" {
            default = Some(number);
        } else {
            efforts.insert(effort.to_lowercase(), number);
        }
    }
    Ok(ModelScore::PerEffort { default, efforts })
}

/// Parse the text of a scores file. Invalid entries are skipped and reported.
pub fn parse_model_scores(text: &str) -> ModelScoreLoad {
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(error) => {
            return ModelScoreLoad {
                scores: ModelScores::default(),
                problems: vec![format!("not valid TOML: {error}")],
            }
        }
    };
    let mut load = ModelScoreLoad::default();
    let mut key_by_normalized_name: BTreeMap<String, &str> = BTreeMap::new();
    // Keys iterate in sorted order, so "later entry wins" is deterministic.
    for (key, value) in &table {
        let normalized = normalize_model_name(key);
        match parse_score(value) {
            Ok(score) => {
                load.scores.entries.insert(normalized.clone(), score);
                if let Some(earlier_key) = key_by_normalized_name.insert(normalized.clone(), key) {
                    load.problems.push(format!(
                        "`{key}`: duplicate entry for normalized name `{normalized}` \
                         (also `{earlier_key}`); later entry wins"
                    ));
                }
            }
            Err(problem) => load.problems.push(format!("`{key}`: {problem}")),
        }
    }
    load
}

/// Read a scores file. A missing file is silent and yields no scores.
pub fn load_model_scores(path: &Path) -> ModelScoreLoad {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_model_scores(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ModelScoreLoad::default(),
        Err(error) => ModelScoreLoad {
            scores: ModelScores::default(),
            problems: vec![format!("cannot read file {}: {error}", path.display())],
        },
    }
}

struct CachedScores {
    path: PathBuf,
    loaded_at: Instant,
    load: Arc<ModelScoreLoad>,
}

static CACHE: Mutex<Option<CachedScores>> = Mutex::new(None);

/// The scores file of the Ylva home, cached for [`MODEL_SCORES_CACHE_LIFETIME`]
/// within a run.
///
/// Problems are logged once per load. `None` home means no scores.
pub fn cached_model_scores() -> Arc<ModelScoreLoad> {
    let Some(home) = crate::ylva_home::ylva_home() else {
        return Arc::default();
    };
    cached_model_scores_at(&home.join(MODEL_SCORES_FILE_NAME))
}

fn cached_model_scores_at(path: &Path) -> Arc<ModelScoreLoad> {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = cache.as_ref().filter(|cached| {
        cached.path == path && cached.loaded_at.elapsed() < MODEL_SCORES_CACHE_LIFETIME
    }) {
        return Arc::clone(&cached.load);
    }
    let load = Arc::new(load_model_scores(path));
    for problem in &load.problems {
        tracing::warn!(path = %path.display(), %problem, "model scores problem");
    }
    *cache = Some(CachedScores {
        path: path.to_path_buf(),
        loaded_at: Instant::now(),
        load: Arc::clone(&load),
    });
    load
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_follows_the_documented_rules() {
        let cases = [
            ("openai/gpt-5", "gpt-5"),
            ("OpenRouter/Anthropic/Claude-Opus-4.5", "claude-opus-4-5"),
            ("claude-opus-4-5", "claude-opus-4-5"),
            ("deepseek/deepseek-r1:free", "deepseek-r1"),
            ("qwen3:thinking", "qwen3"),
            ("claude-sonnet-4@eu", "claude-sonnet-4"),
            ("mistral-large-1017", "mistral-large"),
            ("gpt-4-0613", "gpt-4"),
            ("claude-sonnet-4-20250514", "claude-sonnet-4"),
            ("gpt-4o-2024-08-06", "gpt-4o"),
            ("model-123", "model-123"),
            ("gpt-5-mini", "gpt-5-mini"),
            ("llama-3.1.2", "llama-3-1-2"),
            ("amazon/anthropic.claude-haiku-5-5@eu", "claude-haiku-5-5"),
            ("openrouter/anthropic/claude-haiku-5.5", "claude-haiku-5-5"),
            ("meta.llama3-70b", "llama3-70b"),
            ("llama3.1", "llama3-1"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_model_name(input), expected, "input {input}");
        }
    }

    #[test]
    fn lookup_is_exact_on_the_normalized_name() {
        let load = parse_model_scores("\"anthropic/claude-opus-4.5\" = 90\n\"gpt-5\" = 80.5\n");
        assert!(load.problems.is_empty());
        assert_eq!(
            load.scores.lookup("claude-opus-4-5-20251101"),
            Some(&ModelScore::Plain(90.0))
        );
        assert_eq!(
            load.scores.lookup("openai/gpt-5:free").unwrap().summary(),
            "80.5"
        );
        assert_eq!(load.scores.lookup("gpt-5-mini"), None);
    }

    #[test]
    fn effort_maps_expose_range_default_and_per_effort_values() {
        let load = parse_model_scores("model = { low = 60, High = 90, default = 75 }\n");
        let score = load.scores.lookup("model").expect("score");
        assert_eq!(score.summary(), "60–90");
        assert_eq!(score.for_effort(Some("high")), Some(90.0));
        assert_eq!(score.for_effort(Some("medium")), Some(75.0));
        assert_eq!(score.for_effort(None), Some(75.0));
        let flat = parse_model_scores("model = { low = 70, high = 70 }\n");
        assert_eq!(flat.scores.lookup("model").unwrap().summary(), "70");
    }

    #[test]
    fn invalid_entries_are_reported_and_skipped() {
        let load = parse_model_scores(
            "good = 1\nstring = \"high\"\nflag = true\nnan = nan\nempty = {}\nbad = { low = \"x\" }\nneg = -2\n",
        );
        assert_eq!(load.scores.entries.len(), 2);
        assert_eq!(load.problems.len(), 5);
        assert_eq!(load.scores.lookup("neg").unwrap().summary(), "-2");
    }

    #[test]
    fn duplicate_normalized_keys_warn_and_the_later_entry_wins() {
        let load = parse_model_scores("\"a/model-1.5\" = 1\n\"model-1-5\" = 2\n");
        assert_eq!(
            load.problems,
            vec![
                "`model-1-5`: duplicate entry for normalized name `model-1-5` \
                 (also `a/model-1.5`); later entry wins"
                    .to_string()
            ]
        );
        assert_eq!(
            load.scores.lookup("model-1.5"),
            Some(&ModelScore::Plain(2.0))
        );
    }

    #[test]
    fn invalid_toml_and_missing_files_never_fail() {
        assert_eq!(parse_model_scores("= =").problems.len(), 1);
        let directory = tempfile::tempdir().expect("tempdir");
        let missing = load_model_scores(&directory.path().join(MODEL_SCORES_FILE_NAME));
        assert_eq!(missing, ModelScoreLoad::default());
        let unreadable = load_model_scores(directory.path());
        assert_eq!(unreadable.problems.len(), 1);
    }

    #[test]
    fn cached_scores_are_reused_until_the_path_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join(MODEL_SCORES_FILE_NAME);
        std::fs::write(&path, "model = 1\n").expect("write");
        let first = cached_model_scores_at(&path);
        std::fs::write(&path, "model = 2\n").expect("write");
        assert!(Arc::ptr_eq(&first, &cached_model_scores_at(&path)));
        let other = directory.path().join("other.toml");
        assert!(cached_model_scores_at(&other).scores.is_empty());
    }
}
