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

mod config;
mod router;
mod runtime;
mod selection;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::*;
use crate::llm::{MockProvider, OpenAiCompatibleProvider, Provider};

fn make_engine_config() -> EngineConfig {
    EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    }
}

struct CatalogEngine {
    config: RwLock<EngineConfig>,
    models: Vec<ModelInfo>,
    model_list_error: Option<String>,
    model_list_calls: std::sync::atomic::AtomicUsize,
}

impl CatalogEngine {
    fn new(provider: &str, model: &str, models: &[&str]) -> Self {
        CatalogEngine {
            config: RwLock::new(EngineConfig {
                provider_connection: provider.to_string(),
                model: model.to_string(),
                temperature: None,
                context_length: None,
                api_base_url: Some(format!("https://{provider}.test/v1")),
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            }),
            models: models
                .iter()
                .map(|id| ModelInfo {
                    id: (*id).to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                })
                .collect(),
            model_list_error: None,
            model_list_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn failing(provider: &str, message: &str) -> Self {
        let mut engine = Self::new(provider, "unavailable", &[]);
        engine.model_list_error = Some(message.to_string());
        engine
    }
}

impl Engine for CatalogEngine {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async { anyhow::bail!("catalog engine does not chat") })
    }

    fn config(&self) -> EngineConfig {
        self.config
            .read()
            .expect("catalog engine config lock")
            .clone()
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.model_list_calls.fetch_add(1, Ordering::SeqCst);
        let models = self.models.clone();
        let error = self.model_list_error.clone();
        Box::pin(async move {
            match error {
                Some(error) => anyhow::bail!(error),
                None => Ok(models),
            }
        })
    }

    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.config
            .write()
            .expect("catalog engine config lock")
            .model = model.to_string();
        Ok(())
    }
}

/// Provider stub whose catalog can carry input modalities and reasoning
/// efforts, for exercising [`EngineImpl::hydrate_catalog_metadata`]. `chat` always fails — the
/// hydration tests only need the catalog fetch to run.
struct CatalogProvider {
    models: Vec<ModelInfo>,
    list_models_calls: std::sync::atomic::AtomicUsize,
    fail_list_models: bool,
}

impl CatalogProvider {
    fn new(models: Vec<ModelInfo>) -> Self {
        CatalogProvider {
            models,
            list_models_calls: std::sync::atomic::AtomicUsize::new(0),
            fail_list_models: false,
        }
    }

    fn failing() -> Self {
        CatalogProvider {
            models: Vec::new(),
            list_models_calls: std::sync::atomic::AtomicUsize::new(0),
            fail_list_models: true,
        }
    }

    fn list_models_calls(&self) -> usize {
        self.list_models_calls.load(Ordering::SeqCst)
    }
}

impl Provider for CatalogProvider {
    fn set_model(&self, _model: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "catalog"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.list_models_calls.fetch_add(1, Ordering::SeqCst);
        let models = self.models.clone();
        let fail_list_models = self.fail_list_models;
        Box::pin(async move {
            if fail_list_models {
                anyhow::bail!("catalog unavailable");
            }
            Ok(models)
        })
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async { anyhow::bail!("catalog provider does not chat") })
    }
}

fn model_info_with_modalities(id: &str, input_modalities: Option<Vec<InputModality>>) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        context_length: None,
        reasoning_efforts: None,
        reasoning_default: None,
        input_modalities,
    }
}

fn model_info_with_reasoning_efforts(
    id: &str,
    reasoning_efforts: Option<Vec<String>>,
) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        context_length: None,
        reasoning_efforts,
        reasoning_default: None,
        input_modalities: None,
    }
}

/// An engine seeded with a profile `reasoning` effort on top of the
/// standard `test-model` engine config.
fn engine_with_profile_reasoning(provider: Arc<dyn Provider>, reasoning: &str) -> EngineImpl {
    let config = EngineConfig {
        reasoning: Some(reasoning.to_string()),

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
        ..make_engine_config()
    };
    EngineImpl::new(provider, config)
}

#[test]
fn the_compaction_budget_is_read_off_the_active_engine() {
    let engine = CatalogEngine::new("mock", "test-model", &[]);

    let budget = crate::agent::compaction::CompactionBudget::from_engine(&engine)
        .expect("a non-zero window always yields a budget");
    assert_eq!(
        budget.context_length, DEFAULT_CONTEXT_LENGTH,
        "no profile context_length and no catalog selection fall back to \
         the built-in window"
    );
    assert_eq!(budget.target_ratio, 0.5);
    assert_eq!(
        budget.soft_threshold,
        crate::agent::compaction::DEFAULT_SOFT_COMPACTION_THRESHOLD
    );
    assert_eq!(
        budget.hard_threshold,
        crate::agent::compaction::DEFAULT_HARD_COMPACTION_THRESHOLD
    );
    assert_eq!(budget.target_tokens(), DEFAULT_CONTEXT_LENGTH / 2);
}

