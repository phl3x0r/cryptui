//! The chart panel.
//!
//! Phase P2 reserves the pane and shows the focused contract's numbers; the
//! candles, moving averages, volume subpane and pan/zoom arrive in phase P3.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::state::App;

use super::{format, theme};

/// Draw the chart panel.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let symbol = app.chart_symbol().unwrap_or("no symbol");
    let title = Line::from(vec![
        Span::styled(
            " Chart ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {symbol} · {} ", app.interval),
            Style::default().fg(theme::LABEL),
        ),
    ]);
    let block = Block::bordered()
        .title(title)
        .border_style(theme::border_style(true));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Until the renderer exists, the pane at least reports the focused contract.
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    frame.render_widget(Paragraph::new(detail_lines(app)), rows[0]);
    if rows[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "candles, moving averages and volume arrive in phase P3",
                Style::default().fg(theme::LABEL),
            )))
            .alignment(Alignment::Center),
            rows[1],
        );
    }
}

/// Numbers for the focused contract.
fn detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(position) = app.selected_position() else {
        return vec![Line::from(Span::styled(
            "no position selected",
            Style::default().fg(theme::LABEL),
        ))];
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("mark ", Style::default().fg(theme::LABEL)),
            Span::raw(format::price(position.mark_price)),
            Span::styled("   entry ", Style::default().fg(theme::LABEL)),
            Span::raw(format::price(position.entry_price)),
            Span::styled("   size ", Style::default().fg(theme::LABEL)),
            Span::raw(format::quantity(position.size)),
            Span::styled("   ", Style::default()),
            Span::styled(position.side.to_string(), theme::side_style(position.side)),
        ]),
        Line::from(vec![
            Span::styled("margin ", Style::default().fg(theme::LABEL)),
            Span::raw(format::money(position.initial_margin)),
            Span::styled("   notional ", Style::default().fg(theme::LABEL)),
            // The venue reports a negative notional for shorts; the side and
            // size already carry the direction.
            Span::raw(format::money(position.notional.abs())),
            Span::styled("   maintenance ", Style::default().fg(theme::LABEL)),
            Span::raw(format::money(position.maintenance_margin)),
        ]),
        Line::from(vec![
            Span::styled("pnl ", Style::default().fg(theme::LABEL)),
            Span::styled(
                format::signed_money(position.unrealized_pnl),
                theme::pnl_style(position.unrealized_pnl),
            ),
            Span::styled("   liquidation ", Style::default().fg(theme::LABEL)),
            match position.liquidation_price {
                Some(price) => Span::raw(format::price(price)),
                None => Span::styled("—", Style::default().fg(theme::LABEL)),
            },
        ]),
    ];

    lines
}

#[cfg(test)]
mod tests {
    use crate::state::{App, Update};
    use crate::venue::{Interval, VenueId};

    use super::super::tests::{frame_lines, sample_account, sample_app, sample_position};

    #[test]
    fn shows_the_selected_contract_numbers() {
        let lines = frame_lines(&sample_app(), 120, 40);
        let text = lines.join("\n");

        assert!(text.contains("Chart"), "panel title: {text}");
        assert!(text.contains("mark 85,500.00"), "mark price: {text}");
        assert!(text.contains("liquidation"), "liquidation price: {text}");
        assert!(
            text.contains("arrive in phase P3"),
            "the missing renderer is stated, not faked: {text}"
        );
    }

    #[test]
    fn without_a_selection_the_panel_says_so() {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.apply(Update::Account(Box::new(sample_account())));

        let text = frame_lines(&app, 120, 40).join("\n");
        assert!(text.contains("no position selected"), "got: {text}");
        assert!(!text.contains("liquidation"), "no numbers to show");
    }

    #[test]
    fn a_liquidation_price_is_omitted_when_the_venue_reports_none() {
        let mut app = sample_app();
        let mut position = sample_position("SOLUSDT", crate::venue::PositionSide::Long, 5.0);
        position.liquidation_price = None;
        app.set_positions(vec![position]);

        let text = frame_lines(&app, 120, 40).join("\n");
        assert!(text.contains("liquidation"), "label stays: {text}");
        assert!(
            text.contains("—"),
            "an em dash stands in for a missing price"
        );
    }
}
