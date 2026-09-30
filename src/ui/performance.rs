//! The account performance panel.
//!
//! A full-screen view: the curve and the figures that describe it, over a
//! selectable window. It takes over the screen rather than floating over the
//! chart, because a floating panel leaves the price axis and the positions table
//! poking out around its edges, which reads as clutter rather than as context.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::auth::now_ms;
use crate::performance::{CurveMode, EquitySeries, Window, describe_duration};
use crate::state::App;

use super::{format, theme};

/// Columns the figures are laid out in.
const GRID_COLUMNS: usize = 4;
/// Narrowest column that still reads as a column.
const MIN_COLUMN_WIDTH: usize = 16;
/// Width of the value axis, in cells.
const AXIS_WIDTH: u16 = 14;
/// Marks a point where money moved in or out of the account.
const FLOW_MARKER: &str = "\u{25c6}";
/// Colour of that marker, and of the note counting the flows.
const FLOW_COLOUR: ratatui::style::Color = theme::WARNING;
/// How long a fetched history stays current.
///
/// History is not a price feed: it is fetched on demand and only changes as the
/// day does, so a fetch from an hour ago is still fresh.
const FEED_TOLERANCE_MS: i64 = 3_600_000;

/// Draw the panel, if it is open.
pub(crate) fn render(frame: &mut Frame, app: &App) {
    let performance = app.performance();
    if !performance.open {
        return;
    }

    let now = now_ms();
    let area = frame.area();
    // The panels underneath are hidden rather than covered: this is a view.
    frame.render_widget(Clear, area);

    let series = performance.windowed(now);
    let block = Block::bordered()
        .title(title(app, performance.window))
        .border_style(theme::border_style(true));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Only the curve is flexible: giving the spacer room too would leave a
    // block of blank panel under the figures.
    let rows = Layout::vertical([
        Constraint::Min(6),    // the curve
        Constraint::Length(1), // dates under it
        Constraint::Length(1), // breathing room
        Constraint::Length(4), // the figures
        Constraint::Length(1), // breathing room
        Constraint::Length(1), // coverage
        Constraint::Length(1), // keys
    ])
    .split(inner);

    render_curve(frame, rows[0], &series, performance.mode);
    render_dates(frame, rows[1], &series);
    render_metrics(frame, rows[3], app, &series);
    render_coverage(frame, rows[5], &series, performance.mode);
    render_keys(frame, rows[6], app, performance.window);
}

/// ` Performance · Main · 3 months · performance index, 100 at the start · ● live 2s `
fn title(app: &App, window: Window) -> Line<'static> {
    let performance = app.performance();
    let fresh = performance.feed.summary(now_ms(), FEED_TOLERANCE_MS);

    Line::from(vec![
        Span::styled(
            " Performance ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {} · {} · {} ",
                app.account_label(),
                window.description(),
                performance.mode.label()
            ),
            Style::default().fg(theme::LABEL),
        ),
        Span::styled(format!("● {fresh} "), Style::default().fg(theme::LABEL)),
    ])
}

/// What the curve plots, and what its axis means.
struct Curve {
    /// One value per observation.
    values: Vec<f64>,
    mode: CurveMode,
    /// Where "no change" sits: 100 for the index, the opening balance otherwise.
    reference: Option<f64>,
}

impl Curve {
    /// The curve for a series in the given mode.
    fn of(series: &EquitySeries, mode: CurveMode) -> Self {
        match mode {
            CurveMode::Performance => Self {
                values: series.cumulative_index(),
                mode,
                reference: Some(100.0),
            },
            CurveMode::Balance => Self {
                values: series.points().iter().map(|point| point.wallet).collect(),
                mode,
                reference: series.points().first().map(|point| point.wallet),
            },
        }
    }

    /// Label for an axis value: a percentage for the index, money for balances.
    ///
    /// `decimals` widens the percentage when a window barely moves, so the ticks
    /// of a small account stay apart; money carries its own precision.
    fn label(&self, value: f64, decimals: usize) -> String {
        match self.mode {
            CurveMode::Performance => format::percent_decimals(value - 100.0, decimals),
            CurveMode::Balance => format::money(value),
        }
    }

    /// Decimals the value axis needs to keep its own ticks distinguishable.
    fn axis_decimals(&self, low: f64, high: f64) -> usize {
        if self.mode == CurveMode::Balance {
            return 2;
        }
        let range = (high - low).abs();
        if range >= 1.0 {
            1
        } else if range >= 0.1 {
            2
        } else {
            3
        }
    }

