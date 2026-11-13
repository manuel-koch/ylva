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
use crate::llm::model_scores::parse_model_scores;
use crate::llm::{EngineConfig, ModelInfo};
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};

fn option(connection: &str, model: &str) -> ProviderModelOption {
    ProviderModelOption {
        provider_connection: connection.to_string(),
        model: ModelInfo {
            id: model.to_string(),
            context_length: Some(32_000),
            max_output_tokens: Some(4_000),
            reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
            reasoning_evidence: ReasoningEvidence::Supported,
            reasoning_default: None,
            pricing: None,
            score: None,
            input_modalities: None,
        },
        reasoning_effort: None,
        context_length_override: None,
        max_output_tokens_override: None,
    }
}

fn catalog(connection: &str, models: &[&str]) -> ConnectionCatalog {
    ConnectionCatalog {
        provider_connection: connection.to_string(),
        result: Ok(models
            .iter()
            .map(|model| option(connection, model))
            .collect()),
    }
}

#[test]
fn prepares_unique_favorites_in_configured_order_then_sorted_remaining_models_with_scores() {
    let mut listing = catalog("alpha", &["zulu", "vendor/beta", "apple", "zulu", "first"]);
    let ranks = HashMap::from([
        (("alpha".to_string(), "first".to_string()), 0),
        (("alpha".to_string(), "zulu".to_string()), 1),
    ]);
    let scores = parse_model_scores("beta = 8.5\ninvalid = 'not a score'");
    assert!(!scores.problems.is_empty());
    prepare_catalog(&mut listing, &ranks, &scores.scores);
    let models = listing.result.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|option| option.model.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "zulu", "apple", "vendor/beta"]
    );
    assert_eq!(models[3].model.score.as_ref().unwrap().summary(), "8.5");
    assert!(models[0].model.score.is_none());
}

#[test]
fn renders_safe_grouped_model_and_detail_lines_without_truncating_slash_identifiers() {
    let mut first = catalog("alpha\n", &["vendor/短", "longer\r\x1b[31m"]);
    first.result.as_mut().unwrap()[0].model.reasoning_efforts =
        Some(vec!["bad\n\x1b[2J\u{202e}".to_string()]);
    let second = catalog("beta", &[]);
    let mut output = Vec::new();
    assert_eq!(
        render_catalogs(
            &[first, second],
            &HashMap::new(),
            ModelsListOptions::default(),
            &mut output,
            &mut Vec::new()
        ),
        Ok(0)
    );
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("alpha\\n:\n"), "{text}");
    assert!(text.contains("   vendor/短\n      "), "{text}");
    assert!(text.contains("longer\\r\\u{1b}[31m"), "{text}");
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\r'));
    assert!(text.contains("\n\nbeta:\n  no models reported\n"));
    assert!(text.lines().all(|line| !line.contains(" · ")));
    assert!(text.contains("\n      context: "), "{text}");
}

#[test]
fn color_is_intentional_only_and_favorite_marker_is_rendered() {
    let listing = catalog("alpha", &["vendor/model\x1b"]);
    let ranks = HashMap::from([(("alpha".to_string(), "vendor/model\x1b".to_string()), 0)]);
    let mut output = Vec::new();
    render_connection(
        &listing,
        &ranks,
        ModelsListOptions {
            color: true,
            ..ModelsListOptions::default()
        },
        &mut output,
    )
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("⚑ \x1b[1mvendor/model\\u{1b}\x1b[0m"),
        "{text}"
    );
    assert_eq!(text.matches('\x1b').count(), 2);
}

#[test]
fn evidence_labels_are_unambiguous_and_unknown_efforts_are_unavailable() {
    let mut model = option("alpha", "one");
    for (evidence, label) in [
        (ReasoningEvidence::Unsupported, "unsupported"),
        (ReasoningEvidence::Invalid, "invalid metadata"),
        (ReasoningEvidence::Conflicting, "conflicting metadata"),
    ] {
        model.model.reasoning_evidence = evidence;
        let details = model_details(&model);
        assert!(details.contains(label));
        assert_eq!(details.matches("reasoning:").count(), 1);
    }
    model.model.reasoning_evidence = ReasoningEvidence::Unknown;
    model.model.reasoning_efforts = None;
    assert!(model_details(&model).contains("reasoning: unavailable"));
}

