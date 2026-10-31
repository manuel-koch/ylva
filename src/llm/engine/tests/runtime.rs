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

use super::*;

#[tokio::test]
async fn engine_catalog_filters_disallowed_models_without_losing_allowed_metadata() {
    let mut allowed = model_info_with_reasoning_efforts(
        "team/allowed",
        Some(vec!["low".to_string(), "high".to_string()]),
    );
    allowed.context_length = Some(128_000);
    allowed.input_modalities = Some(vec![InputModality::Image]);
    let mut denied = model_info_with_reasoning_efforts("other/denied", None);
    denied.context_length = Some(64_000);
    let provider = Arc::new(CatalogProvider::new(vec![denied, allowed]));
    let patterns = vec!["team/*".to_string()];
    let engine = EngineImpl::new(provider, make_engine_config())
        .with_model_policy(crate::config::ModelPolicy::compile(Some(&patterns)).expect("policy"));

    let options = engine.list_model_options().await.expect("catalog");

    assert_eq!(options.len(), 1);
    assert_eq!(options[0].provider_connection, "mock");
    assert_eq!(options[0].model.id, "team/allowed");
    assert_eq!(options[0].model.context_length, Some(128_000));
    assert_eq!(
        options[0].model.reasoning_efforts.as_deref(),
        Some(["low".to_string(), "high".to_string()].as_slice())
    );
    assert_eq!(
        options[0].model.input_modalities,
        Some(vec![InputModality::Image])
    );
}

#[test]
fn configured_copies_keep_provider_runtimes_independent_and_restore_original_settings() {
    for (provider, reasoning_effort) in [
        (
            Arc::new(OpenAiCompatibleProvider::new(
                "test".to_string(),
                "http://127.0.0.1:9/v1".to_string(),
                "test-model".to_string(),
                None,
            )) as Arc<dyn Provider>,
            None,
        ),
        (
            Arc::new(crate::llm::OllamaProvider::new(
                "test".to_string(),
                "http://127.0.0.1:9".to_string(),
                "test-model".to_string(),
                None,
            )),
            Some("high"),
        ),
        (
            Arc::new(crate::llm::OpenRouterProvider::new(
                "test".to_string(),
                "http://127.0.0.1:9".to_string(),
                "test-model".to_string(),
                None,
            )),
            Some("high"),
        ),
    ] {
        let mut config = make_engine_config();
        config.context_length = Some(98_304);
        let engine = EngineImpl::new(provider.clone(), config);
        engine
            .switch_model_option(&ProviderModelOption {
                context_length_override: None,
                provider_connection: "mock".to_string(),
                model: ModelInfo {
                    reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                    id: "larger-model".to_string(),
                    context_length: Some(262_144),
                    reasoning_efforts: Some(vec!["high".to_string()]),
                    reasoning_default: None,
                    input_modalities: Some(Vec::new()),
                },
                reasoning_effort: reasoning_effort.map(str::to_string),
            })
            .expect("switch conversation before making a configured copy");
        let summary = engine
            .configured_copy()
            .expect("independent configured runtime");
        assert_eq!(engine.effective_context_length(), 262_144);
        assert_eq!(summary.config().model, "test-model");
        assert_eq!(summary.effective_context_length(), 98_304);
        assert_eq!(summary.reasoning_effort(), None);
        summary
            .switch_model("summary-only-model")
            .expect("mutate the independent copy");
        assert_eq!(engine.config().model, "larger-model");
        assert_eq!(engine.reasoning_effort().as_deref(), reasoning_effort);
        assert_eq!(
            provider
                .runtime_config_handle()
                .expect("provider runtime")
                .read()
                .expect("runtime lock")
                .model,
            "larger-model",
            "the original provider must not use the summary copy's request settings"
        );
    }
}

