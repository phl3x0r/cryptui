//! The account performance panel.
//!
//! A value curve with the figures that describe it, over a selectable window. It
//! is an overlay rather than a pane because it is something you look at
//! occasionally, not something that has to share the screen with the chart.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::auth::now_ms;
use crate::performance::{EquitySeries, Window, describe_duration};
use crate::state::App;

use super::{format, theme};

/// Overlay width, as a percentage of the screen.
const WIDTH_PERCENT: u16 = 88;
/// Overlay height, as a percentage of the screen.
const HEIGHT_PERCENT: u16 = 72;
/// Smallest overlay worth drawing.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;
/// Width of the value axis, in cells.
const AXIS_WIDTH: u16 = 12;

/// Draw the panel, if it is open.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let performance = app.performance();
    if !performance.open {
        return;
    }

    let now = now_ms();
    let overlay = centered(area);
    // The panels underneath stay visible around it, so clear first.
    frame.render_widget(Clear, overlay);

    let series = performance.windowed(now);
    let block = Block::bordered()
        .title(title(app, performance.window))
        .border_style(theme::border_style(true));

    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Curve on top, figures underneath, coverage and keys at the bottom.
    let metrics_height = if inner.height >= 14 { 4 } else { 2 };
    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(metrics_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    render_curve(frame, rows[0], &series);
    render_metrics(frame, rows[1], app, &series);
    render_coverage(frame, rows[2], &series);
    render_keys(frame, rows[3], app, performance.window);
}

/// ` Performance · Main · 3 months `
fn title(app: &App, window: Window) -> Line<'static> {
    let fresh = app.performance().feed.summary(now_ms(), app.stale_after_ms);
    Line::from(vec![
        Span::styled(
            " Performance ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} · {} ", app.account_label(), window.description()),
            Style::default().fg(theme::LABEL),
        ),
        Span::styled(format!("● {fresh} "), Style::default().fg(theme::LABEL)),
    ])
}

/// The value curve, with a value axis on the right.
fn render_curve(frame: &mut Frame, area: Rect, series: &EquitySeries) {
    if area.height == 0 || area.width <= AXIS_WIDTH {
        return;
    }

    if series.is_empty() {
        let message = if series.is_empty() {
            "no history yet: the venue serves only recent income, and the rest accumulates while cryptui runs"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(theme::WARNING),
            )))
            .centered(),
            area,
        );
        return;
    }

    let Some((low, high)) = value_bounds(series) else {
        return;
    };
    let (low, high) = pad(low, high);

    let columns =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(AXIS_WIDTH)]).split(area);

    let points = series.points();
    let count = (points.len().saturating_sub(1)).max(1) as f64;
    let opening = points.first().map(|point| point.wallet);

    let canvas = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([low, high])
        .marker(Marker::Braille)
        .paint(|context| {
            draw_curve(context, points, opening);
        });
    frame.render_widget(canvas, columns[0]);

    frame.render_widget(
        Paragraph::new(Text::from(value_axis(
            low,
            high,
            opening,
            columns[1].height,
        )))
        .alignment(Alignment::Right),
        columns[1],
    );
}

/// Draw the curve and a reference line at the opening balance.
fn draw_curve(
    context: &mut Context,
    points: &[crate::performance::EquityPoint],
    opening: Option<f64>,
) {
    if let Some(opening) = opening {
        context.draw(&CanvasLine::new(
            0.0,
            opening,
            (points.len().saturating_sub(1)).max(1) as f64,
            opening,
            theme::LAST_PRICE,
        ));
    }

    for (index, pair) in points.windows(2).enumerate() {
        context.draw(&CanvasLine::new(
            index as f64,
            pair[0].wallet,
            index as f64 + 1.0,
            pair[1].wallet,
            theme::ACCENT,
        ));
    }

    // A single point is still worth marking.
    if points.len() == 1 {
        context.draw(&CanvasLine::new(
            0.0,
            points[0].wallet,
            1.0,
            points[0].wallet,
            theme::ACCENT,
        ));
    }
}