#[test]
fn default_reasoning_effort_is_prefixed_with_an_asterisk() {
    let mut model = option("alpha", "one");
    model.model.reasoning_efforts = Some(vec!["low".to_string(), "medium".to_string()]);
    model.model.reasoning_default = Some("medium".to_string());
    assert!(model_details(&model).contains("reasoning: low/*medium"));
    model.model.reasoning_default = None;
    assert!(model_details(&model).contains("reasoning: low/medium"));
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken writer"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn exit_codes_distinguish_empty_success_partial_outage_and_total_outage() {
    let failure = || ConnectionCatalog {
        provider_connection: "bad".to_string(),
        result: Err("failed\nunsafe\x1b".to_string()),
    };
    for (catalogs, expected) in [
        (vec![catalog("empty", &[])], 0),
        (vec![failure(), catalog("empty", &[])], 0),
        (vec![failure()], 1),
    ] {
        let mut diagnostics = Vec::new();
        assert_eq!(
            render_catalogs(
                &catalogs,
                &HashMap::new(),
                ModelsListOptions::default(),
                &mut Vec::new(),
                &mut diagnostics
            ),
            Ok(expected)
        );
        assert!(!diagnostics.contains(&b'\x1b'));
    }
    assert!(render_catalogs(
        &[failure()],
        &HashMap::new(),
        ModelsListOptions::default(),
        &mut Vec::new(),
        &mut BrokenWriter
    )
    .unwrap_err()
    .contains("diagnostics"));
    assert!(render_catalogs(
        &[catalog("empty", &[])],
        &HashMap::new(),
        ModelsListOptions::default(),
        &mut BrokenWriter,
        &mut Vec::new()
    )
    .unwrap_err()
    .contains("listing"));
    assert!(render_catalogs(
        &[catalog("one", &[]), catalog("two", &[])],
        &HashMap::new(),
        ModelsListOptions::default(),
        &mut BrokenWriter,
        &mut Vec::new()
    )
    .is_err());
}

struct CatalogEngine {
    calls: AtomicUsize,
    failure: bool,
    delay: bool,
    partial_failure: bool,
}

impl Engine for CatalogEngine {
    fn config(&self) -> EngineConfig {
        toml::from_str("provider_connection = 'alpha'\nmodel = 'one'").unwrap()
    }
    fn chat<'a>(
        &'a self,
        _: Vec<crate::agent::Message>,
        _: Vec<crate::llm::ToolSchema>,
        _: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<tokio::sync::mpsc::Receiver<crate::agent::StreamEvent>>> {
        Box::pin(async { anyhow::bail!("not used") })
    }
    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_model_options_with_failures(
        &self,
    ) -> BoxFuture<'_, anyhow::Result<(Vec<ProviderModelOption>, Vec<String>)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.delay {
                std::future::pending::<()>().await;
            }
            if self.failure {
                anyhow::bail!("https://secret:credential@private-endpoint?token=hidden\n\x1b");
            }
            Ok((
                vec![option("alpha", "one")],
                if self.partial_failure {
                    vec!["secret failure".to_string()]
                } else {
                    Vec::new()
                },
            ))
        })
    }
    fn switch_model(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn favorite_models(&self) -> Vec<(String, Vec<String>)> {
        vec![(
            "alpha".to_string(),
            vec![
                "one".to_string(),
                "one".to_string(),
                "forbidden".to_string(),
            ],
        )]
    }
    fn ensure_model_allowed(&self, model: &str) -> anyhow::Result<()> {
        anyhow::ensure!(model != "forbidden", "forbidden");
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn bounded_catalog_fetch_does_not_expose_provider_error_or_partial_failure_details() {
    for (failure, delay, partial_failure) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let engine = Arc::new(CatalogEngine {
            calls: AtomicUsize::new(0),
            failure,
            delay,
            partial_failure,
        });
        let result = fetch_connection_catalog("alpha\n", engine.clone(), false).await;
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
        match result.result {
            Ok(models) => assert_eq!(models.len(), 1),
            Err(error) => {
                assert!(!error.contains("secret"));
                assert!(!error.contains("credential"));
                assert!(!error.contains('\n'));
                assert!(!error.contains('\x1b'));
                assert_eq!(error.contains("timed out"), delay);
            }
        }
    }
}

#[test]
fn favorite_ranks_preserve_first_duplicate_and_exclude_disallowed_models() {
    let engine: Arc<dyn Engine> = Arc::new(CatalogEngine {
        calls: AtomicUsize::new(0),
        failure: false,
        delay: false,
        partial_failure: false,
    });
    let ranks = favorite_ranks(&[("alpha".to_string(), engine)]);
    assert_eq!(
        ranks.get(&("alpha".to_string(), "one".to_string())),
        Some(&0)
    );
    assert!(!ranks.contains_key(&("alpha".to_string(), "forbidden".to_string())));
}

#[test]
fn run_lists_mock_connections_and_rejects_an_invalid_profile() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK.lock().unwrap();
    let mut profile = crate::config::ProfileConfig::default();
    profile.provider_connections.insert(
        "alpha".to_string(),
        crate::config::ProviderConnectionConfig {
            provider: crate::config::ProviderKind::Mock,
            api_base_url: None,
            api_key: None,
            favorite_models: Vec::new(),
            allowed_models: None,
        },
    );
    let mut output = Vec::new();
    assert_eq!(
        run_with_options(
            &profile,
            ModelsListOptions::default(),
            &mut output,
            &mut Vec::new()
        ),
        Ok(0)
    );
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("alpha:\n  no models reported"));
    assert_eq!(
        run_with_options(
            &profile,
            ModelsListOptions::default(),
            &mut BrokenWriter,
            &mut Vec::new()
        ),
        Ok(1)
    );
    profile.default_engine = "missing".to_string();
    profile.engines.insert(
        "other".to_string(),
        toml::from_str("provider_connection='alpha'").unwrap(),
    );
    assert!(run_with_options(
        &profile,
        ModelsListOptions::default(),
        &mut Vec::new(),
        &mut Vec::new()
    )
    .is_err());
}