    /// Lowest and highest value, padded so the line never touches the edge.
    fn bounds(&self) -> Option<(f64, f64)> {
        let low = self.values.iter().copied().fold(f64::INFINITY, f64::min);
        let high = self
            .values
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        // Non-finite values cover the NaN case, so `high < low` is enough here.
        if !low.is_finite() || !high.is_finite() || high < low {
            return None;
        }

        let span = high - low;
        let padding = if span <= f64::EPSILON {
            (high.abs() * 0.01).max(1.0)
        } else {
            span * 0.08
        };
        Some((low - padding, high + padding))
    }

    /// The colour the line is drawn in.
    fn colour(&self) -> ratatui::style::Color {
        match self.mode {
            CurveMode::Performance => theme::ACCENT,
            CurveMode::Balance => theme::POSITIVE,
        }
    }
}

/// The curve, with its value axis on the right.
fn render_curve(frame: &mut Frame, area: Rect, series: &EquitySeries, mode: CurveMode) {
    if area.height == 0 || area.width <= AXIS_WIDTH {
        return;
    }

    if series.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no history yet: the venue serves only recent income, and the rest \
                 accumulates while cryptui runs",
                Style::default().fg(theme::WARNING),
            )))
            .centered(),
            area,
        );
        return;
    }

    let curve = Curve::of(series, mode);
    let Some((low, high)) = curve.bounds() else {
        return;
    };

    let columns =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(AXIS_WIDTH)]).split(area);

    let count = (curve.values.len().saturating_sub(1)).max(1) as f64;
    let flows: Vec<usize> = series
        .points()
        .iter()
        .enumerate()
        .filter(|(_, point)| point.external_flow.abs() > f64::EPSILON)
        .map(|(index, _)| index)
        .collect();

    let canvas = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([low, high])
        .marker(Marker::Braille)
        .paint(|context| {
            draw_curve(context, &curve, count, &flows);
        });
    frame.render_widget(canvas, columns[0]);

    frame.render_widget(
        Paragraph::new(Text::from(value_axis(&curve, low, high, columns[1].height)))
            .alignment(Alignment::Right),
        columns[1],
    );
}

/// Draw the reference line, the curve, and a tick wherever money moved.
fn draw_curve(context: &mut Context, curve: &Curve, count: f64, flows: &[usize]) {
    if let Some(reference) = curve.reference {
        context.draw(&CanvasLine::new(
            0.0,
            reference,
            count,
            reference,
            theme::LAST_PRICE,
        ));
    }

    for (index, pair) in curve.values.windows(2).enumerate() {
        context.draw(&CanvasLine::new(
            index as f64,
            pair[0],
            index as f64 + 1.0,
            pair[1],
            curve.colour(),
        ));
    }

    // A single observation still deserves a mark.
    if curve.values.len() == 1 {
        context.draw(&CanvasLine::new(
            0.0,
            curve.values[0],
            1.0,
            curve.values[0],
            curve.colour(),
        ));
    }

    // Money moving in or out is not performance, so it is marked rather than
    // allowed to look like a gain: a step in the balance should be explainable.
    // A glyph carries the meaning; dots alone would read as part of the curve.
    for index in flows {
        let Some(value) = curve.values.get(*index) else {
            continue;
        };
        context.print(
            *index as f64,
            *value,
            Span::styled(FLOW_MARKER, Style::default().fg(FLOW_COLOUR)),
        );
    }
}

