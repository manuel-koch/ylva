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

//! Cost accumulation across the providers used for conversation, title and
//! compaction requests of one run.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use ylva::agent::StreamEvent;
use ylva::{CostLedger, MockProvider, Provider, RequestCost};

async fn run_request(provider: &dyn Provider) {
    let mut events = provider
        .chat(Vec::new(), Vec::new(), CancellationToken::new())
        .await
        .expect("chat");
    while !matches!(events.recv().await, Some(StreamEvent::Done) | None) {}
}

#[tokio::test]
async fn costs_of_all_request_kinds_accumulate_in_the_shared_ledger() {
    let ledger = CostLedger::new();
    let provider_with_cost = |amount: f64| -> Arc<dyn Provider> {
        Arc::new(
            MockProvider::new(vec!["ok".into()], Duration::ZERO)
                .with_reported_cost(RequestCost::new(amount, "USD"))
                .with_cost_ledger(ledger.clone()),
        )
    };
    let conversation = provider_with_cost(0.10);
    let title = provider_with_cost(0.02);
    let compaction = provider_with_cost(0.30);

    run_request(conversation.as_ref()).await;
    run_request(title.as_ref()).await;
    run_request(compaction.as_ref()).await;
    run_request(conversation.as_ref()).await;

    assert_eq!(ledger.totals_text().as_deref(), Some("0.52 USD"));
}

#[tokio::test]
async fn requests_without_a_reported_cost_leave_the_total_empty() {
    let ledger = CostLedger::new();
    let provider =
        MockProvider::new(vec!["ok".into()], Duration::ZERO).with_cost_ledger(ledger.clone());

    run_request(&provider).await;

    assert!(ledger.totals().is_empty());
    assert_eq!(ledger.totals_text(), None);
}