#[test]
fn direct_model_switches_discard_previous_metadata_and_rehydrate_the_selected_window() {
    crate::test_support::run_locked(async {
        let mut configured_model = model_info_with_modalities("test-model", Some(Vec::new()));
        configured_model.context_length = Some(131_072);
        let mut larger_model = model_info_with_modalities("larger-model", Some(Vec::new()));
        larger_model.context_length = Some(262_144);
        let provider = Arc::new(CatalogProvider::new(vec![configured_model, larger_model]));
        let mut config = make_engine_config();
        config.context_length = Some(98_304);
        let engine = EngineImpl::new(provider.clone(), config);
        engine.hydrate_catalog_metadata().await;
        assert_eq!(engine.effective_context_length(), 98_304);
        engine
            .switch_model("larger-model")
            .expect("direct model switch");
        assert_eq!(engine.effective_context_length(), DEFAULT_CONTEXT_LENGTH);
        assert_eq!(engine.input_modalities(), None);
        engine.hydrate_catalog_metadata().await;
        assert_eq!(engine.effective_context_length(), 262_144);
        engine
            .switch_model("test-model")
            .expect("return to configured model");
        assert_eq!(engine.effective_context_length(), 98_304);
        engine.hydrate_catalog_metadata().await;
        assert_eq!(provider.list_models_calls(), 3);
        assert_eq!(engine.effective_context_length(), 98_304);
    });
}

#[test]
fn catalog_selection_without_optional_metadata_does_not_fetch_again() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(Vec::new()));
        let engine = EngineImpl::new(provider.clone(), make_engine_config());
        engine
            .switch_model_option(&ProviderModelOption {
                context_length_override: None,
                provider_connection: "mock".to_string(),
                model: model_info_with_modalities("catalog-selection", None),
                reasoning_effort: None,
            })
            .expect("catalog selection");
        engine.hydrate_catalog_metadata().await;
        assert_eq!(provider.list_models_calls(), 0);
        assert_eq!(engine.effective_context_length(), DEFAULT_CONTEXT_LENGTH);
    });
}

#[test]
fn engine_impl_config_returns_injected_config() {
    let provider = Arc::new(MockProvider::new(vec![], Duration::from_millis(0)));
    let config = make_engine_config();
    let engine = EngineImpl::new(provider, config.clone());

    assert_eq!(
        engine.config().provider_connection,
        config.provider_connection
    );
    assert_eq!(engine.config().model, config.model);
    assert_eq!(engine.config().context_length, None);
}

#[tokio::test]
async fn engine_impl_chat_delegates_to_mock_provider() {
    let provider = Arc::new(MockProvider::new(
        vec!["hello".to_string()],
        Duration::from_millis(0),
    ));
    let engine = EngineImpl::new(provider, make_engine_config());
    let cancel = CancellationToken::new();

    let result = engine.chat(vec![], vec![], cancel).await;
    assert!(
        result.is_ok(),
        "chat should delegate to MockProvider successfully"
    );
}

#[test]
fn engine_impl_logs_when_the_model_catalog_is_queried() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::new(vec![]));
        let engine = EngineImpl::new(provider, make_engine_config());
        let _models = engine.list_models().await.expect("catalog");
    });

    assert!(
        log.contains("model catalog queried"),
        "the catalog fetch must be logged: {log}"
    );
    assert!(log.contains("provider_connection=mock"));
}

#[test]
fn engine_impl_hydrates_catalog_metadata_before_the_first_chat() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
            "test-model",
            Some(vec![InputModality::Image]),
        )]));
        let engine = EngineImpl::new(
            Arc::clone(&provider) as Arc<dyn Provider>,
            make_engine_config(),
        );
        assert_eq!(engine.input_modalities(), None);

        // The stub's chat fails; hydration must have run regardless.
        let _ = engine.chat(vec![], vec![], CancellationToken::new()).await;

        assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));
        assert_eq!(provider.list_models_calls(), 1);
    });
}

#[test]
fn engine_impl_hydration_fetches_the_catalog_at_most_once() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
            "test-model",
            Some(vec![InputModality::Image]),
        )]));
        let engine = EngineImpl::new(provider.clone() as Arc<dyn Provider>, make_engine_config());

        engine.hydrate_catalog_metadata().await;
        engine.hydrate_catalog_metadata().await;

        assert_eq!(engine.input_modalities(), Some(vec![InputModality::Image]));
        assert_eq!(provider.list_models_calls(), 1);
    });
}

#[test]
fn engine_impl_hydration_resolves_catalog_context_length_unless_profile_overrides_it() {
    crate::test_support::run_locked(async {
        for configured_context_length in [None, Some(32_000)] {
            let mut model = model_info_with_modalities("test-model", None);
            model.context_length = Some(8_000);
            let provider = Arc::new(CatalogProvider::new(vec![model]));
            let mut config = make_engine_config();
            config.context_length = configured_context_length;
            let engine = EngineImpl::new(provider, config);
            engine.hydrate_catalog_metadata().await;
            assert_eq!(
                engine.effective_context_length(),
                configured_context_length.unwrap_or(8_000),
            );
        }
    });
}