/// Values at the top, middle and bottom of the curve.
fn value_axis(low: f64, high: f64, opening: Option<f64>, height: u16) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut lines = vec![Line::default(); height];
    if height == 0 {
        return lines;
    }

    let plain = Style::default().fg(theme::LABEL);
    let label = |value: f64, style: Style| Line::from(Span::styled(format::money(value), style));

    lines[0] = label(high, plain);
    if height >= 3 {
        lines[height / 2] = label((low + high) / 2.0, plain);
        lines[height - 1] = label(low, plain);
    }

    if let Some(opening) = opening
        && high > low
        && (low..=high).contains(&opening)
        && height >= 4
    {
        let row = ((high - opening) / (high - low) * (height - 1) as f64).round() as usize;
        if row != 0 && row != height - 1 {
            lines[row] = label(
                opening,
                Style::default()
                    .fg(theme::LAST_PRICE)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    lines
}

/// The figures, in three rows of four.
fn render_metrics(frame: &mut Frame, area: Rect, app: &App, series: &EquitySeries) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let Some(metrics) = series.metrics() else {
        let message = if series.is_empty() {
            "no history in this window"
        } else {
            "not enough history to measure: at least two days are needed"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(theme::WARNING),
            ))),
            area,
        );
        return;
    };

    let mut lines = vec![
        Line::from(spans(vec![
            metric(
                "total return",
                signed(metrics.total_return),
                metrics.total_return,
            ),
            metric("CAGR", optional(metrics.cagr), metrics.cagr.unwrap_or(0.0)),
            metric("volatility", unsigned(metrics.volatility), 0.0),
            metric("sharpe", ratio(metrics.sharpe), 0.0),
        ])),
        Line::from(spans(vec![
            metric("sortino", ratio(metrics.sortino), 0.0),
            metric(
                "max drawdown",
                signed(-metrics.max_drawdown),
                -metrics.max_drawdown,
            ),
            metric("calmar", ratio(metrics.calmar), 0.0),
            metric("win rate", unsigned(metrics.win_rate), 0.0),
        ])),
        Line::from(spans(vec![
            metric("best day", signed(metrics.best_day), metrics.best_day),
            metric("worst day", signed(metrics.worst_day), metrics.worst_day),
            metric("days", format!("{}", metrics.window_days()), 0.0),
            metric("points", format!("{}", metrics.samples), 0.0),
        ])),
    ];

    // The current account state, so the curve has a "now" to sit against.
    if let Some(account) = &app.account {
        lines.push(Line::from(spans(vec![
            labelled("wallet", format::money(account.wallet_balance)),
            labelled("equity", format::money(account.equity)),
            labelled("unrealized", format::signed_money(account.unrealized_pnl)),
            labelled(
                "annualised?",
                if metrics.annualised_is_meaningful() {
                    "yes".to_owned()
                } else {
                    format!(
                        "no, {} is too short",
                        describe_duration(metrics.to_ms - metrics.from_ms)
                    )
                },
            ),
        ])));
    }

    lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// `2026-03-04 → 2026-09-30 · 211 days · 211 points`
fn render_coverage(frame: &mut Frame, area: Rect, series: &EquitySeries) {
    if area.height == 0 {
        return;
    }

    let text = match series.span() {
        Some((from, to)) => format!(
            "coverage {} → {} · {} · {} points",
            format::timestamp(from),
            format::timestamp(to),
            describe_duration(to - from),
            series.len()
        ),
        None => "no coverage".to_owned(),
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme::LABEL),
        ))),
        area,
    );
}

/// The window selector, with the active window marked.
fn render_keys(frame: &mut Frame, area: Rect, app: &App, window: Window) {
    if area.height == 0 {
        return;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, candidate) in Window::ALL.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme::LABEL)));
        }
        let active = *candidate == window;
        spans.push(Span::styled(
            format!("{} {}", index + 1, candidate.label()),
            if active {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(theme::LABEL)
            },
        ));
    }
    spans.push(Span::styled(
        "    [ ] cycle · e or Esc closes",
        Style::default().fg(theme::LABEL),
    ));
    if !app.performance().series.is_empty() {
        spans.push(Span::styled(
            format!(" · {} days on record", app.performance().series.len()),
            Style::default().fg(theme::LABEL),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// ` label value `, optionally coloured by sign.
fn metric(label: &str, value: String, signed_by: f64) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), Style::default().fg(theme::LABEL)),
        Span::styled(
            format!("{value:<12}"),
            if signed_by == 0.0 {
                Style::default()
            } else {
                theme::pnl_style(signed_by)
            },
        ),
    ]
}

