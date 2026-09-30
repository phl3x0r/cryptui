//! The two-line footer: account totals, then the key map.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::App;

use super::{format, theme};

/// Key hints, kept in one place so they cannot drift from the key handler.
const HINTS: &str = "q quit · j/k move · g/G ends · 1-7 sort column · , . cycle · R reverse · h/l pan · + - zoom · f follow · [ ] interval · r refresh";

/// Draw the footer.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    frame.render_widget(Paragraph::new(totals_line(app)), rows[0]);
    if rows[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                HINTS,
                Style::default().fg(theme::LABEL),
            ))),
            rows[1],
        );
    }
}

/// `Wallet 4,041.70 · Equity 4,038.14 · Unrealized -3.56 · Margin ratio 2.4% · Available 3,203.16`
fn totals_line(app: &App) -> Line<'static> {
    let Some(account) = &app.account else {
        return Line::from(Span::styled(
            "loading account …",
            Style::default().fg(theme::LABEL),
        ));
    };

    let margin_ratio = if account.equity > 0.0 {
        account.maintenance_margin / account.equity * 100.0
    } else {
        0.0
    };

    let mut spans = Vec::new();
    metric(
        &mut spans,
        "Wallet",
        format::money(account.wallet_balance),
        None,
    );
    metric(&mut spans, "Equity", format::money(account.equity), None);
    metric(
        &mut spans,
        "Unrealized",
        format::signed_money(account.unrealized_pnl),
        Some(theme::pnl_style(account.unrealized_pnl)),
    );
    metric(
        &mut spans,
        "Margin ratio",
        format::percent_plain(margin_ratio),
        None,
    );
    metric(
        &mut spans,
        "Available",
        format::money(account.available_balance),
        None,
    );
    Line::from(spans)
}

/// Append one `label value` pair to a line.
fn metric(spans: &mut Vec<Span<'static>>, label: &str, value: String, style: Option<Style>) {
    if !spans.is_empty() {
        spans.push(Span::styled("  ·  ", Style::default().fg(theme::LABEL)));
    }
    spans.push(Span::styled(
        format!("{label} "),
        Style::default().fg(theme::LABEL),
    ));
    spans.push(Span::styled(value, style.unwrap_or_default()));
}

#[cfg(test)]
mod tests {
    use crate::state::App;
    use crate::venue::{Interval, VenueId};

    use super::super::tests::{frame_lines, sample_app};
    use super::HINTS;

    #[test]
    fn totals_show_every_account_metric() {
        let lines = frame_lines(&sample_app(), 140, 40);
        let totals = &lines[lines.len() - 2];

        assert!(totals.contains("Wallet 4,041.70"), "wallet: {totals}");
        assert!(totals.contains("Equity 4,038.14"), "equity: {totals}");
        assert!(totals.contains("Unrealized -3.56"), "unrealized: {totals}");
        assert!(
            totals.contains("Margin ratio 2.4%"),
            "margin ratio: {totals}"
        );
        assert!(totals.contains("Available 3,203.16"), "available: {totals}");
    }

    #[test]
    fn the_hint_line_advertises_the_real_key_map() {
        let lines = frame_lines(&sample_app(), 160, 40);
        let hints = &lines[lines.len() - 1];

        assert_eq!(hints, HINTS.trim_end());
        for expected in ["quit", "sort column", "interval", "refresh"] {
            assert!(hints.contains(expected), "missing `{expected}`: {hints}");
        }
    }

    #[test]
    fn before_account_data_the_footer_says_it_is_loading() {
        let app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        let lines = frame_lines(&app, 120, 40);
        assert!(lines[lines.len() - 2].contains("loading account"));
    }
}