#[test]
fn engine_impl_hydration_skips_when_modalities_are_already_known() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
            "test-model",
            Some(vec![InputModality::File]),
        )]));
        // The catalog stub advertises no reasoning efforts at all, so a
        // validation pass would drop this effort. A `/models` selection
        // supplied both the modalities and the (validated) effort, so
        // the skip path must leave it untouched without fetching.
        let engine = engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");
        engine.set_input_modalities(Some(vec![InputModality::File]));

        engine.hydrate_catalog_metadata().await;

        assert_eq!(engine.input_modalities(), Some(vec![InputModality::File]));
        assert_eq!(
            provider.list_models_calls(),
            0,
            "a /models-provided state must not trigger a catalog fetch"
        );
        assert_eq!(
            engine.reasoning_effort().as_deref(),
            Some("max"),
            "the skip path must not re-validate a switch-validated effort"
        );
    });
}

#[test]
fn engine_impl_hydration_failure_keeps_modalities_unknown_without_retries() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::failing());
        let engine = EngineImpl::new(provider.clone() as Arc<dyn Provider>, make_engine_config());

        engine.hydrate_catalog_metadata().await;
        engine.hydrate_catalog_metadata().await;

        assert_eq!(engine.input_modalities(), None);
        assert_eq!(
            provider.list_models_calls(),
            1,
            "a failed attempt must not be retried on every request"
        );
    });
    assert!(
        log.contains("failed to discover active model input modalities"),
        "the failure must be logged: {log}"
    );
}

#[test]
fn engine_impl_keeps_a_profile_reasoning_effort_the_model_advertises() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(vec![
            model_info_with_reasoning_efforts(
                "test-model",
                Some(vec!["low".to_string(), "max".to_string()]),
            ),
        ]));
        let engine = engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

        engine.hydrate_catalog_metadata().await;

        assert_eq!(
            engine.reasoning_effort().as_deref(),
            Some("max"),
            "an advertised effort must survive validation untouched"
        );
        assert!(
            engine.take_pending_notices().is_empty(),
            "a kept effort must not produce a notice"
        );
        assert_eq!(provider.list_models_calls(), 1);
    });
}

#[test]
fn engine_impl_drops_a_profile_reasoning_effort_the_model_does_not_advertise() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::new(vec![
            model_info_with_reasoning_efforts("test-model", Some(vec!["low".to_string()])),
        ]));
        let engine = engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

        engine.hydrate_catalog_metadata().await;

        assert_eq!(
            engine.reasoning_effort(),
            None,
            "an effort outside the advertised list must fall back to the provider default"
        );
        assert_eq!(
            engine.take_pending_notices(),
            vec![
                "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default."
                    .to_string()
            ],
            "the drop must be queued as a one-shot notice"
        );
        assert!(
            engine.take_pending_notices().is_empty(),
            "a notice must be handed out only once"
        );
    });
    assert!(
        log.contains("profile reasoning effort is not advertised by the model"),
        "the drop must be logged as a warning: {log}"
    );
    assert!(
        log.contains("model=test-model") && log.contains("reasoning_effort=max"),
        "the warning must name the model and the dropped effort: {log}"
    );
}

#[test]
fn engine_impl_drops_a_profile_reasoning_effort_when_the_model_advertises_no_alternatives() {
    let log = crate::test_support::capture_logs(async {
        for advertised_efforts in [Some(Vec::new()), None::<Vec<String>>] {
            let provider = Arc::new(CatalogProvider::new(vec![
                model_info_with_reasoning_efforts("test-model", advertised_efforts),
            ]));
            let engine =
                engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

            engine.hydrate_catalog_metadata().await;

            assert_eq!(
                engine.reasoning_effort(),
                None,
                "a model without selectable alternatives must not keep an explicit effort"
            );
        }
    });
    assert!(
        log.contains("profile reasoning effort is not advertised by the model"),
        "both drops must be logged: {log}"
    );
}

