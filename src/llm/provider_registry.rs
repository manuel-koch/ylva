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
use std::sync::Arc;
use std::time::Duration;

use crate::config::{ProviderConnectionConfig, ProviderKind};
use crate::llm::engine::EngineConfig;

use super::{
    CopilotProvider, EdenAiProvider, MockProvider, OllamaProvider, OpenAiCompatibleProvider,
    OpenRouterProvider, Provider,
};

/// Default Ollama model when `OLLAMA_MODEL` is unset.
pub const DEFAULT_OLLAMA_MODEL: &str = "gemma4:e4b-mlx";

/// Default OpenRouter model when `OPENROUTER_MODEL` is unset.
pub const DEFAULT_OPENROUTER_MODEL: &str = "deepseek/deepseek-v4-flash-0731";

/// Default EdenAI model when `EDENAI_MODEL` is unset.
pub const DEFAULT_EDENAI_MODEL: &str = "openai/gpt-latest";

/// Default Copilot model when `COPILOT_MODEL` is unset.
pub const DEFAULT_COPILOT_MODEL: &str = "gpt-4o";

/// Default OpenRouter origin when `OPENROUTER_BASE_URL` is unset.
pub const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai";

/// Default EdenAI origin when `EDENAI_BASE_URL` is unset.
pub const DEFAULT_EDENAI_BASE_URL: &str = "https://api.edenai.run";

/// Snapshot of the environment variables used for automatic provider
/// selection.
///
/// Keeping this as a plain value makes environment resolution deterministic
/// and testable without mutating process-global environment variables.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct ProviderEnv {
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
    pub ollama_api_key: Option<String>,
    pub openrouter_base_url: Option<String>,
    pub openrouter_model: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub edenai_base_url: Option<String>,
    pub edenai_model: Option<String>,
    pub edenai_api_key: Option<String>,
    pub copilot_model: Option<String>,
    /// Long-lived GitHub OAuth token for Copilot. `GH_TOKEN` is never read.
    pub copilot_github_token: Option<String>,
}

impl ProviderEnv {
    /// Read the provider-related values from the process environment.
    pub fn from_process_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read provider values from the process environment and an optional
    /// profile-local `.env` file. Process environment values take precedence,
    /// including explicitly exported empty values.
    pub fn from_process_env_and_dotenv(profile_env_path: Option<&Path>) -> anyhow::Result<Self> {
        let values = crate::config::load_profile_environment(profile_env_path)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Self::from_values(&values))
    }

    /// Build a provider snapshot from a previously merged profile
    /// environment. Process values already have precedence in this map.
    pub fn from_values(values: &HashMap<String, String>) -> Self {
        Self::from_lookup(|name| values.get(name).cloned())
    }

    /// Fill an explicit engine's missing API key from the matching
    /// implementation-kind environment variable. Explicit engine values
    /// remain authoritative.
    pub fn apply_missing_api_key(&self, engine_config: &mut EngineConfig) {
        if engine_config.api_key.is_some() {
            return;
        }
        let api_key = match engine_config.provider_connection.as_str() {
            "ollama" => self.ollama_api_key.as_ref(),
            "openrouter" => self.openrouter_api_key.as_ref(),
            "edenai" => self.edenai_api_key.as_ref(),
            "copilot" => self.copilot_github_token.as_ref(),
            _ => None,
        };
        if let Some(api_key) = api_key.filter(|api_key| !api_key.is_empty()) {
            engine_config.api_key = Some(api_key.clone());
        }
    }

    /// Fill a named provider connection's missing API key from the matching
    /// provider environment variable. Explicit connection values remain
    /// authoritative.
    pub fn apply_missing_api_key_to_provider(
        &self,
        provider_config: &mut ProviderConnectionConfig,
    ) {
        if provider_config.api_key.is_some() {
            return;
        }
        let api_key = match provider_config.provider {
            ProviderKind::Ollama => self.ollama_api_key.as_ref(),
            ProviderKind::Openrouter => self.openrouter_api_key.as_ref(),
            ProviderKind::Edenai => self.edenai_api_key.as_ref(),
            ProviderKind::Copilot => self.copilot_github_token.as_ref(),
            ProviderKind::Mock => None,
        };
        if let Some(api_key) = api_key.filter(|api_key| !api_key.is_empty()) {
            provider_config.api_key = Some(api_key.clone());
        }
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            ollama_base_url: lookup("OLLAMA_BASE_URL"),
            ollama_model: lookup("OLLAMA_MODEL"),
            ollama_api_key: lookup("OLLAMA_API_KEY"),
            openrouter_base_url: lookup("OPENROUTER_BASE_URL"),
            openrouter_model: lookup("OPENROUTER_MODEL"),
            openrouter_api_key: lookup("OPENROUTER_API_KEY"),
            edenai_base_url: lookup("EDENAI_BASE_URL"),
            edenai_model: lookup("EDENAI_MODEL"),
            edenai_api_key: lookup("EDENAI_API_KEY"),
            copilot_model: lookup("COPILOT_MODEL"),
            copilot_github_token: lookup("COPILOT_GITHUB_TOKEN"),
        }
    }
}

