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
