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
fn engine_config_context_length_accepts_a_plain_integer() {
    let config: EngineConfig = toml::from_str(
        r#"
provider_connection = "ollama"
model = "local"
context_length = 32000
"#,
    )
    .expect("a plain integer context_length parses");
    assert_eq!(config.context_length, Some(32_000));
}

#[test]
fn engine_config_context_length_defaults_to_none_when_unset() {
    let config: EngineConfig = toml::from_str(
        r#"
provider_connection = "ollama"
model = "local"
"#,
    )
    .expect("an EngineConfig without context_length parses");
    assert_eq!(
        config.context_length, None,
        "an unset context_length must not silently become a concrete default"
    );
}

#[test]
fn engine_config_context_length_accepts_kb_and_mb_suffixed_strings() {
    let kilobytes: EngineConfig = toml::from_str(
        r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
context_length = "265kb"
"#,
    )
    .expect("a 'kb'-suffixed context_length parses");
    assert_eq!(kilobytes.context_length, Some(265 * 1024));

    let megabytes: EngineConfig = toml::from_str(
        r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
context_length = "2mb"
"#,
    )
    .expect("an 'mb'-suffixed context_length parses");
    assert_eq!(megabytes.context_length, Some(2 * 1024 * 1024));
}

#[test]
fn engine_config_context_length_rejects_an_unparsable_string() {
    let error = toml::from_str::<EngineConfig>(
        r#"
provider_connection = "ollama"
model = "local"
context_length = "not-a-size"
"#,
    )
    .expect_err("a non-numeric, non-suffixed context_length must be rejected");
    assert!(
        error.to_string().contains("invalid context_length"),
        "the error must name the offending value: {error}"
    );
}

#[test]
fn engine_config_reasoning_seeds_the_initial_reasoning_effort() {
    let config: EngineConfig = toml::from_str(
        r#"
provider_connection = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
reasoning = "xhigh"
"#,
    )
    .expect("a reasoning field parses");
    assert_eq!(config.reasoning.as_deref(), Some("xhigh"));

    let runtime_config = EngineRuntimeConfig::from(&config);
    assert_eq!(
        runtime_config.reasoning_effort.as_deref(),
        Some("xhigh"),
        "the initial Engine runtime reasoning effort comes from the engine config"
    );
}

#[test]
fn engine_config_reasoning_defaults_to_none() {
    let config: EngineConfig = toml::from_str(
        r#"
provider_connection = "ollama"
model = "local"
"#,
    )
    .expect("an engine config without 'reasoning' still parses");
    assert_eq!(config.reasoning, None);
}

#[test]
fn effective_context_length_prefers_the_configured_toml_value() {
    let config = EngineConfig {
        context_length: Some(32_000),

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
        ..make_engine_config()
    };
    assert_eq!(config.effective_context_length(Some(999_999)), 32_000);
}

#[test]
fn effective_context_length_falls_back_to_the_catalogs_context_length() {
    let config = EngineConfig {
        context_length: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
        ..make_engine_config()
    };
    assert_eq!(config.effective_context_length(Some(64_000)), 64_000);
}

#[test]
fn effective_context_length_falls_back_to_the_built_in_default() {
    let config = EngineConfig {
        context_length: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
        ..make_engine_config()
    };
    assert_eq!(
        config.effective_context_length(None),
        DEFAULT_CONTEXT_LENGTH
    );
}

#[test]
fn compact_target_ratio_defaults_to_half_the_window_and_is_configurable_per_engine() {
    let default_config: EngineConfig =
        toml::from_str("provider = \"mock\"\nmodel = \"test-model\"\n")
            .expect("a config without compact_target_ratio parses");
    assert_eq!(
        default_config.compact_target_ratio,
        DEFAULT_COMPACT_TARGET_RATIO
    );
    assert_eq!(default_config.compact_target_ratio, 0.5);

    let configured: EngineConfig = toml::from_str(
        "provider = \"mock\"\nmodel = \"test-model\"\ncompact_target_ratio = 0.25\n",
    )
    .expect("an explicit compact_target_ratio parses");
    assert_eq!(configured.compact_target_ratio, 0.25);
}