#[test]
fn engine_impl_keeps_a_profile_reasoning_effort_when_the_model_is_absent_from_the_catalog() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::new(vec![
            model_info_with_reasoning_efforts("other-model", Some(vec!["low".to_string()])),
        ]));
        let engine = engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

        engine.hydrate_catalog_metadata().await;

        assert_eq!(
            engine.reasoning_effort().as_deref(),
            Some("max"),
            "an unlisted model cannot disprove the effort, so it must be kept"
        );
    });
    assert!(
        log.contains("the catalog does not list the active model"),
        "the unverifiable case must be logged: {log}"
    );
}

#[test]
fn engine_impl_keeps_a_profile_reasoning_effort_when_the_catalog_fetch_fails() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::failing());
        let engine = engine_with_profile_reasoning(provider.clone() as Arc<dyn Provider>, "max");

        engine.hydrate_catalog_metadata().await;
        engine.hydrate_catalog_metadata().await;

        assert_eq!(
            engine.reasoning_effort().as_deref(),
            Some("max"),
            "a failed fetch cannot validate, so the effort must be kept"
        );
        assert_eq!(
            provider.list_models_calls(),
            1,
            "a failed attempt must not be retried on every request"
        );
    });
    assert!(
        log.contains("failed to discover active model input modalities"),
        "the failure must be logged: {log}"
    );
}

#[test]
fn context_length_override_survives_catalog_adoption_until_a_plain_selection_clears_it() {
    crate::test_support::run_locked(async {
        let mut larger_model = model_info_with_modalities("larger-model", Some(Vec::new()));
        larger_model.context_length = Some(262_144);
        let provider = Arc::new(CatalogProvider::new(vec![
            model_info_with_modalities("test-model", Some(Vec::new())),
            larger_model.clone(),
        ]));
        let engine = EngineImpl::new(provider, make_engine_config());
        let option = |context_length_override| ProviderModelOption {
            provider_connection: "mock".to_string(),
            model: larger_model.clone(),
            reasoning_effort: None,
            context_length_override,
        };

        engine
            .switch_model_option(&option(Some(65_536)))
            .expect("switch with a context override");
        assert_eq!(engine.effective_context_length(), 65_536);

        engine.adopt_catalog_context_length(Some(262_144));
        assert_eq!(
            engine.effective_context_length(),
            65_536,
            "catalog adoption must not replace a session override"
        );

        let preview = engine
            .preview_model_selection(&ModelSelection::ProviderModel(option(Some(65_536))))
            .expect("preview with a context override");
        assert_eq!(preview.context_length, 65_536);

        engine
            .switch_model_option(&option(None))
            .expect("plain catalog selection");
        assert_eq!(
            engine.effective_context_length(),
            262_144,
            "a selection without an override returns to the catalog window"
        );
    });
}

// ── allowed-model policy ──────────────────────────────────────────────────

fn restricted_engine(patterns: &[&str], model: &str) -> EngineImpl {
    restricted_engine_with_provider(patterns, model, Arc::new(CatalogProvider::new(vec![])))
}

fn restricted_engine_with_provider(
    patterns: &[&str],
    model: &str,
    provider: Arc<dyn Provider>,
) -> EngineImpl {
    let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
    let mut config = make_engine_config();
    config.model = model.to_string();
    EngineImpl::new(provider, config)
        .with_model_policy(crate::config::ModelPolicy::compile(Some(&patterns)).expect("compiles"))
}

fn option_for(model: &str, reasoning_effort: Option<&str>) -> ProviderModelOption {
    ProviderModelOption {
        context_length_override: Some(4_096),
        provider_connection: "mock".to_string(),
        model: model_info_with_reasoning_efforts(model, Some(vec!["low".to_string()])),
        reasoning_effort: reasoning_effort.map(str::to_string),
    }
}

#[test]
fn disallowed_model_selection_is_rejected_without_mutating_any_runtime_state() {
    let engine = restricted_engine(&["test-*"], "test-model");

    let error = engine
        .switch_model_option(&option_for("other-model", Some("low")))
        .expect_err("a disallowed model is rejected");

    assert!(error.to_string().contains("other-model"), "{error}");
    assert_eq!(engine.config().model, "test-model");
    assert_eq!(engine.reasoning_effort(), None, "effort untouched");
    assert_eq!(engine.effective_context_length(), DEFAULT_CONTEXT_LENGTH);
    assert_eq!(engine.input_modalities(), None);
    assert!(engine.switch_model("other-model").is_err());
    assert_eq!(engine.config().model, "test-model");
}

