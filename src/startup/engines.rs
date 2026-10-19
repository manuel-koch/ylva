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

//! Provider environment handling and environment-derived engine construction.

use std::collections::HashMap;
use std::sync::Arc;

use crate::llm::provider_registry::{ProviderEnv, ProviderRegistry, ProviderSelection};
use crate::{Engine, EngineImpl, ProfileConfig};

pub(super) fn load_provider_env(
    environment: &HashMap<String, String>,
) -> Result<ProviderEnv, String> {
    Ok(ProviderEnv::from_values(environment))
}

/// Supply missing API keys for explicitly configured connections and legacy
/// flat engines from the selected profile environment. Explicit values in
/// `config.toml` remain authoritative.
pub(super) fn apply_provider_env_to_profile(
    profile: &mut ProfileConfig,
    provider_env: &ProviderEnv,
) {
    for provider_config in profile.provider_connections.values_mut() {
        provider_env.apply_missing_api_key_to_provider(provider_config);
    }
    for engine_config in profile.engines.values_mut() {
        if !profile
            .provider_connections
            .contains_key(&engine_config.provider_connection)
        {
            provider_env.apply_missing_api_key(engine_config);
        }
    }
}

/// Build one engine for every provider configured by the environment.
///
/// The first provider retains the profile's default engine name and therefore
/// remains the initial active engine. Additional providers use their provider
/// names as engine names so the engine router can switch to them.
pub(super) fn engines_from_environment(
    environment: &ProviderEnv,
    default_engine_name: &str,
    default_model: Option<&str>,
) -> anyhow::Result<HashMap<String, Arc<dyn Engine>>> {
    let selections = ProviderRegistry::all_from_environment(environment);
    let mut engines = HashMap::new();
    for (index, selection) in selections.into_iter().enumerate() {
        let provider_connection = selection.provider_connection().to_string();
        let mut engine_name = if index == 0 {
            default_engine_name.to_string()
        } else {
            provider_connection.clone()
        };
        if engines.contains_key(&engine_name) {
            engine_name = format!("{provider_connection}-{index}");
        }

        log_provider_selection(&selection);
        let engine_config = selection.engine_config(default_model);
        let provider = ProviderRegistry::provider_from_environment(selection)?;
        engines.insert(
            engine_name,
            Arc::new(EngineImpl::new(provider, engine_config)) as Arc<dyn Engine>,
        );
    }
    Ok(engines)
}

/// Log provider wiring without ever logging secret values.
fn log_provider_selection(selection: &ProviderSelection) {
    match selection {
        ProviderSelection::OpenAiCompatible {
            provider_connection,
            base_url,
            model,
            api_key,
            ..
        } => {
            tracing::debug!(
                provider_connection,
                base_url,
                model,
                api_key = if api_key.is_some() { "set" } else { "unset" },
                "provider selected"
            );
        }
        ProviderSelection::Mock => {
            tracing::debug!(
                provider_connection = "mock",
                base_url = "-",
                model = "-",
                api_key = "unset",
                "provider selected"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_environment_fills_only_missing_explicit_engine_api_keys() {
        let mut profile = ProfileConfig::default();
        profile.engines.insert(
            "local".to_string(),
            crate::EngineConfig {
                provider_connection: "ollama".to_string(),
                model: "local".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: Some("http://localhost:11434".to_string()),
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        );
        profile.engines.insert(
            "hosted".to_string(),
            crate::EngineConfig {
                provider_connection: "openrouter".to_string(),
                model: "hosted/model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: Some("https://openrouter.ai".to_string()),
                api_key: Some("explicit-key".to_string()),
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        );
        let provider_env = ProviderEnv {
            ollama_api_key: Some("from-dotenv".to_string()),
            openrouter_api_key: Some("ignored".to_string()),
            ..Default::default()
        };

        apply_provider_env_to_profile(&mut profile, &provider_env);

        assert_eq!(
            profile.engines["local"].api_key.as_deref(),
            Some("from-dotenv")
        );
        assert_eq!(
            profile.engines["hosted"].api_key.as_deref(),
            Some("explicit-key")
        );
    }

    #[test]
    fn environment_builds_one_engine_for_each_configured_provider() {
        let provider_env = ProviderEnv {
            ollama_base_url: Some("http://localhost:11434".to_string()),
            ollama_model: Some("local-model".to_string()),
            openrouter_api_key: Some("test-key".to_string()),
            openrouter_model: Some("hosted/model".to_string()),
            ..Default::default()
        };

        let engines =
            engines_from_environment(&provider_env, "default", None).expect("engines build");

        assert_eq!(engines.len(), 2);
        assert_eq!(engines["default"].config().provider_connection, "ollama");
        assert_eq!(engines["default"].config().model, "local-model");
        assert_eq!(
            engines["openrouter"].config().provider_connection,
            "openrouter"
        );
        assert_eq!(engines["openrouter"].config().model, "hosted/model");
    }
}