/// Values at the top, middle and bottom, plus the reference line's own value.
fn value_axis(curve: &Curve, low: f64, high: f64, height: u16) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut lines = vec![Line::default(); height];
    if height == 0 {
        return lines;
    }

    let plain = Style::default().fg(theme::LABEL);
    let decimals = curve.axis_decimals(low, high);
    let label =
        |value: f64, style: Style| Line::from(Span::styled(curve.label(value, decimals), style));

    // The ticks the axis always carries: top, middle, bottom.
    let mut ticks = vec![(0usize, high)];
    if height >= 3 {
        ticks.push((height / 2, (low + high) / 2.0));
        ticks.push((height - 1, low));
    }
    for (row, value) in &ticks {
        lines[*row] = label(*value, plain);
    }

    if let Some(reference) = curve.reference
        && high > low
        && (low..=high).contains(&reference)
        && height >= 4
    {
        let row = ((high - reference) / (high - low) * (height - 1) as f64).round() as usize;
        // A window that opens flat could round the reference to the same string
        // as a tick; the line is on the chart either way, so the label would be
        // noise rather than information.
        let text = curve.label(reference, decimals);
        let repeated = ticks
            .iter()
            .any(|(tick_row, value)| *tick_row != row && curve.label(*value, decimals) == text);
        if row != 0 && row != height - 1 && !repeated {
            lines[row] = label(
                reference,
                Style::default()
                    .fg(theme::LAST_PRICE)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    lines
}

/// Date labels under the curve: first, middle and last observation.
fn render_dates(frame: &mut Frame, area: Rect, series: &EquitySeries) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let Some((from, to)) = series.span() else {
        return;
    };
    let middle = series
        .points()
        .get(series.len() / 2)
        .map_or(to, |point| point.time_ms);

    let width = area.width as usize;
    let mut row = vec![' '; width];
    let mut place = |text: &str, start: usize| {
        for (offset, character) in text.chars().enumerate() {
            if let Some(slot) = row.get_mut(start + offset) {
                *slot = character;
            }
        }
    };

    let first = format::day(from);
    let last = format::day(to);
    place(&first, 0);
    let last_start = width.saturating_sub(last.chars().count());
    if last_start > first.chars().count() {
        place(&last, last_start);
    }

    let centre = format::day(middle);
    let centre_start = width.saturating_sub(centre.chars().count()) / 2;
    if centre_start > first.chars().count() + 1
        && centre_start + centre.chars().count() + 1 < last_start
    {
        place(&centre, centre_start);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            row.into_iter().collect::<String>(),
            Style::default().fg(theme::LABEL),
        ))),
        area,
    );
}

/// The figures, in an aligned grid so the columns line up down the panel.
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

    let width = (usize::from(area.width) / GRID_COLUMNS).max(MIN_COLUMN_WIDTH) as u16;

    let mut lines = vec![
        grid_row(
            &[
                cell(
                    "total return",
                    percent(metrics.total_return),
                    sign(metrics.total_return),
                ),
                cell(
                    "CAGR",
                    optional(metrics.cagr),
                    sign(metrics.cagr.unwrap_or(0.0)),
                ),
                plain_cell("volatility", unsigned(metrics.volatility)),
                plain_cell("sharpe", ratio(metrics.sharpe)),
            ],
            width,
        ),
        grid_row(
            &[
                plain_cell("sortino", ratio(metrics.sortino)),
                cell(
                    "max drawdown",
                    percent(-metrics.max_drawdown),
                    sign(-metrics.max_drawdown),
                ),
                plain_cell("calmar", ratio(metrics.calmar)),
                plain_cell("win rate", unsigned(metrics.win_rate)),
            ],
            width,
        ),
        grid_row(
            &[
                cell(
                    "best day",
                    percent(metrics.best_day),
                    sign(metrics.best_day),
                ),
                cell(
                    "worst day",
                    percent(metrics.worst_day),
                    sign(metrics.worst_day),
                ),
                plain_cell("days", metrics.window_days().to_string()),
                plain_cell("returns", metrics.samples.to_string()),
            ],
            width,
        ),
    ];

    if let Some(account) = &app.account {
        lines.push(grid_row(
            &[
                plain_cell("wallet", format::money(account.wallet_balance)),
                plain_cell("equity", format::money(account.equity)),
                cell(
                    "unrealized",
                    format::signed_money(account.unrealized_pnl),
                    sign(account.unrealized_pnl),
                ),
                plain_cell(
                    "annualised",
                    if metrics.annualised_is_meaningful() {
                        "yes".to_owned()
                    } else {
                        format!(
                            "no, {} is short",
                            describe_duration(metrics.to_ms - metrics.from_ms)
                        )
                    },
                ),
            ],
            width,
        ));
    }

    lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// `coverage 2026-09-21 → 2026-09-30 · 8 days · 10 points · 1 deposit`