#[test]
fn allowed_model_selection_still_applies_with_original_spelling() {
    let engine = restricted_engine(&["TEST-*"], "test-model");

    engine
        .switch_model_option(&option_for("test-Large", None))
        .expect("an allowed model is selected");

    assert_eq!(engine.config().model, "test-Large");
}

#[tokio::test]
async fn chat_with_a_disallowed_or_unbound_model_fails_before_any_dispatch() {
    let unbound = restricted_engine_with_provider(
        &["test-*"],
        "",
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
    );
    let error = unbound
        .chat(vec![], vec![], CancellationToken::new())
        .await
        .expect_err("an unbound restricted engine cannot chat");
    assert!(
        error.to_string().contains("no model is selected"),
        "{error}"
    );

    let disallowed = restricted_engine_with_provider(
        &["test-*"],
        "other",
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
    );
    assert!(disallowed
        .chat_with_session_id(vec![], vec![], CancellationToken::new(), None)
        .await
        .is_err());
    assert!(disallowed
        .chat_with_session_id_and_estimate(
            vec![],
            vec![],
            CancellationToken::new(),
            None,
            Box::new(|_, _| Box::pin(async {})),
        )
        .await
        .is_err());
    assert!(disallowed
        .chat_with_session_id_and_diagnostics(
            vec![],
            vec![],
            CancellationToken::new(),
            None,
            Box::new(|_, _| Box::pin(async {})),
            None,
        )
        .await
        .is_err());

    let allowed = restricted_engine_with_provider(
        &["test-*"],
        "test-ok",
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
    );
    assert!(allowed
        .chat(vec![], vec![], CancellationToken::new())
        .await
        .is_ok());
}

#[test]
fn configured_copies_keep_the_model_policy() {
    let engine = restricted_engine_with_provider(
        &["test-*"],
        "test-model",
        Arc::new(OpenAiCompatibleProvider::new(
            "test".to_string(),
            "http://127.0.0.1:9/v1".to_string(),
            "test-model".to_string(),
            None,
        )),
    );

    let copy = engine.configured_copy().expect("copy");

    assert!(copy.switch_model("other").is_err());
    assert!(copy.ensure_model_allowed("test-x").is_ok());
}

#[test]
fn unrestricted_empty_engine_stays_chat_capable_for_existing_behavior() {
    let engine = EngineImpl::new(
        Arc::new(MockProvider::new(vec![], Duration::from_millis(0))),
        make_engine_config(),
    );

    assert!(engine.ensure_model_allowed("anything").is_ok());
    assert!(engine.ensure_model_allowed("").is_ok());
}

struct EdenCatalogProvider {
    runtime: Option<EngineRuntimeConfigHandle>,
    catalog: CatalogProvider,
    started: Option<Arc<tokio::sync::Notify>>,
    release: Option<Arc<tokio::sync::Notify>>,
}

impl Provider for EdenCatalogProvider {
    fn reasoning_effort_policy(&self) -> std::sync::Arc<dyn crate::llm::ReasoningEffortPolicy> {
        std::sync::Arc::new(crate::llm::edenai_provider::EdenAiReasoningEffortPolicy)
    }
    fn bind_engine_runtime(&self, runtime: EngineRuntimeConfigHandle) -> Option<Arc<dyn Provider>> {
        Some(Arc::new(EdenCatalogProvider {
            catalog: self.catalog.clone(),
            started: self.started.clone(),
            release: self.release.clone(),
            runtime: Some(runtime),
        }))
    }
    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        self.runtime.clone()
    }
    fn name(&self) -> &str {
        "arbitrary-connection"
    }
    fn set_model(&self, _model: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async {
            let (_, receiver) = mpsc::channel(1);
            Ok(receiver)
        })
    }
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async move {
            if let Some(started) = &self.started {
                started.notify_one();
            }
            if let Some(release) = &self.release {
                release.notified().await;
            }
            self.catalog.list_models().await
        })
    }
}