#[test]
fn a_models_switch_scopes_the_context_override_to_the_configured_model() {
    let catalog_model = ModelInfo {
        id: "catalog-model".to_string(),
        context_length: Some(8_192),
        reasoning_efforts: None,
        reasoning_default: None,
        input_modalities: None,
    };
    let mut selection = ProviderModelOption {
        context_length_override: None,
        provider_connection: "mock".to_string(),
        model: catalog_model,
        reasoning_effort: None,
    };

    let mut unconfigured = make_engine_config();
    unconfigured.context_length = None;
    let provider_connection = unconfigured.provider_connection.clone();
    let engine = EngineImpl::new(
        Arc::new(OpenAiCompatibleProvider::new(
            "test".to_string(),
            "http://localhost:1234/v1".to_string(),
            "provider-model".to_string(),
            None,
        )),
        unconfigured,
    );
    assert_eq!(
        engine.effective_context_length(),
        DEFAULT_CONTEXT_LENGTH,
        "no profile value and no catalog value yet"
    );

    selection.provider_connection = provider_connection.clone();
    engine
        .switch_model_option(&selection)
        .expect("model selection succeeds");
    assert_eq!(
        engine.effective_context_length(),
        8_192,
        "the catalog length adopted by the switch is what budgets measure"
    );

    let mut pinned = make_engine_config();
    pinned.context_length = Some(4_096);
    selection.provider_connection = pinned.provider_connection.clone();
    let engine = EngineImpl::new(
        Arc::new(OpenAiCompatibleProvider::new(
            "test".to_string(),
            "http://localhost:1234/v1".to_string(),
            "provider-model".to_string(),
            None,
        )),
        pinned,
    );
    engine
        .switch_model_option(&selection)
        .expect("model selection succeeds");
    assert_eq!(
        engine.effective_context_length(),
        8_192,
        "a different model uses its catalog window instead of the configured model's override"
    );
    assert_eq!(engine.config().context_length, None);
    selection.model.id = "test-model".to_string();
    engine
        .switch_model_option(&selection)
        .expect("return to configured model");
    assert_eq!(engine.effective_context_length(), 4_096);
    assert_eq!(engine.config().context_length, Some(4_096));
    selection.model.id = "model-without-window".to_string();
    selection.model.context_length = None;
    engine
        .switch_model_option(&selection)
        .expect("select model without catalog window");
    assert_eq!(engine.effective_context_length(), DEFAULT_CONTEXT_LENGTH);
}

#[test]
fn engine_and_openai_compatible_provider_share_runtime_configuration() {
    let provider = Arc::new(OpenAiCompatibleProvider::new(
        "test".to_string(),
        "http://localhost:1234/v1".to_string(),
        "provider-model".to_string(),
        None,
    ));
    let config = EngineConfig {
        provider_connection: "test".to_string(),
        model: "engine-model".to_string(),
        temperature: Some(0.2),
        context_length: Some(32_000),
        api_base_url: Some("http://localhost:1234/v1".to_string()),
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    let engine = EngineImpl::new(Arc::clone(&provider) as Arc<dyn Provider>, config);

    assert_eq!(engine.config().model, "engine-model");
    assert_eq!(engine.config().temperature, Some(0.2));
    assert_eq!(engine.config().context_length, Some(32_000));

    provider
        .set_model("updated-model")
        .expect("the provider supports model switching");
    assert_eq!(engine.config().model, "updated-model");
}

#[test]
fn selecting_a_model_carries_its_input_modalities_into_runtime_state() {
    let provider = Arc::new(OpenAiCompatibleProvider::new(
        "test".to_string(),
        "http://localhost:1234/v1".to_string(),
        "text-model".to_string(),
        None,
    ));
    let engine = EngineImpl::new(provider, make_engine_config());
    assert_eq!(engine.input_modalities(), None);

    engine
        .switch_model_option(&ProviderModelOption {
            context_length_override: None,
            provider_connection: "mock".to_string(),
            model: ModelInfo {
                id: "vision-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: Some(vec![InputModality::Image]),
            },
            reasoning_effort: None,
        })
        .expect("model selection succeeds");
    assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));

    engine
        .switch_model_option(&ProviderModelOption {
            context_length_override: None,
            provider_connection: "mock".to_string(),
            model: ModelInfo {
                id: "text-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: Some(vec![]),
            },
            reasoning_effort: None,
        })
        .expect("text-only model selection succeeds");
    assert_eq!(engine.input_modalities(), Some(vec![]));
}

#[test]
fn switch_model_option_rejects_a_reasoning_effort_the_model_does_not_advertise() {
    let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
    let engine = EngineImpl::new(provider, make_engine_config());

    let error = engine
        .switch_model_option(&ProviderModelOption {
            context_length_override: None,
            provider_connection: "mock".to_string(),
            model: model_info_with_reasoning_efforts("test-model", Some(vec!["low".to_string()])),
            reasoning_effort: Some("max".to_string()),
        })
        .expect_err("an unadvertised effort must be rejected at switch time");
    assert!(
        error
            .to_string()
            .contains("does not advertise reasoning effort 'max'"),
        "the error must name the rejected effort: {error}"
    );
}
