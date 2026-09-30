//! The two-line header: identity and feed health, then the focused market.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::auth::now_ms;
use crate::state::{App, FeedStatus};

use super::{format, theme};

/// Width reserved on the right of the first row for feed health.
///
/// Wide enough for both feeds plus a short error message: truncating the reason
/// a feed is degraded would defeat the point of showing it.
const FEED_WIDTH: u16 = 56;

/// Draw the header.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let now = now_ms();

    let columns =
        Layout::horizontal([Constraint::Min(24), Constraint::Length(FEED_WIDTH)]).split(rows[0]);
    frame.render_widget(Paragraph::new(identity_line(app)), columns[0]);
    frame.render_widget(
        Paragraph::new(feed_line(app, now)).alignment(Alignment::Right),
        columns[1],
    );

    frame.render_widget(Paragraph::new(market_line(app)), rows[1]);
}

/// `cryptui 0.1.0 · main · binance_futures`
fn identity_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("cryptui {}", crate::VERSION),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(theme::LABEL)),
        Span::styled(
            app.account_label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(theme::LABEL)),
        Span::styled(app.venue.to_string(), Style::default().fg(theme::LABEL)),
    ])
}

/// `positions ● live 2s   account ● live 2s`
fn feed_line(app: &App, now: i64) -> Line<'static> {
    let feeds = [
        ("positions", &app.positions_feed),
        ("account", &app.account_feed),
    ];

    let mut spans = Vec::new();
    for (index, (name, status)) in feeds.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            format!("{name} "),
            Style::default().fg(theme::LABEL),
        ));
        spans.push(Span::styled(
            "● ",
            Style::default().fg(feed_color(status, app, now)),
        ));
        spans.push(Span::styled(
            status.summary(now, app.stale_after_ms),
            Style::default().fg(theme::LABEL),
        ));
    }
    Line::from(spans)
}

/// Colour for a feed: red on error, yellow when stale, green when fresh.
fn feed_color(status: &FeedStatus, app: &App, now: i64) -> ratatui::style::Color {
    if status.last_error.is_some() {
        theme::ERROR
    } else if status.is_stale(now, app.stale_after_ms) {
        theme::WARNING
    } else {
        theme::POSITIVE
    }
}

/// `BTCUSDT  15m  mark 85,500.00  entry 85,000.00  pnl +141.25  2 open positions`
fn market_line(app: &App) -> Line<'static> {
    let mut spans = Vec::new();

    match app.chart_symbol() {
        Some(symbol) => spans.push(Span::styled(
            format!("{symbol}  "),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        None => spans.push(Span::styled(
            "no symbol  ",
            Style::default().fg(theme::LABEL),
        )),
    }
    spans.push(Span::styled(
        format!("{}  ", app.interval),
        Style::default().fg(theme::LABEL),
    ));

    if let Some(position) = app.selected_position() {
        spans.push(Span::styled("mark ", Style::default().fg(theme::LABEL)));
        spans.push(Span::raw(format::price(position.mark_price)));
        spans.push(Span::styled("  entry ", Style::default().fg(theme::LABEL)));
        spans.push(Span::raw(format::price(position.entry_price)));
        spans.push(Span::styled("  pnl ", Style::default().fg(theme::LABEL)));
        spans.push(Span::styled(
            format::signed_money(position.unrealized_pnl),
            theme::pnl_style(position.unrealized_pnl),
        ));
        spans.push(Span::raw("  "));
    }

    spans.push(Span::styled(
        format!("{} open positions", app.positions.len()),
        Style::default().fg(theme::LABEL),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::state::{App, Feed, Update};
    use crate::venue::{Interval, VenueId};

    use super::super::tests::{frame_lines, sample_app};

    #[test]
    fn identity_and_market_lines_carry_the_essentials() {
        let lines = frame_lines(&sample_app(), 120, 40);

        assert!(lines[0].contains("cryptui 0.1.0"));
        assert!(lines[0].contains("main"));
        assert!(lines[0].contains("binance_futures"));
        assert!(lines[1].contains("BTCUSDT"), "chart symbol: {}", lines[1]);
        assert!(lines[1].contains("mark 85,500.00"), "mark: {}", lines[1]);
        assert!(lines[1].contains("+141.25"), "selected pnl: {}", lines[1]);
    }

    #[test]
    fn a_feed_error_replaces_the_age_with_the_reason() {
        let mut app = sample_app();
        app.apply(Update::Failed {
            feed: Feed::Account,
            message: "timed out".to_owned(),
        });

        let lines = frame_lines(&app, 140, 40);
        assert!(
            lines[0].contains("error: timed out"),
            "header shows the failure: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("positions"),
            "the healthy feed is untouched"
        );
    }

    #[test]
    fn before_any_data_the_header_says_loading() {
        let app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        let lines = frame_lines(&app, 120, 40);

        assert!(lines[0].contains("loading"), "got: {}", lines[0]);
        assert!(lines[1].contains("no symbol"), "got: {}", lines[1]);
        assert!(lines[1].contains("0 open positions"), "got: {}", lines[1]);
    }

    #[test]
    fn the_header_renders_in_a_narrow_terminal() {
        let app = sample_app();
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| super::render(frame, frame.area(), &app))
            .expect("draw succeeds");
    }
}