fn eden_catalog_provider(models: Vec<ModelInfo>, failing: bool) -> Arc<EdenCatalogProvider> {
    Arc::new(EdenCatalogProvider {
        catalog: if failing {
            CatalogProvider::failing()
        } else {
            CatalogProvider::new(models)
        },
        runtime: None,
        started: None,
        release: None,
    })
}

#[tokio::test]
async fn edenai_profile_hydration_keeps_unverified_for_incomplete_missing_or_failed_catalog() {
    for (models, failing, known_modalities) in [
        (
            vec![model_info_with_modalities("test-model", Some(vec![]))],
            false,
            false,
        ),
        (vec![], false, false),
        (vec![], true, false),
        (
            vec![model_info_with_modalities("test-model", Some(vec![]))],
            false,
            true,
        ),
    ] {
        let provider = eden_catalog_provider(models, failing);
        let engine = engine_with_profile_reasoning(provider.clone(), "medium");
        if known_modalities {
            engine.set_input_modalities(Some(vec![]));
        }
        for _ in 0..2 {
            engine
                .chat(vec![], vec![], CancellationToken::new())
                .await
                .unwrap();
        }
        assert_eq!(engine.reasoning_effort().as_deref(), Some("medium"));
        assert_eq!(
            engine.effective_reasoning_provenance(),
            crate::llm::ReasoningEffortProvenance::Unverified
        );
        assert_eq!(provider.catalog.list_models_calls(), 1);
        assert_eq!(engine.take_pending_notices().len(), 1);
    }
}

#[tokio::test]
async fn edenai_invalid_profile_is_rejected_without_catalog_fetch_even_with_known_modalities() {
    for known_modalities in [true, false] {
        let provider = eden_catalog_provider(vec![], true);
        let engine = engine_with_profile_reasoning(provider.clone(), "med");
        if known_modalities {
            engine.set_input_modalities(Some(vec![]));
        }
        let error = engine
            .chat(vec![], vec![], CancellationToken::new())
            .await
            .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("mock")
                && rendered.contains("test-model")
                && rendered.contains("med")
        );
        assert_eq!(provider.catalog.list_models_calls(), 0);
        assert_eq!(engine.reasoning_effort().as_deref(), Some("med"));
    }
}

#[tokio::test]
async fn edenai_unsupported_profile_fails_repeated_chats_without_resetting_effort() {
    let mut unsupported = model_info_with_modalities("test-model", Some(vec![]));
    unsupported.reasoning_evidence = crate::llm::ReasoningEvidence::Unsupported;
    let provider = eden_catalog_provider(vec![unsupported], false);
    let engine = engine_with_profile_reasoning(provider.clone(), "medium");
    for _ in 0..2 {
        let error = engine
            .chat(vec![], vec![], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("Unsupported"));
    }
    assert_eq!(engine.reasoning_effort().as_deref(), Some("medium"));
    assert_eq!(provider.catalog.list_models_calls(), 1);
}

