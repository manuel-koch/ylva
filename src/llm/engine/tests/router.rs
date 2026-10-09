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
fn engine_router_logs_the_selected_named_engine_for_chat_requests() {
    let log = crate::test_support::capture_logs(async {
        let engine: Arc<dyn Engine> =
            Arc::new(CatalogEngine::new("ollama-local", "local-model", &[]));
        let router = EngineRouter::new(HashMap::from([("local".to_string(), engine)]), "local")
            .expect("router construction");

        let _request = router.chat(vec![], vec![], CancellationToken::new()).await;
    });

    assert!(log.contains("named engine selected for chat request"));
    assert!(log.contains("engine=local"));
    assert!(log.contains("provider_connection=ollama-local"));
    assert!(log.contains("model=local-model"));
    assert!(
        log.contains("reasoning_effort=default"),
        "the selection log must carry the thinking mode: {log}"
    );
    assert!(
        log.contains("input_modalities=unknown"),
        "the selection log must carry the modalities: {log}"
    );
}

#[test]
fn engine_router_logs_thinking_and_modalities_after_hydration() {
    let log = crate::test_support::capture_logs(async {
        let provider = Arc::new(CatalogProvider::new(vec![model_info_with_modalities(
            "local-model",
            Some(vec![InputModality::Image]),
        )]));
        let mut config = make_engine_config();
        config.provider_connection = "ollama-local".to_string();
        config.model = "local-model".to_string();
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, config));
        let router = EngineRouter::new(HashMap::from([("local".to_string(), engine)]), "local")
            .expect("router construction");

        let _request = router.chat(vec![], vec![], CancellationToken::new()).await;
    });

    assert!(log.contains("named engine selected for chat request"));
    assert!(
        log.contains("input_modalities=image"),
        "hydration must complete before the selection is logged: {log}"
    );
    assert!(log.contains("active model input modalities discovered"));
}

#[test]
fn engine_router_drains_pending_notices_from_its_engines() {
    crate::test_support::run_locked(async {
        let provider = Arc::new(CatalogProvider::new(vec![
            model_info_with_reasoning_efforts("test-model", Some(vec!["low".to_string()])),
        ]));
        let engine = engine_with_profile_reasoning(provider as Arc<dyn Provider>, "max");
        engine.hydrate_catalog_metadata().await;
        let router = EngineRouter::new(
            HashMap::from([("mock".to_string(), Arc::new(engine) as Arc<dyn Engine>)]),
            "mock",
        )
        .expect("router construction");

        assert_eq!(
            router.take_pending_notices().len(),
            1,
            "the router must surface the notice its engine queued"
        );
        assert!(
            router.take_pending_notices().is_empty(),
            "draining must be one-shot through the router too"
        );
    });
}

#[tokio::test]
async fn engine_router_lists_all_providers_and_switches_the_active_engine() {
    let local: Arc<dyn Engine> =
        Arc::new(CatalogEngine::new("ollama", "local", &["local", "other"]));
    let hosted: Arc<dyn Engine> = Arc::new(CatalogEngine::new("openrouter", "hosted", &["hosted"]));
    let router = EngineRouter::new(
        HashMap::from([
            ("default".to_string(), local),
            ("openrouter".to_string(), hosted),
        ]),
        "default",
    )
    .expect("router construction");

    let options = router
        .list_model_options()
        .await
        .expect("all model lists should load");
    assert_eq!(
        options
            .iter()
            .map(|option| { format!("{}/{}", option.provider_connection, option.model.id) })
            .collect::<Vec<_>>(),
        vec![
            "ollama/local".to_string(),
            "ollama/other".to_string(),
            "openrouter/hosted".to_string()
        ]
    );

    let hosted_option = options
        .iter()
        .find(|option| option.provider_connection == "openrouter")
        .expect("hosted option");
    router
        .switch_model_option(hosted_option)
        .expect("provider/model switch");
    assert_eq!(router.config().provider_connection, "openrouter");
    assert_eq!(router.config().model, "hosted");
}