#[cfg(test)]
fn load_dotenv_values(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HashMap::new());
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "cannot read profile environment at {}: {error}",
                path.display()
            ));
        }
    };
    anyhow::ensure!(
        metadata.is_file(),
        "profile environment path {} is not a file",
        path.display()
    );

    let entries = dotenvy::from_path_iter(path).map_err(|error| {
        anyhow::anyhow!(
            "cannot parse profile environment at {}: {error}",
            path.display()
        )
    })?;
    let mut values = HashMap::new();
    for entry in entries {
        let (name, value) = entry.map_err(|error| {
            anyhow::anyhow!(
                "cannot parse profile environment at {}: {error}",
                path.display()
            )
        })?;
        values.insert(name, value);
    }
    tracing::info!(
        path = %path.display(),
        variables = values.len(),
        "profile environment loaded"
    );
    Ok(values)
}

/// A provider selected from either explicit engine configuration or the
/// process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelection {
    /// The deterministic in-process provider used when an engine explicitly
    /// names `mock`.
    Mock,
    /// A provider connection name backed by the OpenAI-compatible implementation.
    OpenAiCompatible {
        provider_kind: ProviderKind,
        provider_connection: String,
        base_url: String,
        model: String,
        api_key: Option<String>,
    },
}

impl ProviderSelection {
    /// The model selected by this provider configuration.
    pub fn model(&self) -> &str {
        match self {
            ProviderSelection::Mock => "",
            ProviderSelection::OpenAiCompatible { model, .. } => model,
        }
    }

    /// The stable provider-connection name used in engine configuration and UI
    /// labels.
    pub fn provider_connection(&self) -> &str {
        match self {
            ProviderSelection::Mock => ProviderKind::Mock.as_str(),
            ProviderSelection::OpenAiCompatible {
                provider_connection,
                ..
            } => provider_connection,
        }
    }

