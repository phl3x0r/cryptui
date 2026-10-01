//! The account panel.
//!
//! The margin state of the account being watched: the figures the venue shows
//! beside its trading view, minus the actions, because this is a read-only
//! monitor. It is a wide-screen luxury — the layout drops it rather than squeeze
//! the positions table — so on a narrow terminal the footer's summary line is
//! what carries these numbers.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::state::App;
use crate::venue::{AccountSnapshot, Position};

use super::{format, theme};

/// Width of the value column, in cells.
const VALUE_WIDTH: u16 = 14;
/// The most balances listed before the rest are counted instead.
const MAX_BALANCES: usize = 6;

/// Draw the panel.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title(" Account ")
        .border_style(theme::border_style(true));

    let Some(account) = &app.account else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "loading account …",
                Style::default().fg(theme::LABEL),
            )))
            .block(block)
            .centered(),
            area,
        );
        return;
    };

    // The value column sits hard against the border, so the label has the rest of
    // the width: `Position value` is the longest label and must not be truncated.
    let table = Table::new(
        rows(account, &app.positions),
        [Constraint::Min(16), Constraint::Length(VALUE_WIDTH)],
    )
    .column_spacing(0)
    .block(block);

    frame.render_widget(table, area);
}

/// The panel's contents, top to bottom.
fn rows(account: &AccountSnapshot, positions: &[Position]) -> Vec<Row<'static>> {
    let value = position_value(positions);

    let mut rows = vec![
        metric(
            "Margin ratio",
            format::percent_plain(account.margin_ratio()),
            None,
        ),
        metric(
            "Maint. margin",
            format::money(account.maintenance_margin),
            None,
        ),
        metric("Equity", format::money(account.equity), None),
        metric(
            "Unrealized",
            format::signed_money(account.unrealized_pnl),
            Some(theme::pnl_style(account.unrealized_pnl)),
        ),
        metric("Position value", format::money(value), None),
        metric("Leverage", leverage(value, account.equity), None),
        metric("Mode", mode(account.multi_assets), None),
        heading("Balances"),
    ];

    for balance in account.balances.iter().take(MAX_BALANCES) {
        rows.push(metric(
            &balance.asset,
            format::quantity(balance.total),
            None,
        ));
    }
    if account.balances.len() > MAX_BALANCES {
        rows.push(heading(&format!(
            "and {} more",
            account.balances.len() - MAX_BALANCES
        )));
    }

    rows
}

/// Total value of the open positions at their mark prices.
///
/// This is the venue's "position value": what the account is exposed to, which
/// is what its actual leverage is measured against.
fn position_value(positions: &[Position]) -> f64 {
    positions
        .iter()
        .map(|position| position.notional.abs())
        .sum::<f64>()
}

/// Position value against equity, as the venue reports actual leverage.
fn leverage(value: f64, equity: f64) -> String {
    if equity <= 0.0 {
        return "—".to_owned();
    }
    format!("{:.2}x", value / equity)
}

/// The venue's account mode, or a dash when it did not say.
fn mode(multi_assets: Option<bool>) -> String {
    match multi_assets {
        Some(true) => "Multi-Assets".to_owned(),
        Some(false) => "Single-Asset".to_owned(),
        None => "—".to_owned(),
    }
}

/// One `label  value` row, with the value right-aligned.
fn metric(label: &str, value: String, style: Option<Style>) -> Row<'static> {
    Row::new(vec![
        Cell::from(Span::styled(
            label.to_owned(),
            Style::default().fg(theme::LABEL),
        )),
        Cell::from(
            Line::from(Span::styled(value, style.unwrap_or_default())).alignment(Alignment::Right),
        ),
    ])
}

/// A row that only labels the rows below it.
fn heading(text: &str) -> Row<'static> {
    Row::new(vec![Cell::from(Span::styled(
        text.to_owned(),
        Style::default()
            .fg(theme::LABEL)
            .add_modifier(Modifier::BOLD),
    ))])
}

#[cfg(test)]
mod tests {
    use super::super::tests::{frame_lines, sample_app};
    use super::{leverage, mode};
    use crate::state::Update;
    use crate::venue::AccountSnapshot;

    /// The panel as text, on a screen wide enough to hold it.
    fn panel(app: &crate::state::App) -> String {
        frame_lines(app, 160, 44).join("\n")
    }

    #[test]
    fn the_panel_reports_the_margin_state_of_the_account() {
        let text = panel(&sample_app());

        for expected in [
            "Account",
            "Margin ratio",
            "Maint. margin",
            "Equity",
            "Unrealized",
            "Position value",
            "Leverage",
            "Mode",
            "Balances",
        ] {
            assert!(text.contains(expected), "`{expected}` missing from: {text}");
        }
    }

    #[test]
    fn the_panel_shows_the_margin_ratio_the_footer_shows() {
        let app = sample_app();
        let account = app.account.as_ref().expect("a sample account");

        // Same number in both places, from one calculation: 97.49 / 4,038.14.
        assert_eq!(super::format::percent_plain(account.margin_ratio()), "2.4%");
        assert!(panel(&app).contains("2.4%"), "the footer's ratio");
    }

    #[test]
    fn the_panel_lists_the_balances() {
        let text = panel(&sample_app());

        assert!(text.contains("USDT"), "{text}");
        assert!(text.contains("4,041.70"), "the wallet balance: {text}");
    }

    #[test]
    fn the_panel_counts_balances_it_cannot_list() {
        let mut app = sample_app();
        let mut account = app.account.clone().expect("a sample account");
        account.balances = (0..9)
            .map(|index| crate::venue::Balance {
                asset: format!("ASSET{index}"),
                total: 1.0,
                available: 1.0,
            })
            .collect();
        app.apply(Update::Account(Box::new(account)));

        let text = panel(&app);
        assert!(text.contains("and 3 more"), "{text}");
        assert!(!text.contains("ASSET8"), "the rest are not listed: {text}");
    }

    #[test]
    fn the_mode_is_named_or_admitted_unknown() {
        assert_eq!(mode(Some(true)), "Multi-Assets");
        assert_eq!(mode(Some(false)), "Single-Asset");
        assert_eq!(mode(None), "—", "not told is not the same as single");
    }

    #[test]
    fn leverage_is_position_value_over_equity() {
        assert_eq!(leverage(5_000.0, 4_000.0), "1.25x");
        assert_eq!(leverage(0.0, 4_000.0), "0.00x");
        assert_eq!(leverage(5_000.0, 0.0), "—", "no equity, no ratio");
    }

    #[test]
    fn a_missing_account_says_so_rather_than_showing_zeroes() {
        let mut app = sample_app();
        app.account = None;

        let text = panel(&app);
        assert!(text.contains("loading account"), "{text}");
        assert!(
            !text.contains("Margin ratio"),
            "no invented figures: {text}"
        );
    }

    #[test]
    fn the_mode_reaches_the_panel() {
        let mut app = sample_app();
        let mut account: AccountSnapshot = app.account.clone().expect("a sample account");
        account.multi_assets = Some(true);
        app.apply(Update::Account(Box::new(account)));

        assert!(panel(&app).contains("Multi-Assets"));
    }
}