/// A plain `label value` pair.
fn labelled(label: &str, value: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), Style::default().fg(theme::LABEL)),
        Span::styled(format!("{value:<12}"), Style::default()),
    ]
}

/// Flatten per-metric spans into one line.
fn spans(groups: Vec<Vec<Span<'static>>>) -> Vec<Span<'static>> {
    groups.into_iter().flatten().collect()
}

/// A percentage with its sign, for returns.
fn signed(value: f64) -> String {
    format::percent(value * 100.0)
}

/// A percentage that is not a gain or a loss.
fn unsigned(value: Option<f64>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| format::percent_plain(value * 100.0),
    )
}

/// A percentage that may be missing.
fn optional(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format::percent(value * 100.0))
}

/// A plain ratio that may be missing.
fn ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.2}"))
}

/// Lowest and highest balance in the series.
fn value_bounds(series: &EquitySeries) -> Option<(f64, f64)> {
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for point in series.points() {
        low = low.min(point.wallet);
        high = high.max(point.wallet);
    }
    (low <= high).then_some((low, high))
}

/// Leave a little air above and below the curve.
fn pad(low: f64, high: f64) -> (f64, f64) {
    let span = high - low;
    if !span.is_finite() || span <= f64::EPSILON {
        let padding = (high.abs() * 0.01).max(1.0);
        return (low - padding, high + padding);
    }
    let padding = span * 0.06;
    (low - padding, high + padding)
}

