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

//! The model-change and compaction history shown by `/status`.

use crate::store::{CompactionEffect, CompactionRecord, ModelParameters, StoreEvent};

/// One step in the history of an agent: a model becoming active or a
/// compaction being applied. `at` is the RFC 3339 UTC event timestamp, empty
/// when the history was not read from a persisted session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusTimelineEntry {
    Model {
        at: String,
        provider_connection: String,
        model: String,
        parameters: ModelParameters,
    },
    Compaction {
        at: String,
        /// `None` when the effect was not recorded.
        effect: Option<CompactionEffect>,
    },
}

/// The timeline of persisted `events`, oldest first. A model entry identical
/// to the one before it (every process start records one) is dropped.
pub(crate) fn timeline_from_events(events: &[StoreEvent]) -> Vec<StatusTimelineEntry> {
    let mut timeline: Vec<StatusTimelineEntry> = Vec::new();
    for event in events {
        let entry = match event {
            StoreEvent::ModelChanged {
                changed_at,
                provider_connection,
                model,
                parameters,
                ..
            } => StatusTimelineEntry::Model {
                at: changed_at.clone(),
                provider_connection: provider_connection.clone(),
                model: model.clone(),
                parameters: parameters.clone(),
            },
            StoreEvent::Compaction {
                compacted_at,
                estimated_tokens_before,
                estimated_tokens_after,
                ..
            } => StatusTimelineEntry::Compaction {
                at: compacted_at.clone(),
                effect: estimated_tokens_before.zip(*estimated_tokens_after).map(
                    |(before, after)| CompactionEffect {
                        estimated_tokens_before: before,
                        estimated_tokens_after: after,
                    },
                ),
            },
            _ => continue,
        };
        let repeats_previous_model = matches!(
            (timeline.iter().rev().find(|previous| matches!(previous, StatusTimelineEntry::Model { .. })), &entry),
            (
                Some(StatusTimelineEntry::Model { provider_connection: previous_connection, model: previous_model, parameters: previous_parameters, .. }),
                StatusTimelineEntry::Model { provider_connection, model, parameters, .. },
            ) if previous_connection == provider_connection
                && previous_model == model
                && previous_parameters == parameters
        );
        if !repeats_previous_model {
            timeline.push(entry);
        }
    }
    timeline
}

/// The timeline of a loop without a persisted session: the active model, then
/// the in-memory compactions.
pub(crate) fn timeline_from_memory(
    started_at: &str,
    provider_connection: &str,
    model: &str,
    parameters: ModelParameters,
    compactions: &[CompactionRecord],
    compaction_recorded_at: &[String],
) -> Vec<StatusTimelineEntry> {
    std::iter::once(StatusTimelineEntry::Model {
        at: started_at.to_string(),
        provider_connection: provider_connection.to_string(),
        model: model.to_string(),
        parameters,
    })
    .chain(compactions.iter().enumerate().map(|(index, record)| {
        StatusTimelineEntry::Compaction {
            at: compaction_recorded_at
                .get(index)
                .cloned()
                .unwrap_or_default(),
            effect: record.effect,
        }
    }))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_event(model: &str, context_length: usize) -> StoreEvent {
        StoreEvent::ModelChanged {
            version: 1,
            changed_at: "2026-01-01T10:00:00.000Z".to_string(),
            provider_connection: "mock".to_string(),
            model: model.to_string(),
            parameters: ModelParameters {
                context_length: Some(context_length),
                ..Default::default()
            },
        }
    }

    #[test]
    fn in_memory_timeline_carries_the_recorded_times() {
        let record = CompactionRecord {
            boundary: 2,
            transcript_boundary: Some(0),
            summary: String::new(),
            effect: None,
        };
        let timeline = timeline_from_memory(
            "2026-01-01T09:00:00.000Z",
            "mock",
            "a",
            ModelParameters::default(),
            &[record],
            &["2026-01-01T09:30:00.000Z".to_string()],
        );
        assert!(matches!(&timeline[0],
            StatusTimelineEntry::Model { at, .. } if at == "2026-01-01T09:00:00.000Z"));
        assert!(matches!(&timeline[1],
            StatusTimelineEntry::Compaction { at, .. } if at == "2026-01-01T09:30:00.000Z"));
    }

    #[test]
    fn repeated_identical_models_collapse_and_compactions_interleave() {
        let events = [
            model_event("a", 1000),
            model_event("a", 1000),
            StoreEvent::Compaction {
                version: 1,
                compacted_at: "2026-01-01T10:30:00.000Z".to_string(),
                boundary: 2,
                transcript_boundary: Some(2),
                summary: String::new(),
                estimated_tokens_before: Some(800),
                estimated_tokens_after: Some(200),
            },
            model_event("a", 500),
            model_event("b", 500),
        ];
        let timeline = timeline_from_events(&events);
        assert_eq!(timeline.len(), 4);
        assert!(matches!(
            &timeline[1],
            StatusTimelineEntry::Compaction { effect: Some(effect), .. }
                if effect.estimated_tokens_before == 800
        ));
        assert!(matches!(&timeline[3], StatusTimelineEntry::Model { model, .. } if model == "b"));
    }
}