fn render_coverage(frame: &mut Frame, area: Rect, series: &EquitySeries, mode: CurveMode) {
    if area.height == 0 {
        return;
    }

    let flows = match series.flows().len() {
        0 => String::new(),
        1 => " · 1 deposit or withdrawal".to_owned(),
        count => format!(" · {count} deposits or withdrawals"),
    };
    let mode_note = match mode {
        CurveMode::Performance => " · net of deposits",
        CurveMode::Balance => "",
    };
    let units = income_note(series.assets());

    let text = match series.span() {
        Some((from, to)) => format!(
            "coverage {} → {} · {} · {} points{flows}{units}{mode_note}",
            format::date(from),
            format::date(to),
            describe_duration(to - from),
            series.len()
        ),
        None => "no coverage".to_owned(),
    };

    let spans: Vec<Span<'static>> = text
        .split_inclusive(" · ")
        .map(|part| {
            let style = if part.contains("deposit") {
                Style::default().fg(theme::WARNING)
            } else {
                Style::default().fg(theme::LABEL)
            };
            Span::styled(part.to_owned(), style)
        })
        .collect();

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Names the assets the income arrived in, when they are worth naming.
///
/// Currencies are not converted, so an account whose profits settle in credits,
/// or whose rebates arrive in BNB, is worth telling apart from one that simply
/// trades USDⓈ. A single stablecoin is the ordinary case and says nothing.
fn income_note(assets: &[String]) -> String {
    let dollar = |asset: &str| {
        matches!(
            asset,
            "USDT"
                | "USDC"
                | "FDUSD"
                | "USD1"
                | "BUSD"
                | "DAI"
                | "TUSD"
                | "BFUSD"
                | "RWUSD"
                | "LDUSDT"
        )
    };
    let unusual: Vec<&str> = assets
        .iter()
        .map(String::as_str)
        .filter(|asset| !dollar(asset))
        .collect();
    if unusual.is_empty() {
        return String::new();
    }
    format!(" · income in {}", unusual.join(", "))
}

/// The window selector and the keys, with the active window marked.
fn render_keys(frame: &mut Frame, area: Rect, app: &App, window: Window) {
    if area.height == 0 {
        return;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, candidate) in Window::ALL.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme::LABEL)));
        }
        spans.push(Span::styled(
            format!("{} {}", index + 1, candidate.label()),
            if *candidate == window {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(theme::LABEL)
            },
        ));
    }

    let next_mode = match app.performance().mode {
        CurveMode::Performance => "balance",
        CurveMode::Balance => "index",
    };
    spans.push(Span::styled(
        format!(
            "    [ ] cycle window · b show {next_mode} · e or Esc closes · {FLOW_MARKER} money in or out"
        ),
        Style::default().fg(theme::LABEL),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A grid cell: label, value, and the value's colour.
fn cell(label: &str, value: String, style: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), Style::default().fg(theme::LABEL)),
        Span::styled(value, style),
    ]
}

/// A grid cell whose value is not a gain or a loss.
fn plain_cell(label: &str, value: String) -> Vec<Span<'static>> {
    cell(label, value, Style::default())
}

