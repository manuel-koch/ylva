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
fn plural_noun_uses_singular_only_for_one() {
    assert_eq!(plural_noun(1, "message", "messages"), "message");
    assert_eq!(plural_noun(0, "message", "messages"), "messages");
    assert_eq!(plural_noun(2, "message", "messages"), "messages");
}

#[test]
fn footer_uses_three_rows_only_below_the_narrow_width_threshold() {
    assert_eq!(footer_height(FOOTER_NARROW_WIDTH_THRESHOLD - 1), 3);
    assert_eq!(footer_height(FOOTER_NARROW_WIDTH_THRESHOLD), 2);
    assert_eq!(footer_height(FOOTER_NARROW_WIDTH_THRESHOLD + 40), 2);
}

#[test]
fn split_footer_rows_matches_footer_height() {
    let narrow = Rect::new(0, 0, FOOTER_NARROW_WIDTH_THRESHOLD - 1, 3);
    assert!(matches!(
        split_footer_rows(narrow),
        FooterRows::Narrow { .. }
    ));
    let wide = Rect::new(0, 0, FOOTER_NARROW_WIDTH_THRESHOLD, 2);
    assert!(matches!(split_footer_rows(wide), FooterRows::Wide { .. }));
}

#[test]
fn elide_balanced_tokens_keeps_text_that_fits() {
    assert_eq!(elide_balanced_tokens("openrouter", 10), "openrouter");
    assert_eq!(elide_balanced_tokens("openrouter", 0), "");
}

#[test]
fn elide_balanced_tokens_never_elides_separator_characters() {
    let label = "openrouter → anthropic/claude-fable-5.1:batch";
    let elided = elide_balanced_tokens(label, 30);
    assert_eq!(elided.chars().count(), 30);
    assert!(elided.contains(" → "), "{elided:?}");
    assert!(elided.contains('/') && elided.contains(':'), "{elided:?}");
    assert!(!elided.contains("..."));
}

#[test]
fn elide_balanced_tokens_balances_the_budget_across_tokens() {
    let elided = elide_balanced_tokens("openrouter → anthropic/claude-fable-5.1:batch", 30);
    let widths: Vec<usize> = elided
        .split(|character| LABEL_SEPARATOR_CHARS.contains(&character))
        .filter(|token| !token.is_empty())
        .map(|token| token.chars().count())
        .collect();
    let widest = widths.iter().max().unwrap();
    let narrowest = widths.iter().min().unwrap();
    // Tokens that were already narrower than the shrink level stay intact, so
    // the spread is bounded by that intact token rather than by zero.
    assert!(widest - narrowest <= 2, "{elided:?} widths {widths:?}");
    assert!(elided.matches('…').count() >= 2, "{elided:?}");
}

#[test]
fn elide_balanced_tokens_with_budget_of_one_is_just_the_ellipsis() {
    assert_eq!(elide_balanced_tokens("openrouter", 1), "…");
}

#[test]
fn elide_balanced_tokens_falls_back_to_truncation_when_separators_do_not_fit() {
    assert_eq!(elide_balanced_tokens("a → b → c", 3), "a …");
}

fn render_row(width: u16, draw: impl FnOnce(&mut Frame, Rect)) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| draw(frame, Rect::new(0, 0, width, 1)))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    (0..width)
        .map(|column| buffer.cell((column, 0)).unwrap().symbol())
        .collect()
}

#[test]
fn context_usage_row_puts_cost_left_of_usage_and_drops_it_first() {
    let render = |width| {
        render_row(width, |frame, area| {
            draw_footer_context_usage(frame, area, Some("0.12 USD"), Some("64.2K (45%)"), 9);
        })
    };
    assert_eq!(render(60).trim(), "0.12 USD · 64.2K (45%)");
    // 9 controls columns + 12 usage columns fit; the cost does not.
    assert_eq!(render(22).trim(), "64.2K (45%)");
    assert_eq!(render(15).trim(), "");
}

#[test]
fn context_usage_row_shows_cost_without_context_usage() {
    let line = render_row(40, |frame, area| {
        draw_footer_context_usage(frame, area, Some("<0.01 USD"), None, 9);
    });
    assert_eq!(line.trim(), "<0.01 USD");
    let hidden = render_row(40, |frame, area| {
        draw_footer_context_usage(frame, area, None, None, 9);
    });
    assert_eq!(hidden.trim(), "");
}

#[test]
fn narrow_meta_row_shows_cost_before_usage_and_drops_cost_when_too_narrow() {
    let state = TuiState {
        cost_total_text: Some("0.12 USD".to_string()),
        context_usage_text: Some("64.2K (45%)".to_string()),
        ..TuiState::new()
    };
    let wide = render_row(40, |frame, area| draw_footer_meta(frame, area, &state));
    assert!(wide.contains("0.12 USD · 64.2K (45%)"), "{wide:?}");
    let tight = render_row(14, |frame, area| draw_footer_meta(frame, area, &state));
    assert!(
        !tight.contains("USD") && tight.contains("64.2K (45%)"),
        "{tight:?}"
    );
}