#[tokio::test]
async fn stale_hydration_cannot_overwrite_a_new_selection() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut old_model = model_info_with_modalities("test-model", Some(vec![InputModality::Image]));
    old_model.context_length = Some(11);
    old_model.reasoning_evidence = crate::llm::ReasoningEvidence::Unsupported;
    let provider = Arc::new(EdenCatalogProvider {
        catalog: CatalogProvider::new(vec![old_model]),
        runtime: None,
        started: Some(started.clone()),
        release: Some(release.clone()),
    });
    let engine = Arc::new(engine_with_profile_reasoning(provider, "medium"));
    let hydration_engine = engine.clone();
    let hydration = tokio::spawn(async move {
        hydration_engine.hydrate_catalog_metadata().await;
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    engine
        .switch_model_option(&ProviderModelOption {
            provider_connection: "mock".into(),
            model: model_info_with_modalities("new-model", Some(vec![])),
            reasoning_effort: Some("low".into()),
            context_length_override: Some(999),
        })
        .unwrap();
    let selected = engine.runtime_snapshot().unwrap();
    release.notify_one();
    hydration.await.unwrap();
    assert_eq!(engine.runtime_snapshot().unwrap(), selected);
}

#[test]
fn edenai_handles_and_warning_state_are_independent_but_shared_engine_deduplicates() {
    let provider = Arc::new(crate::llm::EdenAiProvider::new(
        "shared".into(),
        "http://localhost:1".into(),
        "test-model".into(),
        None,
    ));
    let first = engine_with_profile_reasoning(provider.clone(), "medium");
    let second = engine_with_profile_reasoning(provider, "high");
    assert_eq!(first.reasoning_effort().as_deref(), Some("medium"));
    assert_eq!(second.reasoning_effort().as_deref(), Some("high"));
    for engine in [&first, &second] {
        engine.prepare_request_notices().unwrap();
        engine.prepare_request_notices().unwrap();
        assert_eq!(engine.take_pending_notices().len(), 1);
        engine.set_reasoning_effort(None).unwrap();
        engine.set_reasoning_effort(Some("medium")).unwrap();
        engine.prepare_request_notices().unwrap();
        assert_eq!(engine.take_pending_notices().len(), 1);
    }
    let copy = first.configured_copy().unwrap();
    assert_eq!(copy.reasoning_effort().as_deref(), Some("medium"));
    copy.set_reasoning_effort(Some("low")).unwrap();
    assert_eq!(first.reasoning_effort().as_deref(), Some("medium"));
    copy.prepare_request_notices().unwrap();
    assert_eq!(copy.take_pending_notices().len(), 1);
}

#[tokio::test]
async fn edenai_omitted_profile_stays_default_even_with_unsupported_catalog() {
    let mut unsupported = model_info_with_modalities("test-model", Some(vec![]));
    unsupported.reasoning_evidence = crate::llm::ReasoningEvidence::Unsupported;
    let provider = eden_catalog_provider(vec![unsupported], false);
    let engine = EngineImpl::new(provider.clone(), make_engine_config());
    engine
        .chat(vec![], vec![], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(engine.reasoning_effort(), None);
    assert_eq!(
        engine.effective_reasoning_provenance(),
        crate::llm::ReasoningEffortProvenance::ProviderDefault
    );
    assert!(engine.take_pending_notices().is_empty());
    assert_eq!(provider.catalog.list_models_calls(), 1);
}

#[test]
fn independent_runtime_binding_does_not_depend_on_reasoning_policy() {
    let provider = Arc::new(RuntimeBoundProvider {
        runtime: Arc::new(RwLock::new(
            EngineRuntimeConfig::from(&make_engine_config()),
        )),
    });
    let first = engine_with_profile_reasoning(provider.clone(), "first");
    let second = engine_with_profile_reasoning(provider, "second");
    assert_eq!(first.reasoning_effort().as_deref(), Some("first"));
    assert_eq!(second.reasoning_effort().as_deref(), Some("second"));
    first.switch_model("first-model").unwrap();
    assert_eq!(second.config().model, "test-model");
}

struct RuntimeBoundProvider {
    runtime: EngineRuntimeConfigHandle,
}

impl Provider for RuntimeBoundProvider {
    fn bind_engine_runtime(&self, runtime: EngineRuntimeConfigHandle) -> Option<Arc<dyn Provider>> {
        Some(Arc::new(Self { runtime }))
    }

    fn runtime_config_handle(&self) -> Option<EngineRuntimeConfigHandle> {
        Some(self.runtime.clone())
    }

    fn name(&self) -> &str {
        "neutral-runtime"
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async { anyhow::bail!("not used") })
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![model_info_with_modalities("test-model", Some(vec![]))]) })
    }
}

#[derive(Debug)]
struct NeutralReasoningPolicy;

impl crate::llm::ReasoningEffortPolicy for NeutralReasoningPolicy {
    fn resolve(
        &self,
        _model_id: &str,
        _model: Option<&ModelInfo>,
        effort: Option<&str>,
    ) -> anyhow::Result<crate::llm::ReasoningEffortProvenance> {
        match effort {
            None => Ok(crate::llm::ReasoningEffortProvenance::ProviderDefault),
            Some("custom") => Ok(crate::llm::ReasoningEffortProvenance::Unverified),
            Some(_) => anyhow::bail!("neutral contract rejection"),
        }
    }

    fn validate_request(
        &self,
        model_id: &str,
        model: Option<&ModelInfo>,
        effort: Option<&str>,
    ) -> anyhow::Result<()> {
        self.resolve(model_id, model, effort).map(|_| ())
    }

    fn requires_catalog_evidence(&self) -> bool {
        true
    }

    fn shows_capability_evidence(&self) -> bool {
        true
    }

