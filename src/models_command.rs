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

//! Read-only model catalogs for every connection in the selected profile.
//! No session, stdin read, model selection or configuration write is involved.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

use crate::agent_factory::AgentFactory;
use crate::llm::engine::{Engine, ProviderModelOption};
use crate::llm::{ModelScores, ReasoningEvidence};
use crate::skills::terminal_safe;
use crate::startup::engines::{
    apply_provider_env_to_profile, engines_from_environment, load_provider_env,
};
use crate::startup::profile::{
    load_profile, load_selected_profile_environment, trusted_profile_workspace_root,
};

/// The same whole-request budget used by the `/models` picker.
const MODEL_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type FavoriteRanks = HashMap<(String, String), usize>;

/// Presentation settings independent of process terminal/environment detection.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelsListOptions {
    /// Bold the terminal-safe model identity when enabled.
    pub color: bool,
    /// List every model the provider advertises, skipping `allowed_models`.
    pub include_disallowed: bool,
    /// List only models marked as favorites.
    pub only_favorites: bool,
    /// List the profile's configured engines instead of provider catalogs.
    pub only_engines: bool,
}

struct ConnectionCatalog {
    provider_connection: String,
    result: Result<Vec<ProviderModelOption>, String>,
}

/// The catalog the provider advertises, without the `allowed_models` filter
/// that [`Engine::list_model_options_with_failures`] applies.
async fn list_unfiltered_model_options(
    engine: &dyn Engine,
) -> anyhow::Result<(Vec<ProviderModelOption>, Vec<String>)> {
    let provider_connection = engine.config().provider_connection;
    let options = engine
        .list_models()
        .await?
        .into_iter()
        .map(|model| ProviderModelOption {
            context_length_override: None,
            max_output_tokens_override: None,
            provider_connection: provider_connection.clone(),
            model,
            reasoning_effort: None,
        })
        .collect();
    Ok((options, Vec::new()))
}

async fn fetch_connection_catalog(
    provider_connection: &str,
    engine: Arc<dyn Engine>,
    include_disallowed: bool,
) -> ConnectionCatalog {
    // Provider errors can contain credentials, URLs or response bodies. Do not
    // display or log them; only the connection identity and failure class are safe.
    let connection = terminal_safe(provider_connection);
    let listing = async {
        if include_disallowed {
            list_unfiltered_model_options(engine.as_ref()).await
        } else {
            engine.list_model_options_with_failures().await
        }
    };
    let result = match tokio::time::timeout(MODEL_LIST_TIMEOUT, listing).await {
        Ok(Ok((_, failures))) if !failures.is_empty() => Err(format!(
            "provider connection '{connection}' failed to list models"
        )),
        Ok(Ok((models, _))) => Ok(models),
        Ok(Err(_)) => Err(format!(
            "provider connection '{connection}' failed to list models"
        )),
        Err(_) => Err(format!(
            "provider connection '{connection}' timed out after {}s",
            MODEL_LIST_TIMEOUT.as_secs()
        )),
    };
    ConnectionCatalog {
        provider_connection: provider_connection.to_string(),
        result,
    }
}

fn favorite_ranks(engines: &[(String, Arc<dyn Engine>)]) -> FavoriteRanks {
    let mut ranks = HashMap::new();
    for (_, engine) in engines {
        for (connection, favorites) in engine.favorite_models() {
            for (index, model_id) in favorites.into_iter().enumerate() {
                if engine
                    .ensure_provider_model_allowed(&connection, &model_id)
                    .is_ok()
                {
                    ranks.entry((connection.clone(), model_id)).or_insert(index);
                }
            }
        }
    }
    ranks
}

fn prepare_catalog(catalog: &mut ConnectionCatalog, ranks: &FavoriteRanks, scores: &ModelScores) {
    let Ok(models) = &mut catalog.result else {
        return;
    };
    // Sort identities before deduplication, then apply favorite order. Different
    // connections may legitimately advertise the same model id.
    models.sort_by(|left, right| left.model.id.cmp(&right.model.id));
    models.dedup_by(|left, right| left.model.id == right.model.id);
    for option in models.iter_mut() {
        option
            .provider_connection
            .clone_from(&catalog.provider_connection);
        option.model.score = scores.lookup(&option.model.id).cloned();
    }
    models.sort_by(|left, right| {
        let rank = |option: &ProviderModelOption| {
            ranks
                .get(&(option.provider_connection.clone(), option.model.id.clone()))
                .copied()
                .unwrap_or(usize::MAX)
        };
        rank(left)
            .cmp(&rank(right))
            .then_with(|| left.model.id.cmp(&right.model.id))
    });
}