    /// Convert this environment selection into the engine configuration that
    /// owns its connection and initial Engine runtime settings.
    pub fn engine_config(&self, default_model: Option<&str>) -> EngineConfig {
        match self {
            ProviderSelection::Mock => EngineConfig {
                provider_connection: ProviderKind::Mock.as_str().to_string(),
                model: default_model.unwrap_or_default().to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
            ProviderSelection::OpenAiCompatible {
                provider_connection,
                base_url,
                model,
                api_key,
                ..
            } => EngineConfig {
                provider_connection: provider_connection.clone(),
                model: if model.is_empty() {
                    default_model.unwrap_or_default().to_string()
                } else {
                    model.clone()
                },
                temperature: None,
                context_length: None,
                api_base_url: Some(base_url.clone()).filter(|url| !url.is_empty()),
                api_key: api_key.clone(),
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        }
    }
}

/// Central provider-name validation, environment resolution, and provider
/// construction.
pub struct ProviderRegistry;

impl ProviderRegistry {
    /// Resolve a legacy flat engine whose `provider` field directly names the
    /// implementation kind.
    ///
    /// Profile configuration is authoritative here: unlike environment
    /// resolution, this path never supplies provider-specific defaults.
    pub fn from_engine_config(engine_config: &EngineConfig) -> anyhow::Result<ProviderSelection> {
        match engine_config.provider_connection.as_str() {
            "mock" => Ok(ProviderSelection::Mock),
            "copilot" => Ok(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Copilot,
                provider_connection: engine_config.provider_connection.clone(),
                base_url: String::new(),
                model: engine_config.model.clone(),
                api_key: engine_config.api_key.clone(),
            }),
            name @ ("ollama" | "openrouter" | "edenai") => {
                let provider_kind = match name {
                    "ollama" => ProviderKind::Ollama,
                    "openrouter" => ProviderKind::Openrouter,
                    _ => ProviderKind::Edenai,
                };
                let base_url = engine_config
                    .api_base_url
                    .as_deref()
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "engine using provider '{}' requires an api_base_url",
                            engine_config.provider_connection
                        )
                    })?;
                Ok(ProviderSelection::OpenAiCompatible {
                    provider_kind,
                    provider_connection: engine_config.provider_connection.clone(),
                    base_url: base_url.to_string(),
                    model: engine_config.model.clone(),
                    api_key: engine_config.api_key.clone(),
                })
            }

