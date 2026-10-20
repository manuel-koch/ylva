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

//! Run-scoped accounting of provider-reported model request costs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Cost a provider reported for one completed model request.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestCost {
    pub amount: f64,
    pub currency: String,
}

impl RequestCost {
    pub fn new(amount: f64, currency: impl Into<String>) -> Self {
        Self {
            amount,
            currency: currency.into(),
        }
    }
}

/// Shareable accumulator of [`RequestCost`] values, totalled per currency.
///
/// Clones share the same underlying totals. The ledger is in-memory only and
/// is never persisted.
#[derive(Debug, Clone, Default)]
pub struct CostLedger {
    totals: Arc<Mutex<BTreeMap<String, f64>>>,
}

impl CostLedger {
    /// Create an independent, empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// The ledger shared by every provider of this process unless a provider
    /// is given its own ledger.
    pub fn process() -> Self {
        static PROCESS_LEDGER: OnceLock<CostLedger> = OnceLock::new();
        PROCESS_LEDGER.get_or_init(CostLedger::new).clone()
    }

    /// Add one request cost. Non-finite amounts are ignored.
    pub fn record(&self, cost: RequestCost) {
        if !cost.amount.is_finite() {
            return;
        }
        let mut totals = self.totals.lock().unwrap_or_else(PoisonError::into_inner);
        *totals.entry(cost.currency).or_insert(0.0) += cost.amount;
    }

    /// One total per currency, ordered by currency code. Empty until a cost
    /// has been recorded.
    pub fn totals(&self) -> Vec<RequestCost> {
        let totals = self.totals.lock().unwrap_or_else(PoisonError::into_inner);
        totals
            .iter()
            .map(|(currency, amount)| RequestCost::new(*amount, currency.clone()))
            .collect()
    }

    /// Display text for the current totals, or `None` when nothing was
    /// recorded.
    pub fn totals_text(&self) -> Option<String> {
        format_cost_totals(&self.totals())
    }
}

/// Format totals as `<amount> <CUR>` with two decimals, joined with ` + `.
///
/// A non-zero amount below 0.01 shows as `<0.01 <CUR>`. Returns `None` for
/// an empty slice.
pub fn format_cost_totals(totals: &[RequestCost]) -> Option<String> {
    if totals.is_empty() {
        return None;
    }
    let text = totals
        .iter()
        .map(|total| {
            if total.amount > 0.0 && total.amount < 0.005 {
                format!("<0.01 {}", total.currency)
            } else {
                format!("{:.2} {}", total.amount, total.currency)
            }
        })
        .collect::<Vec<_>>()
        .join(" + ");
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ledger_has_no_totals() {
        let ledger = CostLedger::new();
        assert!(ledger.totals().is_empty());
        assert_eq!(ledger.totals_text(), None);
    }

    #[test]
    fn record_accumulates_per_currency_in_sorted_order() {
        let ledger = CostLedger::new();
        ledger.record(RequestCost::new(0.25, "USD"));
        ledger.record(RequestCost::new(0.10, "EUR"));
        ledger.record(RequestCost::new(0.50, "USD"));
        let totals = ledger.totals();
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].currency, "EUR");
        assert!((totals[0].amount - 0.10).abs() < 1e-9);
        assert_eq!(totals[1].currency, "USD");
        assert!((totals[1].amount - 0.75).abs() < 1e-9);
        assert_eq!(ledger.totals_text().unwrap(), "0.10 EUR + 0.75 USD");
    }

    #[test]
    fn zero_cost_is_recorded_and_non_finite_is_ignored() {
        let ledger = CostLedger::new();
        ledger.record(RequestCost::new(f64::NAN, "USD"));
        assert_eq!(ledger.totals_text(), None);
        ledger.record(RequestCost::new(0.0, "USD"));
        assert_eq!(ledger.totals_text().unwrap(), "0.00 USD");
    }

    #[test]
    fn tiny_non_zero_total_is_shown_as_below_one_cent() {
        let ledger = CostLedger::new();
        ledger.record(RequestCost::new(0.0004, "USD"));
        assert_eq!(ledger.totals_text().unwrap(), "<0.01 USD");
        ledger.record(RequestCost::new(0.0096, "USD"));
        assert_eq!(ledger.totals_text().unwrap(), "0.01 USD");
    }

    #[test]
    fn clones_share_totals_and_concurrent_recording_is_exact() {
        let ledger = CostLedger::new();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let shared = ledger.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        shared.record(RequestCost::new(0.01, "USD"));
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!((ledger.totals()[0].amount - 8.0).abs() < 1e-6);
    }

    #[test]
    fn process_ledger_is_shared() {
        assert!(Arc::ptr_eq(
            &CostLedger::process().totals,
            &CostLedger::process().totals
        ));
    }
}