/// Lay cells out in a row of equal columns, padded so they align across rows.
fn grid_row(cells: &[Vec<Span<'static>>], width: u16) -> Line<'static> {
    let target = usize::from(width);
    let mut spans: Vec<Span<'static>> = Vec::new();

    for group in cells {
        let used: usize = group.iter().map(|span| span.content.chars().count()).sum();
        spans.extend(group.iter().cloned());
        spans.push(Span::raw(" ".repeat(target.saturating_sub(used))));
    }

    Line::from(spans)
}

/// A percentage with its sign.
fn percent(value: f64) -> String {
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

/// The colour a figure is drawn in: by sign, or neutral.
fn sign(value: f64) -> Style {
    if value == 0.0 {
        Style::default()
    } else {
        theme::pnl_style(value)
    }
}

/// Colour of the curve, for tests that check what was drawn.
#[cfg(test)]
pub(crate) const CURVE_COLOUR: ratatui::style::Color = theme::ACCENT;
#[cfg(test)]
mod tests {
    use crate::performance::{CurveMode, DAY_MS, EquityPoint, EquitySeries, Window};
    use crate::state::{App, Update};
    use crate::venue::{Interval, VenueId};

    use super::super::tests::{frame_cells, frame_lines, sample_app};
    use super::{CURVE_COLOUR, FLOW_COLOUR, FLOW_MARKER, GRID_COLUMNS};

    /// A series with a rise, a dip, and optionally a deposit part-way through.
    fn sample_series(days: i64, with_flow: bool) -> EquitySeries {
        let now = crate::auth::now_ms();
        let points = (0..=days)
            .map(|index| {
                let wave = (index as f64 * 0.4).sin() * 120.0;
                let deposit = if with_flow && index == days / 2 {
                    2_500.0
                } else {
                    0.0
                };
                EquityPoint {
                    time_ms: now - (days - index) * DAY_MS,
                    wallet: 10_000.0 + index as f64 * 12.0 + wave + deposit,
                    external_flow: deposit,
                }
            })
            .collect();
        EquitySeries::new(points)
    }

    fn panel_app(days: i64, with_flow: bool) -> App {
        let mut app = sample_app();
        app.toggle_performance();
        app.apply(Update::Equity(sample_series(days, with_flow)));
        app
    }

    fn panel(app: &App) -> String {
        frame_lines(app, 140, 44).join("\n")
    }

    #[test]
    fn the_panel_takes_the_whole_screen() {
        let app = panel_app(60, false);
        let text = panel(&app);

        assert!(text.contains("Performance"), "the panel is drawn");
        assert!(
            !text.contains("binance_futures"),
            "the header underneath is hidden, not peeking around the edge: {text}"
        );
        assert!(
            !text.contains("Positions ("),
            "and so is the positions table: {text}"
        );
    }

    #[test]
    fn the_panel_names_the_account_the_window_and_the_curve() {
        let app = panel_app(120, false);
        let text = panel(&app);

        assert!(text.contains("main"), "account: {text}");
        assert!(text.contains("all available history"), "window: {text}");
        assert!(
            text.contains("performance index"),
            "the curve mode is stated: {text}"
        );
    }

    #[test]
    fn the_curve_shows_performance_by_default_and_balance_on_request() {
        let mut app = panel_app(120, true);
        let index = panel(&app);

        assert!(
            index.contains("0.0%"),
            "the index axis is a percentage: {index}"
        );
        assert!(index.contains("net of deposits"), "and says so: {index}");

        app.toggle_curve_mode();
        assert_eq!(app.performance().mode, CurveMode::Balance);
        let balance = panel(&app);
        assert!(
            balance.contains("wallet balance"),
            "the title follows: {balance}"
        );
        assert!(
            balance.contains("12,500.00") || balance.contains("10,000.00"),
            "the balance axis is money: {balance}"
        );
    }

    #[test]
    fn deposits_are_marked_on_the_curve() {
        let app = panel_app(120, true);
        let cells = frame_cells(&app, 140, 44);
        let marks = cells
            .iter()
            .filter(|(_, _, text, colour)| {
                *colour == Some(FLOW_COLOUR) && text.contains(FLOW_MARKER)
            })
            .count();

        assert!(marks > 0, "the deposit is marked, found {marks} cells");
        assert!(
            panel(&app).contains("1 deposit or withdrawal"),
            "and counted in the coverage line"
        );
    }

    #[test]
    fn the_curve_is_drawn() {
        let app = panel_app(120, false);
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
    fn a_narrow_range_is_labelled_with_enough_precision() {
        // A small account moves in fractions of a percent: at one decimal the
        // ticks collapse into the same string.
        let now = crate::auth::now_ms();
        let points = [4_000.0, 4_010.0, 3_990.0]
            .iter()
            .enumerate()
            .map(|(index, wallet)| EquityPoint {
                time_ms: now - (2 - index as i64) * DAY_MS,
                wallet: *wallet,
                external_flow: 0.0,
            })
            .collect();
        let mut app = sample_app();
        app.toggle_performance();
        app.apply(Update::Equity(EquitySeries::new(points)));

        let text = panel(&app);
        let ticks: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix('\u{2502}'))
            .map(str::trim)
            .filter(|cell| cell.ends_with('%'))
            .collect();

        assert!(ticks.len() >= 3, "the axis is labelled: {text}");
        assert!(
            ticks.iter().all(|tick| tick.len() > "+0.0%".len()),
            "a fraction of a percent needs two decimals: {ticks:?}"
        );
        assert_eq!(
            text.matches("+0.00%").count(),
            1,
            "the reference is labelled once, not twice: {text}"
        );
    }

    #[test]
    fn the_coverage_names_income_that_is_not_the_usual_currency() {
        use super::income_note;

        assert_eq!(income_note(&[]), "");
        assert_eq!(income_note(&["USDT".to_owned()]), "", "the ordinary case");
        assert_eq!(
            income_note(&["USDC".to_owned(), "USDT".to_owned()]),
            "",
            "dollar units are the ordinary case, however many"
        );
        assert_eq!(
            income_note(&["BNFCR".to_owned(), "BNB".to_owned(), "USDC".to_owned()]),
            " · income in BNFCR, BNB",
            "credits and rebates are worth naming, dollars are not"
        );
    }

    #[test]
    fn dates_are_labelled_under_the_curve() {
        let app = panel_app(200, false);
        let text = panel(&app);

        let stamps = text.matches("09-").count() + text.matches("08-").count();
        assert!(stamps >= 2, "expected date labels, got {stamps}: {text}");
    }

    #[test]
    fn the_metric_grid_aligns_its_columns() {
        let app = panel_app(120, false);
        let lines: Vec<String> = frame_lines(&app, 140, 44);
        let width = 140 / GRID_COLUMNS;

        let offsets: Vec<Option<usize>> = ["CAGR", "max drawdown", "worst day", "equity"]
            .iter()
            .map(|label| {
                lines
                    .iter()
                    .find(|line| line.contains(label))
                    .and_then(|line| line.find(label))
            })
            .collect();

        assert!(
            offsets.iter().all(Option::is_some),
            "every row was found: {offsets:?}"
        );
        let first = offsets[0].expect("a first row");
        for offset in offsets.iter().flatten() {
            assert_eq!(
                *offset, first,
                "columns must line up, got {offsets:?} (column width {width})"
            );
        }
    }

    #[test]
    fn every_metric_is_shown() {
        let app = panel_app(120, false);
        let text = panel(&app);

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
            "returns",
        ] {
            assert!(text.contains(expected), "`{expected}` missing from: {text}");
        }
    }

    #[test]
    fn the_window_selector_marks_the_active_window() {
        let mut app = panel_app(120, false);
        assert!(panel(&app).contains("4 All"), "the windows are listed");

        app.set_performance_window(Window::Month);
        assert!(
            panel(&app).contains("1 month"),
            "the title follows the window"
        );
    }

    #[test]
    fn shortening_the_window_shortens_the_coverage() {
        let mut app = panel_app(200, false);
        assert!(panel(&app).contains("201 points"), "everything");

        app.set_performance_window(Window::Month);
        let month = panel(&app);
        assert!(
            month.contains("30 points") || month.contains("31 points"),
            "one month: {month}"
        );
        assert!(!month.contains("201 points"), "the window cuts: {month}");
    }

    #[test]
    fn an_empty_history_says_where_history_comes_from() {
        let mut app = sample_app();
        app.toggle_performance();

        let text = panel(&app);
        assert!(text.contains("no history yet"), "got: {text}");
        assert!(
            text.contains("accumulates while cryptui runs"),
            "and how it fills: {text}"
        );
    }

    #[test]
    fn a_single_point_is_reported_as_too_little_to_measure() {
        let app = panel_app(0, false);
        let text = panel(&app);

        assert!(
            text.contains("at least two days"),
            "one point is not a series: {text}"
        );
    }

    #[test]
    fn a_closed_panel_draws_nothing() {
        let app = sample_app();
        let text = panel(&app);
        assert!(!text.contains("Performance"), "got: {text}");
        assert!(text.contains("Positions ("), "the normal view is back");
    }

    #[test]
    fn the_panel_survives_a_small_terminal() {
        for (width, height) in [(40u16, 12u16), (60, 16), (24, 8), (80, 24)] {
            let app = panel_app(60, true);
            let lines = frame_lines(&app, width, height);
            assert_eq!(lines.len(), height as usize);
        }
    }

    #[test]
    fn old_history_is_not_reported_as_stale() {
        let app = panel_app(60, false);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let text = panel(&app);

        assert!(!text.contains("stale"), "a recent fetch is not stale");
        assert!(text.contains("live"), "but the age is still reported");
    }

    #[test]
    fn switching_accounts_keeps_the_panel_open_but_drops_the_history() {
        let mut app = panel_app(30, false);
        app.begin_account("Paper".to_owned(), VenueId::BinanceFutures);

        assert!(app.performance_is_open(), "the panel stays as it was");
        assert!(app.performance().series.is_empty(), "another history");
        assert!(app.take_history_request(), "and it asks for the new one");
    }

    #[test]
    fn the_curve_mode_survives_an_account_switch() {
        let mut app = panel_app(30, false);
        app.toggle_curve_mode();
        app.begin_account("Paper".to_owned(), VenueId::BinanceFutures);

        assert_eq!(
            app.performance().mode,
            CurveMode::Balance,
            "a display preference, not account data"
        );
        let _ = Interval::M15;
    }
}