            provider_connection => {
                anyhow::bail!("unsupported provider '{provider_connection}'")
            }
        }
    }

    /// Resolve an engine through a named provider connection.
    ///
    /// Engine-level connection values override the named connection when
    /// present. The engine's `provider` remains the connection name so that
    /// model-picker labels and persistence distinguish two connections that
    /// share the same implementation kind.
    pub fn from_provider_config(
        provider_connection: &str,
        provider_config: &ProviderConnectionConfig,
        engine_config: &EngineConfig,
    ) -> anyhow::Result<ProviderSelection> {
        match provider_config.provider {
            ProviderKind::Mock => Ok(ProviderSelection::Mock),
            ProviderKind::Copilot => Ok(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Copilot,
                provider_connection: provider_connection.to_string(),
                base_url: String::new(),
                model: engine_config.model.clone(),
                api_key: engine_config
                    .api_key
                    .clone()
                    .or_else(|| provider_config.api_key.clone()),
            }),
            ProviderKind::Ollama | ProviderKind::Openrouter | ProviderKind::Edenai => {
                let base_url = engine_config
                    .api_base_url
                    .as_ref()
                    .or(provider_config.api_base_url.as_ref())
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "provider connection '{}' using provider '{}' requires an api_base_url",
                            provider_connection,
                            provider_config.provider.as_str()
                        )
                    })?;
                Ok(ProviderSelection::OpenAiCompatible {
                    provider_kind: provider_config.provider,
                    provider_connection: provider_connection.to_string(),
                    base_url: base_url.clone(),
                    model: engine_config.model.clone(),
                    api_key: engine_config
                        .api_key
                        .clone()
                        .or_else(|| provider_config.api_key.clone()),
                })
            }
        }
    }

    /// Resolve the backend for a captured environment snapshot, or `None` when
    /// no provider is configured.
    ///
    /// Precedence when several are configured: Ollama, OpenRouter, EdenAI,
    /// Copilot.
    pub fn from_environment(env: &ProviderEnv) -> Option<ProviderSelection> {
        Self::all_from_environment(env).into_iter().next()
    }

    /// Resolve every configured implementation kind from a captured
    /// environment snapshot. Ollama remains the first/default provider for
    /// compatibility, while OpenRouter, EdenAI and Copilot follow, in that
    /// order, as additional selectable providers when configured.
    pub fn all_from_environment(env: &ProviderEnv) -> Vec<ProviderSelection> {
        let mut selections = Vec::new();
        if let Some(base_url) = env.ollama_base_url.as_ref().filter(|url| !url.is_empty()) {
            let model = env
                .ollama_model
                .clone()
                .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());
            selections.push(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Ollama,
                provider_connection: ProviderKind::Ollama.as_str().to_string(),
                base_url: base_url.clone(),
                model,
                api_key: env.ollama_api_key.clone(),
            });
        }

        let openrouter_requested = [
            &env.openrouter_base_url,
            &env.openrouter_model,
            &env.openrouter_api_key,
        ]
        .into_iter()
        .any(|value| value.as_ref().is_some_and(|value| !value.is_empty()));
        if openrouter_requested {
            let base_url = env
                .openrouter_base_url
                .clone()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_OPENROUTER_BASE_URL.to_string());
            let model = env
                .openrouter_model
                .clone()
                .unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string());
            selections.push(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Openrouter,
                provider_connection: ProviderKind::Openrouter.as_str().to_string(),
                base_url,
                model,
                api_key: env.openrouter_api_key.clone(),
            });
        }

        let edenai_requested = [&env.edenai_base_url, &env.edenai_model, &env.edenai_api_key]
            .into_iter()
            .any(|value| value.as_ref().is_some_and(|value| !value.is_empty()));
        if edenai_requested {
            let base_url = env
                .edenai_base_url
                .clone()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_EDENAI_BASE_URL.to_string());
            let model = env
                .edenai_model
                .clone()
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| DEFAULT_EDENAI_MODEL.to_string());
            selections.push(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Edenai,
                provider_connection: ProviderKind::Edenai.as_str().to_string(),
                base_url,
                model,
                api_key: env.edenai_api_key.clone(),
            });
        }

        if let Some(github_token) = env
            .copilot_github_token
            .as_ref()
            .filter(|token| !token.is_empty())
        {
            selections.push(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Copilot,
                provider_connection: ProviderKind::Copilot.as_str().to_string(),
                base_url: String::new(),
                model: env
                    .copilot_model
                    .clone()
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| DEFAULT_COPILOT_MODEL.to_string()),
                api_key: Some(github_token.clone()),
            });
        }

        selections
    }

    /// Build the provider for an explicitly configured engine.
    pub fn provider_from_engine_config(
        engine_config: &EngineConfig,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        Self::build(Self::from_engine_config(engine_config)?)
    }

    /// Build a provider from a named connection and an engine's Engine runtime
    /// settings.
    pub fn provider_from_provider_config(
        provider_connection: &str,
        provider_config: &ProviderConnectionConfig,
        engine_config: &EngineConfig,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        Self::build(Self::from_provider_config(
            provider_connection,
            provider_config,
            engine_config,
        )?)
    }

    /// Build the provider represented by a resolved selection.
    pub fn build(selection: ProviderSelection) -> anyhow::Result<Arc<dyn Provider>> {
        match selection {
            ProviderSelection::Mock => Ok(Arc::new(MockProvider::new(
                vec!["mock response".to_string()],
                Duration::from_millis(0),
            ))),
            ProviderSelection::OpenAiCompatible {
                provider_kind,
                provider_connection,
                base_url,
                model,
                api_key,
            } => match provider_kind {
                ProviderKind::Ollama => Ok(Arc::new(OllamaProvider::new(
                    provider_connection,
                    base_url,
                    model,
                    api_key,
                ))),
                ProviderKind::Openrouter => Ok(Arc::new(OpenRouterProvider::new(
                    provider_connection,
                    base_url,
                    model,
                    api_key,
                ))),
                ProviderKind::Edenai => Ok(Arc::new(EdenAiProvider::new(
                    provider_connection,
                    base_url,
                    model,
                    api_key,
                ))),
                ProviderKind::Copilot => {
                    let github_token =
                        api_key.filter(|token| !token.is_empty()).ok_or_else(|| {
                            anyhow::anyhow!(
                                "provider connection '{provider_connection}' requires a Copilot \
                             GitHub token (COPILOT_GITHUB_TOKEN), run `ylva connect copilot`"
                            )
                        })?;
                    Ok(Arc::new(CopilotProvider::new(
                        provider_connection,
                        model,
                        github_token,
                    )?))
                }
                ProviderKind::Mock => Ok(Arc::new(OpenAiCompatibleProvider::new(
                    provider_connection,
                    base_url,
                    model,
                    api_key,
                ))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_resolution_without_provider_is_empty() {
        assert_eq!(
            ProviderRegistry::from_environment(&ProviderEnv::default()),
            None
        );
        assert!(ProviderRegistry::all_from_environment(&ProviderEnv::default()).is_empty());
    }

    #[test]
    fn environment_resolution_prefers_ollama() {
        let env = ProviderEnv {
            ollama_base_url: Some("http://localhost:11434".to_string()),
            openrouter_api_key: Some("sk-or-key".to_string()),
            ..Default::default()
        };

        assert_eq!(
            ProviderRegistry::from_environment(&env),
            Some(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Ollama,
                provider_connection: ProviderKind::Ollama.as_str().to_string(),
                base_url: "http://localhost:11434".to_string(),
                model: DEFAULT_OLLAMA_MODEL.to_string(),
                api_key: None,
            })
        );
    }

    #[test]
    fn environment_resolution_uses_openrouter_defaults() {
        let env = ProviderEnv {
            openrouter_api_key: Some("sk-or-key".to_string()),
            ..Default::default()
        };

        assert_eq!(
            ProviderRegistry::from_environment(&env),
            Some(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Openrouter,
                provider_connection: ProviderKind::Openrouter.as_str().to_string(),
                base_url: DEFAULT_OPENROUTER_BASE_URL.to_string(),
                model: DEFAULT_OPENROUTER_MODEL.to_string(),
                api_key: Some("sk-or-key".to_string()),
            })
        );
    }

    #[test]
    fn all_environment_resolution_keeps_each_configured_provider() {
        let env = ProviderEnv {
            ollama_base_url: Some("http://localhost:11434".to_string()),
            ollama_model: Some("local-model".to_string()),
            openrouter_api_key: Some("sk-or-key".to_string()),
            openrouter_model: Some("hosted/model".to_string()),
            ..Default::default()
        };

        let selections = ProviderRegistry::all_from_environment(&env);

        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].provider_connection(), "ollama");
        assert_eq!(selections[1].provider_connection(), "openrouter");
        assert_eq!(selections[0].model(), "local-model");
        assert_eq!(selections[1].model(), "hosted/model");
    }

    #[test]
    fn environment_resolution_uses_edenai_defaults_and_ranks_after_openrouter() {
        let env = ProviderEnv {
            edenai_api_key: Some("eden-key".to_string()),
            ..Default::default()
        };
        assert_eq!(
            ProviderRegistry::from_environment(&env),
            Some(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Edenai,
                provider_connection: "edenai".to_string(),
                base_url: DEFAULT_EDENAI_BASE_URL.to_string(),
                model: DEFAULT_EDENAI_MODEL.to_string(),
                api_key: Some("eden-key".to_string()),
            })
        );

        let env = ProviderEnv {
            openrouter_api_key: Some("sk-or-key".to_string()),
            edenai_api_key: Some("eden-key".to_string()),
            copilot_github_token: Some("ghu_token".to_string()),
            ..Default::default()
        };
        let names: Vec<_> = ProviderRegistry::all_from_environment(&env)
            .iter()
            .map(|selection| selection.provider_connection().to_string())
            .collect();
        assert_eq!(names, ["openrouter", "edenai", "copilot"]);
    }

    #[test]
    fn edenai_connection_requires_base_url_takes_env_key_and_builds() {
        let mut provider_config = ProviderConnectionConfig {
            provider: ProviderKind::Edenai,
            api_base_url: None,
            api_key: None,
            favorite_models: Vec::new(),
            allowed_models: None,
        };
        let engine_config = ProviderSelection::Mock.engine_config(Some("openai/gpt-latest"));
        assert!(
            ProviderRegistry::from_provider_config("eden", &provider_config, &engine_config)
                .is_err()
        );

        provider_config.api_base_url = Some("https://api.edenai.run".to_string());
        ProviderEnv::from_values(&HashMap::from([(
            "EDENAI_API_KEY".to_string(),
            "eden-key".to_string(),
        )]))
        .apply_missing_api_key_to_provider(&mut provider_config);
        assert_eq!(provider_config.api_key.as_deref(), Some("eden-key"));
        let selection =
            ProviderRegistry::from_provider_config("eden", &provider_config, &engine_config)
                .expect("resolves");
        assert_eq!(selection.provider_connection(), "eden");
        assert!(ProviderRegistry::build(selection).is_ok());
    }

    #[test]
    fn environment_resolution_detects_copilot_token_with_default_or_custom_model() {
        let env = ProviderEnv {
            copilot_github_token: Some("ghu_token".to_string()),
            ..Default::default()
        };
        let expected = |model: &str| {
            Some(ProviderSelection::OpenAiCompatible {
                provider_kind: ProviderKind::Copilot,
                provider_connection: "copilot".to_string(),
                base_url: String::new(),
                model: model.to_string(),
                api_key: Some("ghu_token".to_string()),
            })
        };
        assert_eq!(
            ProviderRegistry::from_environment(&env),
            expected(DEFAULT_COPILOT_MODEL)
        );
        let env = ProviderEnv {
            copilot_model: Some("claude-sonnet-4".to_string()),
            ..env
        };
        assert_eq!(
            ProviderRegistry::from_environment(&env),
            expected("claude-sonnet-4")
        );
        assert_eq!(
            ProviderRegistry::from_environment(&env)
                .expect("copilot token configures a provider")
                .engine_config(None)
                .api_base_url,
            None
        );
    }

    #[test]
    fn copilot_connection_needs_no_base_url_and_builds_with_token() {
        let provider_config = ProviderConnectionConfig {
            provider: ProviderKind::Copilot,
            api_base_url: None,
            api_key: Some("ghu_token".to_string()),
            favorite_models: Vec::new(),
            allowed_models: None,
        };
        let engine_config = ProviderSelection::Mock.engine_config(Some("gpt-4o"));
        let selection =
            ProviderRegistry::from_provider_config("work", &provider_config, &engine_config)
                .expect("resolves without api_base_url");
        assert_eq!(selection.provider_connection(), "work");
        assert!(ProviderRegistry::build(selection).is_ok());
    }

    #[test]
    fn copilot_build_rejects_missing_and_pat_tokens() {
        for api_key in [None, Some("github_pat_abc".to_string())] {
            let provider_config = ProviderConnectionConfig {
                provider: ProviderKind::Copilot,
                api_base_url: None,
                api_key,
                favorite_models: Vec::new(),
                allowed_models: None,
            };
            let engine_config = ProviderSelection::Mock.engine_config(Some("gpt-4o"));
            assert!(ProviderRegistry::provider_from_provider_config(
                "copilot",
                &provider_config,
                &engine_config
            )
            .is_err());
        }
    }

    #[test]
    fn copilot_token_fills_missing_api_key_but_gh_token_is_ignored() {
        let env = ProviderEnv::from_values(&HashMap::from([
            ("GH_TOKEN".to_string(), "ghu_other".to_string()),
            ("COPILOT_GITHUB_TOKEN".to_string(), "ghu_token".to_string()),
        ]));
        let mut provider_config = ProviderConnectionConfig {
            provider: ProviderKind::Copilot,
            api_base_url: None,
            api_key: None,
            favorite_models: Vec::new(),
            allowed_models: None,
        };
        env.apply_missing_api_key_to_provider(&mut provider_config);
        assert_eq!(provider_config.api_key.as_deref(), Some("ghu_token"));
        let only_gh = ProviderEnv::from_values(&HashMap::from([(
            "GH_TOKEN".to_string(),
            "ghu_other".to_string(),
        )]));
        assert_eq!(only_gh.copilot_github_token, None);
    }

    #[test]
    fn dotenv_values_are_parsed_and_process_values_take_precedence() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let environment_path = temporary_directory.path().join(".env");
        std::fs::write(
            &environment_path,
            "OLLAMA_BASE_URL=http://file-host\nOPENROUTER_API_KEY=file-key\n",
        )
        .expect("write dotenv fixture");
        let dotenv_values = load_dotenv_values(&environment_path).expect("parse dotenv fixture");
        let process_values =
            HashMap::from([("OPENROUTER_API_KEY".to_string(), "process-key".to_string())]);

        let environment = ProviderEnv::from_lookup(|name| {
            process_values
                .get(name)
                .cloned()
                .or_else(|| dotenv_values.get(name).cloned())
        });

        assert_eq!(
            environment.ollama_base_url.as_deref(),
            Some("http://file-host")
        );
        assert_eq!(
            environment.openrouter_api_key.as_deref(),
            Some("process-key")
        );
    }

    #[test]
    fn dotenv_api_key_fills_explicit_engine_without_overwriting_it() {
        let environment = ProviderEnv {
            ollama_api_key: Some("from-environment".to_string()),
            openrouter_api_key: Some("openrouter-key".to_string()),
            ..Default::default()
        };
        let mut engine = EngineConfig {
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
        };

        environment.apply_missing_api_key(&mut engine);
        assert_eq!(engine.api_key.as_deref(), Some("from-environment"));

        engine.api_key = Some("explicit-key".to_string());
        environment.apply_missing_api_key(&mut engine);
        assert_eq!(engine.api_key.as_deref(), Some("explicit-key"));
    }

    #[test]
    fn configured_provider_names_are_validated() {
        let config = EngineConfig {
            provider_connection: "anthropic".to_string(),
            model: "claude".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some("https://example.test/v1".to_string()),
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };

        let error = ProviderRegistry::from_engine_config(&config)
            .expect_err("unknown provider names must be rejected");
        assert_eq!(error.to_string(), "unsupported provider 'anthropic'");
    }

    #[test]
    fn configured_openai_compatible_provider_requires_a_base_url() {
        let config = EngineConfig {
            provider_connection: "ollama".to_string(),
            model: "llama".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };

        let error = ProviderRegistry::from_engine_config(&config)
            .expect_err("an OpenAI-compatible provider needs a base URL");
        assert_eq!(
            error.to_string(),
            "engine using provider 'ollama' requires an api_base_url"
        );
    }

    #[test]
    fn configured_mock_builds_the_mock_provider() {
        let config = EngineConfig {
            provider_connection: "mock".to_string(),
            model: "ignored".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };

        let provider = ProviderRegistry::provider_from_engine_config(&config)
            .expect("mock provider configuration is valid");
        assert_eq!(provider.name(), "mock");
    }

    #[test]
    fn configured_openai_compatible_provider_preserves_its_provider_name() {
        let config = EngineConfig {
            provider_connection: "openrouter".to_string(),
            model: "some/model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: Some("https://openrouter.ai".to_string()),
            api_key: Some("secret".to_string()),
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };

        let provider = ProviderRegistry::provider_from_engine_config(&config)
            .expect("OpenAI-compatible provider configuration is valid");
        assert_eq!(provider.name(), "openrouter");
    }

    #[test]
    fn named_provider_connection_preserves_its_alias_for_runtime_identity() {
        let provider_config = ProviderConnectionConfig {
            provider: ProviderKind::Ollama,
            api_base_url: Some("http://localhost:11434".to_string()),
            api_key: None,
            favorite_models: Vec::new(),
            allowed_models: None,
        };
        let engine_config = EngineConfig {
            provider_connection: "ollama-local".to_string(),
            model: "local-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,

            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };

        let selection = ProviderRegistry::from_provider_config(
            "ollama-local",
            &provider_config,
            &engine_config,
        )
        .expect("the named provider connection is valid");
        assert_eq!(selection.provider_connection(), "ollama-local");

        let provider = ProviderRegistry::provider_from_provider_config(
            "ollama-local",
            &provider_config,
            &engine_config,
        )
        .expect("the named provider builds");
        assert_eq!(provider.name(), "ollama-local");
    }
}