    fn retain_profile_effort(&self, _model: &ModelInfo, _effort: &str) -> bool {
        true
    }

    fn unverified_choices(&self, _model: Option<&ModelInfo>) -> &'static [&'static str] {
        &["custom"]
    }

    fn unverified_notice(&self, model: &str, effort: &str) -> Option<String> {
        Some(format!("Neutral selection {model}/{effort} is unverified"))
    }
}

struct NeutralPolicyProvider(CatalogProvider);

impl Provider for NeutralPolicyProvider {
    fn reasoning_effort_policy(&self) -> std::sync::Arc<dyn crate::llm::ReasoningEffortPolicy> {
        std::sync::Arc::new(NeutralReasoningPolicy)
    }

    fn set_reasoning_effort(&self, _effort: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn name(&self) -> &str {
        "neutral-contract"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        self.0.list_models()
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async {
            let (_, receiver) = mpsc::channel(1);
            Ok(receiver)
        })
    }
}

#[tokio::test]
async fn neutral_reasoning_contract_controls_validation_hydration_and_notices_without_runtime_binding(
) {
    let provider = Arc::new(NeutralPolicyProvider(CatalogProvider::new(vec![
        model_info_with_modalities("test-model", Some(vec![])),
    ])));
    let engine = engine_with_profile_reasoning(provider.clone(), "custom");
    engine.set_input_modalities(Some(vec![]));
    for _ in 0..2 {
        engine
            .chat(vec![], vec![], CancellationToken::new())
            .await
            .unwrap();
    }
    assert_eq!(engine.reasoning_effort().as_deref(), Some("custom"));
    assert_eq!(provider.0.list_models_calls(), 1);
    assert_eq!(
        engine.take_pending_notices(),
        vec!["Neutral selection test-model/custom is unverified"]
    );
    let before = engine.runtime_snapshot();
    assert!(engine.set_reasoning_effort(Some("medium")).is_err());
    assert_eq!(engine.runtime_snapshot(), before);
    engine.set_reasoning_effort(None).unwrap();
    engine.set_reasoning_effort(Some("custom")).unwrap();
    engine.prepare_request_notices().unwrap();
    assert_eq!(engine.take_pending_notices().len(), 1);
    let option = ProviderModelOption {
        provider_connection: "mock".into(),
        model: model_info_with_modalities("test-model", None),
        reasoning_effort: Some("custom".into()),
        context_length_override: None,
    };
    let options = engine
        .selection_reasoning_options(&ModelSelection::ProviderModel(option.clone()))
        .unwrap();
    assert_eq!(options.unverified_efforts, ["custom"]);
    assert!(options.show_capability);
    assert_eq!(
        engine
            .resolve_selection_reasoning(&ModelSelection::ProviderModel(option))
            .unwrap(),
        crate::llm::ReasoningEffortProvenance::Unverified
    );

    let invalid_provider = Arc::new(NeutralPolicyProvider(CatalogProvider::failing()));
    let invalid = engine_with_profile_reasoning(invalid_provider.clone(), "medium");
    invalid.hydrate_catalog_metadata().await;
    assert!(invalid
        .chat(vec![], vec![], CancellationToken::new())
        .await
        .is_err());
    assert_eq!(invalid_provider.0.list_models_calls(), 0);
    assert_eq!(invalid.reasoning_effort().as_deref(), Some("medium"));
}

#[tokio::test]
async fn independently_bound_runtime_retains_strict_profile_fallback_and_atomic_rejection() {
    let provider = Arc::new(RuntimeBoundProvider {
        runtime: Arc::new(RwLock::new(
            EngineRuntimeConfig::from(&make_engine_config()),
        )),
    });
    let engine = engine_with_profile_reasoning(provider, "unadvertised");
    let before = engine.runtime_snapshot();
    assert!(engine
        .switch_model_option(&ProviderModelOption {
            provider_connection: "mock".into(),
            model: model_info_with_modalities("new-model", Some(vec![])),
            reasoning_effort: Some("unadvertised".into()),
            context_length_override: Some(999),
        })
        .is_err());
    assert_eq!(engine.runtime_snapshot(), before);
    engine.hydrate_catalog_metadata().await;
    assert_eq!(engine.reasoning_effort(), None);
    assert_eq!(engine.take_pending_notices().len(), 1);
}