#[test]
fn engine_router_previews_the_same_target_used_for_named_and_catalog_selection() {
    let engine_a = CatalogEngine::new("ollama", "shared", &[]);
    engine_a
        .config
        .write()
        .expect("engine config lock")
        .context_length = Some(32_000);
    let engine_a: Arc<dyn Engine> = Arc::new(engine_a);
    let engine_b: Arc<dyn Engine> = Arc::new(CatalogEngine::new("ollama", "shared", &[]));
    let engine_c: Arc<dyn Engine> = Arc::new(CatalogEngine::new("openrouter", "hosted", &[]));
    let router = EngineRouter::new(
        HashMap::from([
            ("engine-a".to_string(), engine_a),
            ("engine-b".to_string(), engine_b),
            ("engine-c".to_string(), engine_c),
        ]),
        "engine-a",
    )
    .expect("router construction");
    let option = |connection: &str, model: &str| ProviderModelOption {
        provider_connection: connection.to_string(),
        model: ModelInfo {
            id: model.to_string(),
            context_length: Some(64_000),
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    let named_target = router
        .preview_model_selection(&ModelSelection::NamedEngine {
            engine_name: "engine-b".to_string(),
            option: option("ollama", "shared"),
            reasoning_effort: ReasoningEffortChange::Unchanged,
        })
        .expect("named target preview");
    assert_eq!(named_target.engine_name.as_deref(), Some("engine-b"));
    assert_eq!(named_target.context_length, DEFAULT_CONTEXT_LENGTH);
    assert_eq!(
        router.active_engine_name().as_deref(),
        Some("engine-a"),
        "previewing a named shortcut must not change the active engine"
    );

    let active_connection_target = router
        .preview_model_selection(&ModelSelection::ProviderModel(option(
            "ollama",
            "catalog-model",
        )))
        .expect("active connection target preview");
    assert_eq!(
        active_connection_target.engine_name.as_deref(),
        Some("engine-a"),
        "raw rows on the active connection must target the active named engine"
    );
    assert_eq!(
        active_connection_target.context_length, 64_000,
        "another model uses its catalog window instead of the configured override"
    );

    let other_connection_target = router
        .preview_model_selection(&ModelSelection::ProviderModel(option(
            "openrouter",
            "another-model",
        )))
        .expect("catalog target preview");
    assert_eq!(
        other_connection_target.engine_name.as_deref(),
        Some("engine-c")
    );
    assert_eq!(
        other_connection_target.model, "another-model",
        "catalog preview carries the requested model on the routed engine"
    );
    assert_eq!(other_connection_target.context_length, 64_000);
    assert_eq!(
        router.active_engine_name().as_deref(),
        Some("engine-a"),
        "previewing a catalog row must not change the active engine"
    );

    for selection in [
        ModelSelection::NamedEngine {
            engine_name: "engine-b".to_string(),
            option: option("ollama", "shared"),
            reasoning_effort: ReasoningEffortChange::Unchanged,
        },
        ModelSelection::ProviderModel(option("ollama", "catalog-model")),
        ModelSelection::ProviderModel(option("openrouter", "another-model")),
    ] {
        let target = router
            .preview_model_selection(&selection)
            .expect("selection preview");
        router
            .select_model(&selection)
            .expect("selection activation");
        assert_eq!(router.active_engine_name(), target.engine_name);
        assert_eq!(
            router.config().provider_connection,
            target.provider_connection
        );
        assert_eq!(router.config().model, target.model);
    }
}

#[test]
fn named_engine_selection_rejects_invalid_targets_without_switching() {
    let engine_a: Arc<dyn Engine> = Arc::new(CatalogEngine::new("mock", "shared", &[]));
    let engine_b: Arc<dyn Engine> = Arc::new(CatalogEngine::new("mock", "shared", &[]));
    let router = EngineRouter::new(
        HashMap::from([
            ("engine-a".to_string(), engine_a),
            ("engine-b".to_string(), engine_b),
        ]),
        "engine-a",
    )
    .expect("router construction");
    let option = |provider_connection: &str, reasoning_efforts: Option<Vec<&str>>, id: &str| {
        ProviderModelOption {
            provider_connection: provider_connection.to_string(),
            model: ModelInfo {
                id: id.to_string(),
                context_length: None,
                reasoning_efforts: reasoning_efforts
                    .map(|efforts| efforts.into_iter().map(str::to_string).collect()),
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        }
    };

    let unknown_engine = ModelSelection::NamedEngine {
        engine_name: "missing".to_string(),
        option: option("mock", None, "shared"),
        reasoning_effort: ReasoningEffortChange::Unchanged,
    };
    assert!(
        router.select_model(&unknown_engine).is_err(),
        "an unknown engine name must be rejected"
    );

    let wrong_connection = ModelSelection::NamedEngine {
        engine_name: "engine-b".to_string(),
        option: option("other", None, "shared"),
        reasoning_effort: ReasoningEffortChange::Unchanged,
    };
    assert!(
        router.select_model(&wrong_connection).is_err(),
        "a named shortcut cannot target another provider connection"
    );

    let unsupported_effort = ModelSelection::NamedEngine {
        engine_name: "engine-b".to_string(),
        option: option("mock", Some(vec!["low"]), "shared"),
        reasoning_effort: ReasoningEffortChange::Set(Some("max".to_string())),
    };
    assert!(
        router.select_model(&unsupported_effort).is_err(),
        "an edited effort must be advertised by the selected model"
    );
    assert_eq!(
        router.active_engine_name().as_deref(),
        Some("engine-a"),
        "failed selection must not change the active engine"
    );
}

#[tokio::test]
async fn engine_router_lists_each_named_provider_connection_once() {
    let local_default: Arc<dyn Engine> =
        Arc::new(CatalogEngine::new("ollama-local", "local", &["local"]));
    let local_alternate: Arc<dyn Engine> = Arc::new(CatalogEngine::new(
        "ollama-local",
        "alternate",
        &["alternate"],
    ));
    let cloud: Arc<dyn Engine> = Arc::new(CatalogEngine::new("ollama-cloud", "cloud", &["cloud"]));
    let router = EngineRouter::new(
        HashMap::from([
            ("local".to_string(), local_default),
            ("alternate".to_string(), local_alternate),
            ("cloud".to_string(), cloud),
        ]),
        "local",
    )
    .expect("router construction");

    let options = router
        .list_model_options()
        .await
        .expect("provider model lists should load");

    assert_eq!(
        options
            .iter()
            .map(|option| { format!("{}/{}", option.provider_connection, option.model.id) })
            .collect::<Vec<_>>(),
        vec![
            "ollama-cloud/cloud".to_string(),
            "ollama-local/local".to_string(),
        ]
    );
}

#[tokio::test]
async fn engine_router_keeps_successful_models_when_one_provider_fails() {
    let failing: Arc<dyn Engine> = Arc::new(CatalogEngine::failing("ollama", "server unavailable"));
    let working: Arc<dyn Engine> =
        Arc::new(CatalogEngine::new("openrouter", "hosted", &["hosted"]));
    let router = EngineRouter::new(
        HashMap::from([
            ("default".to_string(), failing),
            ("openrouter".to_string(), working),
        ]),
        "default",
    )
    .expect("router construction");

    let options = router
        .list_model_options()
        .await
        .expect("one provider succeeding should be enough");

    assert_eq!(options.len(), 1);
    assert_eq!(options[0].provider_connection, "openrouter");
    assert_eq!(options[0].model.id, "hosted");
}

#[tokio::test]
async fn engine_router_reports_an_aggregate_error_when_all_providers_fail() {
    let router = EngineRouter::new(
        HashMap::from([
            (
                "local".to_string(),
                Arc::new(CatalogEngine::failing("ollama", "server unavailable")) as Arc<dyn Engine>,
            ),
            (
                "hosted".to_string(),
                Arc::new(CatalogEngine::failing("openrouter", "unauthorized")) as Arc<dyn Engine>,
            ),
        ]),
        "local",
    )
    .expect("router construction");

    let error = router
        .list_model_options()
        .await
        .expect_err("all provider failures should be reported");
    assert_eq!(
        error.to_string(),
        "no configured provider connection returned models: ollama: server unavailable; openrouter: unauthorized"
    );
}