fn model_details(option: &ProviderModelOption) -> String {
    let mut details = Vec::new();
    if let Some(context_length) = option.model.context_length {
        details.push(format!(
            "context: {}",
            crate::llm::token_estimate::format_token_estimate(context_length)
        ));
    }
    if let Some(maximum) = option.model.max_output_tokens {
        details.push(format!(
            "output: {}",
            crate::llm::token_estimate::format_token_estimate(maximum)
        ));
    }
    details.push(match option.model.reasoning_evidence {
        ReasoningEvidence::Unsupported => "reasoning: unsupported".to_string(),
        ReasoningEvidence::Invalid => "reasoning: invalid metadata".to_string(),
        ReasoningEvidence::Conflicting => "reasoning: conflicting metadata".to_string(),
        ReasoningEvidence::Unknown | ReasoningEvidence::Supported => {
            match &option.model.reasoning_efforts {
                Some(efforts) => {
                    let default = option.model.reasoning_default.as_deref();
                    let efforts: Vec<_> = efforts
                        .iter()
                        .map(|effort| {
                            if Some(effort.as_str()) == default {
                                format!("*{effort}")
                            } else {
                                effort.clone()
                            }
                        })
                        .collect();
                    format!("reasoning: {}", efforts.join("/"))
                }
                None => "reasoning: unavailable".to_string(),
            }
        }
    });
    if let Some(score) = &option.model.score {
        details.push(format!("score: {}", score.summary()));
    }
    terminal_safe(&details.join("  "))
}

fn render_connection(
    catalog: &ConnectionCatalog,
    ranks: &FavoriteRanks,
    options: ModelsListOptions,
    output: &mut dyn Write,
) -> io::Result<()> {
    let Ok(models) = &catalog.result else {
        return Ok(());
    };
    writeln!(output, "{}:", terminal_safe(&catalog.provider_connection))?;
    if models.is_empty() {
        return writeln!(output, "  no models reported");
    }
    for option in models {
        let favorite =
            ranks.contains_key(&(catalog.provider_connection.clone(), option.model.id.clone()));
        let model = terminal_safe(&option.model.id);
        let model = if options.color {
            format!("\x1b[1m{model}\x1b[0m")
        } else {
            model
        };
        writeln!(output, "  {} {model}", if favorite { "⚑" } else { " " })?;
        writeln!(output, "      {}", model_details(option))?;
    }
    Ok(())
}

fn listing_scores() -> crate::llm::model_scores::ModelScoreLoad {
    crate::ylva_home()
        .map(|home| {
            crate::llm::model_scores::load_model_scores(
                &home.join(crate::llm::model_scores::MODEL_SCORES_FILE_NAME),
            )
        })
        .unwrap_or_default()
}

/// Load the selected profile through startup's trust and environment rules.
/// Missing implicit profiles use the built-in default; explicit names must exist.
/// Environment-only profiles are materialized as legacy engine configurations,
/// preserving the same provider selection and defaults as chat startup.
pub fn load_profile_for_listing(
    explicit_profile_name: Option<&str>,
    strict_config: bool,
) -> Result<crate::config::ProfileConfig, String> {
    let workspace_root = std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(crate::discover_workspace_root);
    let workspace_root =
        trusted_profile_workspace_root(explicit_profile_name, workspace_root.as_deref())?;
    let environment =
        load_selected_profile_environment(explicit_profile_name, workspace_root.as_deref())?;
    let (mut profile, _) = load_profile(
        explicit_profile_name,
        &environment,
        workspace_root.as_deref(),
    )?;
    if strict_config {
        if let Some(notice) = profile.config_warnings_notice() {
            return Err(format!("{} (--strict-config)", terminal_safe(&notice)));
        }
        if !listing_scores().problems.is_empty() {
            return Err("model-scores.toml has problems (--strict-config)".to_string());
        }
    }
    let provider_env = load_provider_env(&environment)?;
    apply_provider_env_to_profile(&mut profile, &provider_env);
    if profile.engines.is_empty() && profile.provider_connections.is_empty() {
        profile.engines = engines_from_environment(
            &provider_env,
            &profile.default_engine,
            profile.default_model.as_deref(),
        )
        .map_err(|_| {
            "no usable model provider is configured; run `ylva connect <provider>`".to_string()
        })?
        .into_iter()
        .map(|(name, engine)| (name, engine.config()))
        .collect();
    }
    Ok(profile)
}

fn engine_details(config: &crate::llm::EngineConfig) -> String {
    let mut details = vec![
        format!("connection: {}", config.provider_connection),
        format!("model: {}", config.model),
    ];
    if let Some(context_length) = config.context_length {
        details.push(format!(
            "context: {}",
            crate::llm::token_estimate::format_token_estimate(context_length)
        ));
    }
    if let Some(maximum) = config.max_output_tokens {
        details.push(format!(
            "output: {}",
            crate::llm::token_estimate::format_token_estimate(maximum)
        ));
    }
    if let Some(reasoning) = &config.reasoning {
        details.push(format!("reasoning: {reasoning}"));
    }
    if let Some(temperature) = config.temperature {
        details.push(format!("temperature: {temperature}"));
    }
    terminal_safe(&details.join("  "))
}

