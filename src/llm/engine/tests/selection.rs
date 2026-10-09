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
fn engine_display_info_is_a_snapshot_not_runtime_state() {
    let provider = Arc::new(crate::llm::OpenRouterProvider::new(
        "test".to_string(),
        "http://localhost:1234/v1".to_string(),
        "provider-model".to_string(),
        None,
    ));
    let engine = EngineImpl::new(provider, make_engine_config());
    let snapshot = EngineDisplayInfo::from_engine(&engine);
    engine.switch_model("next-model").expect("model switch");
    engine
        .set_reasoning_effort(Some("low"))
        .expect("reasoning switch");

    assert_eq!(snapshot.model, "test-model");
    assert_eq!(snapshot.reasoning_effort, None);
    let refreshed = EngineDisplayInfo::from_engine(&engine);
    assert_eq!(refreshed.model, "next-model");
    assert_eq!(refreshed.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(refreshed.provider_connection, snapshot.provider_connection);
}

#[test]
fn direct_engine_previews_the_selected_effective_context_length() {
    let option = |context_length| {
        ModelSelection::ProviderModel(ProviderModelOption {
            provider_connection: "mock".to_string(),
            model: ModelInfo {
                id: "candidate".to_string(),
                context_length,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        })
    };

    let pinned_engine = CatalogEngine::new("mock", "current", &[]);
    pinned_engine
        .config
        .write()
        .expect("engine config lock")
        .context_length = Some(32_000);
    assert_eq!(
        pinned_engine
            .preview_model_selection(&option(Some(64_000)))
            .expect("pinned model preview")
            .context_length,
        64_000
    );

    let catalog_engine = CatalogEngine::new("mock", "current", &[]);
    assert_eq!(
        catalog_engine
            .preview_model_selection(&option(Some(64_000)))
            .expect("catalog model preview")
            .context_length,
        64_000
    );
    assert_eq!(
        catalog_engine
            .preview_model_selection(&option(None))
            .expect("model without catalog metadata preview")
            .context_length,
        DEFAULT_CONTEXT_LENGTH
    );
}
