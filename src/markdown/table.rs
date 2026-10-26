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

//! Column-aligned table rendering.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::text::{terminal_display_width, wrap_table_words, TableWord, Word};
use crate::theme::markdown_dim_fg;

/// Distributes `total` units across `weights.len()` buckets proportionally
/// to `weights`, using the largest-remainder method so the allocation sums
/// exactly to `min(total, sum(caps))`. Each bucket is capped at the
/// corresponding entry in `caps`. Used to shrink table columns so that
/// narrow columns keep a fair share of the available width instead of being
/// crowded out by a single very wide column (see docs/backlog-issues.md).
pub(super) fn distribute_proportional(
    weights: &[usize],
    total: usize,
    caps: &[usize],
) -> Vec<usize> {
    let count = weights.len();
    if count == 0 {
        return Vec::new();
    }
    let cap_sum: u128 = caps.iter().map(|&cap| cap as u128).sum();
    let target = if cap_sum < total as u128 {
        cap_sum as usize
    } else {
        total
    };
    let weight_sum: usize = weights.iter().sum();
    let mut allocation = vec![0usize; count];
    let mut remainders = vec![0u128; count];
    if weight_sum > 0 {
        for index in 0..count {
            let numerator = weights[index] as u128 * target as u128;
            let floor_value = (numerator / weight_sum as u128) as usize;
            allocation[index] = floor_value.min(caps[index]);
            remainders[index] = numerator % weight_sum as u128;
        }
    }
    // Hand out the remainder (from flooring, or the whole target when every
    // weight is zero) to the buckets with the largest fractional remainder
    // first, cycling until the target is met or every cap is exhausted.
    let mut leftover = target.saturating_sub(allocation.iter().sum());
    if leftover > 0 {
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|&a, &b| remainders[b].cmp(&remainders[a]));
        loop {
            let mut progressed = false;
            for &index in &order {
                if leftover == 0 {
                    break;
                }
                if allocation[index] < caps[index] {
                    allocation[index] += 1;
                    leftover -= 1;
                    progressed = true;
                }
            }
            if leftover == 0 || !progressed {
                break;
            }
        }
    }
    allocation
}

/// Renders all accumulated table rows as column-aligned lines.
/// Two-pass: compute natural per-column max widths, then shrink them to fit
/// `width` (minimum 1 char per column). Long cells are word-wrapped to the
/// capped column width, and unbreakable words are split when necessary,
/// producing multi-terminal-line rows so that no content is ever clipped or
/// rendered beyond `width`.
/// Header rows are rendered bold and followed by a separator line.
pub(super) fn flush_table(
    out: &mut Vec<Line<'static>>,
    rows: &[(bool, Vec<Vec<Word>>)],
    width: usize,
) {
    if rows.is_empty() {
        return;
    }
    let col_count = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }
    // Pass 1: natural column widths, and the minimum width imposed by each
    // column's longest unbreakable word when whitespace wrapping is enough.
    let mut col_widths = vec![0usize; col_count];
    let mut col_min_widths = vec![1usize; col_count];
    for (_, cells) in rows {
        for (col_idx, cell) in cells.iter().enumerate() {
            col_widths[col_idx] = col_widths[col_idx].max(cell_display_width(cell));
            for word in cell {
                col_min_widths[col_idx] =
                    col_min_widths[col_idx].max(terminal_display_width(&word.text));
            }
        }
    }
    // Shrink columns to the available width. When the minimum widths of
    // unbreakable words do not fit, table cells are character-wrapped below,
    // so preserving those minimums would make the table overflow its bubble.
    let separator_overhead = col_count.saturating_sub(1) * 3;
    let total_natural: usize = col_widths.iter().sum::<usize>() + separator_overhead;
    if total_natural > width {
        let budget = width.saturating_sub(separator_overhead).max(col_count);
        let minimum_sum: usize = col_min_widths.iter().sum();
        let natural_widths = col_widths.clone();
        if minimum_sum >= budget {
            // Not even the per-word minimums fit: distribute the budget
            // proportionally to natural width, floor 1 per column.
            col_widths = distribute_proportional(&natural_widths, budget, &natural_widths)
                .into_iter()
                .map(|w| w.max(1))
                .collect();
        } else {
            // Every column keeps its minimum width; the remaining budget is
            // then handed out proportionally to how much more each column's
            // natural width wants, so narrow columns aren't crowded out by
            // a single very wide column (see docs/backlog-issues.md).
            let extra_budget = budget - minimum_sum;
            let wants: Vec<usize> = natural_widths
                .iter()
                .zip(&col_min_widths)
                .map(|(natural, min_width)| natural.saturating_sub(*min_width))
                .collect();
            let extra = distribute_proportional(&wants, extra_budget, &wants);
            col_widths = col_min_widths
                .iter()
                .zip(&extra)
                .map(|(min_width, extra)| min_width + extra)
                .collect();
        }
    }
    let separator_style = Style::default().fg(markdown_dim_fg());
    let empty_table_words: Vec<TableWord> = Vec::new();
    for (is_header, cells) in rows {
        let extra = if *is_header {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        // Pass 2: wrap each cell to its (possibly capped) column width.
        let wrapped_cells: Vec<Vec<Vec<TableWord>>> = (0..col_count)
            .map(|col_idx| {
                let cell = cells.get(col_idx).map(Vec::as_slice).unwrap_or(&[]);
                wrap_table_words(cell, col_widths[col_idx].max(1))
            })
            .collect();
        let row_height = wrapped_cells
            .iter()
            .map(|c| c.len().max(1))
            .max()
            .unwrap_or(1);
        // Emit one terminal line per wrapped row-line.
        for line_idx in 0..row_height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for col_idx in 0..col_count {
                if col_idx > 0 {
                    spans.push(Span::styled(" │ ", separator_style));
                }
                let cell_lines = wrapped_cells[col_idx].as_slice();
                let line_words = cell_lines.get(line_idx).unwrap_or(&empty_table_words);
                let cell_len = line_words
                    .iter()
                    .map(|word| usize::from(word.space_before) + terminal_display_width(&word.text))
                    .sum::<usize>();
                for word in line_words {
                    if word.space_before {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::styled(
                        word.text.clone(),
                        word.style.to_style().patch(extra),
                    ));
                }
                let pad = col_widths[col_idx].saturating_sub(cell_len);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            out.push(Line::from(spans));
        }
        if *is_header {
            let table_width = col_widths.iter().sum::<usize>() + separator_overhead;
            out.push(Line::from(Span::styled(
                "─".repeat(table_width),
                separator_style,
            )));
        }
    }
}

/// Returns the display width of a table cell (sum of word lengths plus
/// one space between consecutive words).
pub(super) fn cell_display_width(words: &[Word]) -> usize {
    words
        .iter()
        .enumerate()
        .map(|(i, word)| (if i > 0 { 1 } else { 0 }) + terminal_display_width(&word.text))
        .sum()
}