/// The profile's explicit `[engines.*]` entries by name, without any catalog
/// request. `--only-favorites` keeps the engines whose model is a favorite.
fn render_engines(
    profile: &crate::config::ProfileConfig,
    ranks: &FavoriteRanks,
    options: ModelsListOptions,
    output: &mut dyn Write,
) -> io::Result<()> {
    let mut names: Vec<_> = profile.engines.keys().collect();
    names.sort();
    let mut printed = false;
    for name in names {
        let config = &profile.engines[name];
        let favorite =
            ranks.contains_key(&(config.provider_connection.clone(), config.model.clone()));
        if options.only_favorites && !favorite {
            continue;
        }
        if !printed {
            writeln!(output, "engines:")?;
            printed = true;
        }
        let name = terminal_safe(name);
        let name = if options.color {
            format!("\x1b[1m{name}\x1b[0m")
        } else {
            name
        };
        writeln!(output, "  {} {name}", if favorite { "⚑" } else { " " })?;
        writeln!(output, "      {}", engine_details(config))?;
    }
    if !printed {
        writeln!(output, "no engines configured")?;
    }
    Ok(())
}

fn render_catalogs(
    catalogs: &[ConnectionCatalog],
    ranks: &FavoriteRanks,
    options: ModelsListOptions,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> Result<u8, String> {
    let mut successes = 0;
    let mut printed = 0;
    for catalog in catalogs {
        match &catalog.result {
            // A connection without favorites has nothing to show.
            Ok(models) if options.only_favorites && models.is_empty() => successes += 1,
            Ok(_) => {
                if printed > 0 {
                    writeln!(output)
                        .map_err(|error| format!("cannot write the model listing: {error}"))?;
                }
                render_connection(catalog, ranks, options, output)
                    .map_err(|error| format!("cannot write the model listing: {error}"))?;
                successes += 1;
                printed += 1;
            }
            Err(error) => writeln!(diagnostics, "warning: {}", terminal_safe(error))
                .map_err(|error| format!("cannot write model diagnostics: {error}"))?,
        }
    }
    Ok(u8::from(successes == 0))
}

/// List every configured connection exactly once. `Ok(1)` means all catalogs
/// failed; successful empty catalogs count as success. `Err` is a startup or
/// error, which the executable reports as exit code 2. Runtime output failures
/// return `Ok(1)` after a best-effort diagnostic.
pub fn run_with_options(
    profile: &crate::config::ProfileConfig,
    options: ModelsListOptions,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> Result<u8, String> {
    let factory = AgentFactory::from_profile(profile)
        .map_err(|_| "cannot build model connections from the selected profile; check provider and engine configuration".to_string())?;
    let engines = factory.connection_engines();
    if engines.is_empty() {
        return Err(
            "no provider connections are configured; run `ylva connect <provider>`".to_string(),
        );
    }
    for warning in &profile.config_warnings {
        writeln!(
            diagnostics,
            "warning: {}",
            terminal_safe(&warning.to_string())
        )
        .map_err(|error| format!("cannot write model diagnostics: {error}"))?;
    }
    let ranks = favorite_ranks(&engines);
    if options.only_engines {
        return match render_engines(profile, &ranks, options, output) {
            Ok(()) => Ok(0),
            Err(error) => {
                let _ = writeln!(
                    diagnostics,
                    "error: cannot write the engine listing: {error}"
                );
                Ok(1)
            }
        };
    }
    // Listing is one-shot. Use the same loader/lookup as the picker without
    // its cache's raw malformed-file diagnostics (which can echo file values).
    let scores = listing_scores();
    if !scores.problems.is_empty() {
        writeln!(
            diagnostics,
            "warning: model-scores.toml has problems; invalid scores are ignored"
        )
        .map_err(|error| format!("cannot write model diagnostics: {error}"))?;
    }
    let mut catalogs = tokio::runtime::Runtime::new()
        .map_err(|error| format!("cannot start Tokio runtime: {error}"))?
        .block_on(async {
            futures::future::join_all(engines.iter().map(|(connection, engine)| {
                fetch_connection_catalog(connection, Arc::clone(engine), options.include_disallowed)
            }))
            .await
        });
    for catalog in &mut catalogs {
        prepare_catalog(catalog, &ranks, &scores.scores);
        if options.only_favorites {
            if let Ok(models) = &mut catalog.result {
                models.retain(|option| {
                    ranks.contains_key(&(
                        catalog.provider_connection.clone(),
                        option.model.id.clone(),
                    ))
                });
            }
        }
    }
    match render_catalogs(&catalogs, &ranks, options, output, diagnostics) {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = writeln!(diagnostics, "error: {}", terminal_safe(&error));
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests;