/// Centre the overlay on the screen, bounded by the screen itself.
fn centered(area: Rect) -> Rect {
    let width = (area.width * WIDTH_PERCENT / 100).clamp(MIN_WIDTH.min(area.width), area.width);
    let height =
        (area.height * HEIGHT_PERCENT / 100).clamp(MIN_HEIGHT.min(area.height), area.height);

    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Colours the curve uses, for tests that check what was drawn.
#[cfg(test)]
pub(crate) const CURVE_COLOUR: ratatui::style::Color = theme::ACCENT;

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use crate::performance::{DAY_MS, EquityPoint, EquitySeries, Window};
    use crate::state::{App, Update};
    use crate::venue::{Interval, VenueId};

    use super::super::tests::{frame_cells, frame_lines, sample_app};
    use super::{CURVE_COLOUR, centered};

    /// A series with a rise, a fall and a recovery.
    fn sample_series(days: i64) -> EquitySeries {
        let now = crate::auth::now_ms();
        let points = (0..=days)
            .map(|index| {
                let wave = (index as f64 * 0.4).sin() * 120.0;
                EquityPoint {
                    time_ms: now - (days - index) * DAY_MS,
                    wallet: 10_000.0 + index as f64 * 12.0 + wave,
                    external_flow: 0.0,
                }
            })
            .collect();
        EquitySeries::new(points)
    }

    fn panel_app(days: i64) -> App {
        let mut app = sample_app();
        app.toggle_performance();
        app.apply(Update::Equity(sample_series(days)));
        app
    }

    /// Only the cells the overlay covers: the panels underneath stay visible.
    fn overlay_text(app: &App, width: u16, height: u16) -> String {
        let lines = frame_lines(app, width, height);
        let rect = centered(Rect::new(0, 0, width, height));

        lines[rect.y as usize..rect.bottom() as usize]
            .iter()
            .map(|line| {
                line.chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_panel_names_the_account_and_the_window() {
        let app = panel_app(120);
        let text = overlay_text(&app, 140, 44);

        assert!(text.contains("Performance"), "title: {text}");
        assert!(text.contains("main"), "account label: {text}");
        assert!(
            text.contains("all available history"),
            "the window is spelled out: {text}"
        );
    }

    #[test]
    fn the_panel_shows_every_metric() {
        let app = panel_app(120);
        let text = overlay_text(&app, 140, 44);

        for expected in [
            "total return",
            "CAGR",
            "volatility",
            "sharpe",
            "sortino",
            "max drawdown",
            "calmar",
            "win rate",
            "best day",
            "worst day",
        ] {
            assert!(text.contains(expected), "`{expected}` missing from: {text}");
        }
    }

    #[test]
    fn the_curve_is_drawn_in_the_panel() {
        let app = panel_app(120);
        let cells = frame_cells(&app, 140, 44);
        let curve = cells
            .iter()
            .filter(|(_, _, text, colour)| {
                *colour == Some(CURVE_COLOUR)
                    && text.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
            })
            .count();

        assert!(curve > 50, "expected a drawn curve, found {curve} cells");
    }

    #[test]
    fn the_window_selector_marks_the_active_window() {
        let mut app = panel_app(120);
        assert!(
            overlay_text(&app, 140, 44).contains("4 All"),
            "the windows are listed"
        );

        app.set_performance_window(Window::Month);
        let text = overlay_text(&app, 140, 44);
        assert!(text.contains("1 1M"), "got: {text}");
        assert!(
            text.contains("1 month"),
            "the title follows the window: {text}"
        );
    }

    #[test]
    fn shortening_the_window_shortens_the_coverage() {
        let mut app = panel_app(200);
        let all = overlay_text(&app, 140, 44);
        assert!(all.contains("201 points"), "everything: {all}");

        app.set_performance_window(Window::Month);
        let month = overlay_text(&app, 140, 44);
        // The series is built against one clock and rendered against a slightly
        // later one, so a month can be 30 or 31 daily points.
        assert!(
            month.contains("30 points") || month.contains("31 points"),
            "one month: {month}"
        );
        assert!(
            !month.contains("201 points"),
            "the window really cuts: {month}"
        );
    }

    #[test]
    fn an_empty_history_says_where_history_comes_from() {
        let mut app = sample_app();
        app.toggle_performance();

        let text = overlay_text(&app, 140, 44);
        assert!(
            text.contains("no history yet"),
            "it must not look broken: {text}"
        );
        assert!(
            text.contains("accumulates while cryptui runs"),
            "and must say how it fills: {text}"
        );
    }

    #[test]
    fn a_single_point_is_reported_as_too_little_to_measure() {
        let app = panel_app(0);
        let text = overlay_text(&app, 140, 44);

        assert!(
            text.contains("at least two days"),
            "one point is not a series: {text}"
        );
    }

    #[test]
    fn a_closed_panel_draws_nothing() {
        let app = sample_app();
        let text = frame_lines(&app, 140, 44).join("\n");
        assert!(!text.contains("Performance"), "got: {text}");
    }

    #[test]
    fn the_panel_fits_a_small_terminal() {
        for (width, height) in [(50u16, 12u16), (60, 16), (40, 10), (80, 24)] {
            let app = panel_app(60);
            let lines = frame_lines(&app, width, height);
            assert_eq!(lines.len(), height as usize);

            let rect = centered(Rect::new(0, 0, width, height));
            assert!(rect.right() <= width, "width fits at {width}x{height}");
            assert!(rect.bottom() <= height, "height fits at {width}x{height}");
        }
    }

    #[test]
    fn switching_accounts_keeps_the_panel_open_but_drops_the_history() {
        let mut app = panel_app(30);
        app.begin_account("Paper".to_owned(), VenueId::BinanceFutures);

        assert!(app.performance_is_open(), "the panel stays as it was");
        assert!(
            app.performance().series.is_empty(),
            "another account is another history"
        );
        assert!(app.take_history_request(), "and it asks for the new one");
    }

    #[test]
    fn the_interval_key_mapping_matches_the_panel() {
        let mut app = panel_app(30);
        for (window, label) in [
            (Window::Month, "1M"),
            (Window::Quarter, "3M"),
            (Window::Year, "1Y"),
            (Window::All, "All"),
        ] {
            app.set_performance_window(window);
            assert!(
                overlay_text(&app, 140, 44).contains(label),
                "the active window is {label}"
            );
            assert_eq!(app.performance().window, window);
        }
        let _ = Interval::M15;
    }
}
