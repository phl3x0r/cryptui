//! The candle chart: price pane with moving averages, volume pane, and axes.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Rectangle};
use ratatui::widgets::{Block, Paragraph};

use crate::auth::now_ms;
use crate::chart::MOVING_AVERAGE_WINDOWS;
use crate::state::{App, Chart};
use crate::venue::Kline;

use super::{format, theme};

/// Width of the price axis on the right, in cells.
const AXIS_WIDTH: u16 = 11;
/// Smallest price pane worth drawing.
const MIN_PRICE_HEIGHT: u16 = 3;
/// How far the price range may stretch to keep the entry line visible, as a
/// multiple of the visible candle range.
const ENTRY_RANGE_LIMIT: f64 = 3.0;
/// Half-width of a candle body, in candle slots.
const BODY_HALF_WIDTH: f64 = 0.34;
/// Colour of each moving-average line, in [`MOVING_AVERAGE_WINDOWS`] order.
const AVERAGE_COLORS: [Color; MOVING_AVERAGE_WINDOWS.len()] =
    [Color::Yellow, Color::LightMagenta, Color::LightBlue];

/// Draw the chart panel.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chart = &app.chart;
    // Resolved first: both the title and the plot annotate it.
    let entry = entry_price(app);
    let block = Block::bordered()
        .title(panel_title(app, entry))
        .border_style(theme::border_style(true));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Nothing legible fits in less than this.
    if inner.height < MIN_PRICE_HEIGHT + 2 || inner.width < AXIS_WIDTH + 10 {
        return;
    }
    if chart.candles.is_empty() {
        render_message(frame, inner, chart);
        return;
    }

    let Some((low, high)) = chart.price_bounds_with(entry, ENTRY_RANGE_LIMIT) else {
        return;
    };
    let (low, high) = pad_bounds(low, high);

    let plot_width = inner.width - AXIS_WIDTH;
    let columns = Layout::horizontal([
        Constraint::Length(plot_width),
        Constraint::Length(AXIS_WIDTH),
    ])
    .split(inner);

    // The volume pane is a fixed slice at the bottom of the price pane, with the
    // time axis under it; the axis column mirrors the same split so the price
    // labels line up with the prices they belong to.
    let volume_height = (inner.height / 4).clamp(3, 6);
    let constraints = [
        Constraint::Min(MIN_PRICE_HEIGHT),
        Constraint::Length(volume_height),
        Constraint::Length(1),
    ];
    let panes = Layout::vertical(constraints).split(columns[0]);
    let axis_panes = Layout::vertical(constraints).split(columns[1]);

    let count = chart.visible().len().max(1) as f64;
    let last_close = chart.last_close();

    let price = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([low, high])
        .marker(Marker::Braille)
        .paint(|context| {
            draw_last_price(context, last_close, count);
            draw_candles(context, chart.visible());
            if chart.overlays.averages {
                draw_averages(context, chart);
            }
            draw_entry(context, entry, count);
        });
    frame.render_widget(price, panes[0]);

    let volume = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([0.0, chart.volume_max().max(f64::MIN_POSITIVE)])
        .marker(Marker::Braille)
        .paint(|context| draw_volumes(context, chart.visible()));
    frame.render_widget(volume, panes[1]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            time_axis(chart.visible(), plot_width),
            Style::default().fg(theme::LABEL),
        ))),
        panes[2],
    );

    frame.render_widget(
        Paragraph::new(Text::from(price_axis(
            low,
            high,
            last_close,
            panes[0].height,
        )))
        .alignment(Alignment::Right),
        axis_panes[0],
    );
    if axis_panes[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format::quantity(chart.volume_max()),
                Style::default().fg(theme::LABEL),
            )))
            .alignment(Alignment::Right),
            Rect {
                height: 1,
                ..axis_panes[1]
            },
        );
    }
}

/// ` Chart  BTCUSDT · 15m  MA7 … MA25 … MA99 …  entry 84,200.00  ● live 2s `
fn panel_title(app: &App, entry: Option<f64>) -> Line<'static> {
    let chart = &app.chart;
    let symbol = chart.symbol.clone().unwrap_or_else(|| "—".to_owned());
    let mut spans = vec![
        Span::styled(
            " Chart ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {symbol} · {} ", chart.interval),
            Style::default().fg(theme::LABEL),
        ),
    ];

    if chart.overlays.averages {
        for ((window, value), color) in MOVING_AVERAGE_WINDOWS
            .iter()
            .zip(chart.latest_averages())
            .zip(AVERAGE_COLORS)
        {
            let text = match value {
                Some(value) => format!("MA{window} {}", format::price(value)),
                None => format!("MA{window} —"),
            };
            spans.push(Span::styled(
                format!("{text}  "),
                Style::default().fg(color),
            ));
        }
    } else {
        // Say that they are hidden, so the toggle is discoverable.
        spans.push(Span::styled(
            "MAs off (m)  ",
            Style::default().fg(theme::LABEL),
        ));
    }

    if let Some(price) = entry {
        spans.push(Span::styled(
            format!("entry {}  ", format::price(price)),
            Style::default().fg(theme::ENTRY),
        ));
    }

    // The feed's freshness belongs next to the thing it describes.
    let now = now_ms();
    let tolerance = chart.stale_after_ms(app.stale_after_ms);
    let stale = chart.feed.is_stale(now, tolerance);
    let feed_color = if chart.feed.last_error.is_some() {
        theme::ERROR
    } else if stale {
        theme::WARNING
    } else {
        theme::POSITIVE
    };
    spans.push(Span::styled("● ", Style::default().fg(feed_color)));
    spans.push(Span::styled(
        format!("{} ", chart.feed.summary(now, tolerance)),
        Style::default().fg(theme::LABEL),
    ));

    if !chart.is_following() {
        spans.push(Span::styled(
            "history (f to follow) ",
            Style::default().fg(theme::WARNING),
        ));
    }
    Line::from(spans)
}

/// Say why the chart is empty, rather than drawing an empty frame.
fn render_message(frame: &mut Frame, area: Rect, chart: &Chart) {
    let (text, color) = match &chart.feed.last_error {
        Some(error) => (format!("candles unavailable: {error}"), theme::ERROR),
        None => ("loading candles …".to_owned(), theme::LABEL),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))).centered(),
        area,
    );
}

/// Entry price to annotate: only when the overlay is on and the charted
/// contract is actually held.
fn entry_price(app: &App) -> Option<f64> {
    if !app.chart.overlays.entry {
        return None;
    }
    app.chart_position().map(|position| position.entry_price)
}

/// Draw the position's entry price as a line across the pane.
fn draw_entry(context: &mut Context, entry: Option<f64>, count: f64) {
    if let Some(price) = entry {
        context.draw(&CanvasLine::new(0.0, price, count, price, theme::ENTRY));
    }
}

/// Draw the newest close as a reference line across the pane.
fn draw_last_price(context: &mut Context, last_close: Option<f64>, count: f64) {
    if let Some(price) = last_close {
        context.draw(&CanvasLine::new(
            0.0,
            price,
            count,
            price,
            theme::LAST_PRICE,
        ));
    }
}

/// Draw wicks and bodies, coloured by direction.
fn draw_candles(context: &mut Context, candles: &[Kline]) {
    for (index, candle) in candles.iter().enumerate() {
        let center = index as f64 + 0.5;
        let color = candle_color(candle);
        context.draw(&CanvasLine::new(
            center,
            candle.low,
            center,
            candle.high,
            color,
        ));

        let (bottom, top) = (candle.open.min(candle.close), candle.open.max(candle.close));
        let height = top - bottom;
        if height <= f64::EPSILON {
            // A doji still deserves a visible line.
            context.draw(&CanvasLine::new(
                center - BODY_HALF_WIDTH,
                top,
                center + BODY_HALF_WIDTH,
                top,
                color,
            ));
        } else {
            context.draw(&Rectangle {
                x: center - BODY_HALF_WIDTH,
                y: bottom,
                width: BODY_HALF_WIDTH * 2.0,
                height,
                color,
            });
        }
    }
}

/// Draw the moving-average lines, skipping the stretch before each is defined.
fn draw_averages(context: &mut Context, chart: &Chart) {
    for (index, color) in AVERAGE_COLORS.into_iter().enumerate() {
        let values = chart.visible_average(index);
        let mut previous: Option<(f64, f64)> = None;
        for (position, value) in values.iter().enumerate() {
            match (previous, value) {
                (Some((x1, y1)), Some(current)) => {
                    let x2 = position as f64 + 0.5;
                    context.draw(&CanvasLine::new(x1, y1, x2, *current, color));
                    previous = Some((x2, *current));
                }
                (_, Some(current)) => previous = Some((position as f64 + 0.5, *current)),
                (_, None) => previous = None,
            }
        }
    }
}

/// Draw volumes as bars from the baseline of the volume pane.
fn draw_volumes(context: &mut Context, candles: &[Kline]) {
    for (index, candle) in candles.iter().enumerate() {
        let center = index as f64 + 0.5;
        context.draw(&Rectangle {
            x: center - BODY_HALF_WIDTH,
            y: 0.0,
            width: BODY_HALF_WIDTH * 2.0,
            height: candle.volume,
            color: candle_color(candle),
        });
    }
}

/// Colour of a candle: green when it closed at or above its open.
fn candle_color(candle: &Kline) -> Color {
    if candle.close >= candle.open {
        theme::POSITIVE
    } else {
        theme::NEGATIVE
    }
}

/// Price labels for the axis: high at the top, low at the bottom, the newest
/// close marked where it falls.
fn price_axis(low: f64, high: f64, last_close: Option<f64>, height: u16) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut lines = vec![Line::default(); height];
    if height == 0 {
        return lines;
    }

    let label = |price: f64, style: Style| Line::from(Span::styled(format::price(price), style));
    let plain = Style::default().fg(theme::LABEL);

    lines[0] = label(high, plain);
    if height >= 3 {
        lines[height / 2] = label((low + high) / 2.0, plain);
        lines[height - 1] = label(low, plain);
    }

    if let Some(close) = last_close
        && low <= close
        && close <= high
        && high > low
        && height >= 4
    {
        let fraction = (high - close) / (high - low);
        let row = (fraction * (height - 1) as f64).round() as usize;
        if row != 0 && row != height - 1 && row != height / 2 {
            lines[row] = label(
                close,
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    lines
}

/// Time labels under the chart: the first, middle and last visible candle.
fn time_axis(candles: &[Kline], width: u16) -> String {
    let width = width as usize;
    if width == 0 || candles.is_empty() {
        return String::new();
    }

    let mut row = vec![' '; width];
    let mut place = |text: &str, start: usize| {
        for (offset, character) in text.chars().enumerate() {
            if let Some(slot) = row.get_mut(start + offset) {
                *slot = character;
            }
        }
    };

    let first = format::timestamp(candles[0].open_time_ms);
    let last = format::timestamp(candles[candles.len() - 1].open_time_ms);
    place(&first, 0);
    let last_start = width.saturating_sub(last.chars().count());
    if last_start >= first.chars().count() + 2 {
        place(&last, last_start);
    }

    if candles.len() >= 3 {
        let middle = format::timestamp(candles[candles.len() / 2].open_time_ms);
        let middle_start = (width.saturating_sub(middle.chars().count())) / 2;
        let clear_of_first = middle_start >= first.chars().count() + 2;
        let clear_of_last = middle_start + middle.chars().count() + 2 <= last_start;
        if clear_of_first && clear_of_last {
            place(&middle, middle_start);
        }
    }

    row.into_iter().collect()
}

/// Expand a price range so candles never sit exactly on the pane edge.
fn pad_bounds(low: f64, high: f64) -> (f64, f64) {
    let span = high - low;
    if !span.is_finite() || span <= f64::EPSILON {
        // A flat series still needs a range to map onto.
        let padding = (high.abs() * 0.005).max(1e-8);
        return (low - padding, high + padding);
    }
    let padding = span * 0.04;
    (low - padding, high + padding)
}

#[cfg(test)]
mod tests {
    use crate::state::{App, Update};
    use crate::venue::Interval;

    use super::super::tests::{frame_lines, sample_account, sample_app, sample_candles};

    /// The chart panel's title row, where the overlay legend lives.
    ///
    /// Assertions must be scoped to it: the header also mentions the selected
    /// position's entry price, so a whole-frame check would pass or fail for the
    /// wrong reason.
    fn title_line(app: &App) -> String {
        frame_lines(app, 140, 40)
            .into_iter()
            .find(|line| line.contains("Chart"))
            .expect("the chart panel is rendered")
    }

    /// An app whose chart holds deterministic candles.
    fn chart_app() -> App {
        let mut app = sample_app();
        app.set_chart_symbol("BTCUSDT".to_owned());
        app.chart.reset("BTCUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "BTCUSDT".to_owned(),
            interval: Interval::M15,
            candles: sample_candles(),
        });
        app
    }

    #[test]
    fn the_title_names_the_target_and_the_averages() {
        let text = frame_lines(&chart_app(), 140, 40).join("\n");

        assert!(text.contains("Chart"), "panel is titled: {text}");
        assert!(text.contains("BTCUSDT"), "symbol: {text}");
        assert!(text.contains("15m"), "interval: {text}");
        assert!(text.contains("MA7"), "fast average legend: {text}");
        assert!(text.contains("MA25"), "medium average legend: {text}");
        assert!(text.contains("MA99"), "slow average legend: {text}");
    }

    #[test]
    fn candles_are_drawn_in_the_plot_area() {
        let lines = frame_lines(&chart_app(), 140, 40);
        let glyphs = ['⠁', '⠉', '⣿', '⠿', '⡀', '⢀', '⠤'];

        let painted = lines
            .iter()
            .filter(|line| glyphs.iter().any(|glyph| line.contains(*glyph)))
            .count();
        assert!(
            painted >= 4,
            "expected candle and volume pixels, painted rows: {painted}"
        );
    }

    #[test]
    fn the_price_axis_labels_the_range() {
        let app = chart_app();
        let text = frame_lines(&app, 140, 40).join("\n");
        // The axis is padded so candles do not touch the pane edge, so the
        // labels carry the padded bounds rather than the raw extremes.
        let (raw_low, raw_high) = app.chart.price_bounds().expect("bounds");
        let (low, high) = super::pad_bounds(raw_low, raw_high);

        assert!(
            text.contains(&crate::ui::format::price(high)),
            "high label {high} missing from: {text}"
        );
        assert!(
            text.contains(&crate::ui::format::price(low)),
            "low label {low} missing from: {text}"
        );
    }

    #[test]
    fn the_time_axis_labels_the_visible_range() {
        let text = frame_lines(&chart_app(), 140, 40).join("\n");
        assert!(
            text.contains("09-30") || text.contains("10-0"),
            "a date label should be visible: {text}"
        );
    }

    #[test]
    fn pressing_h_steps_into_history_and_stops_following() {
        let mut app = chart_app();
        assert!(
            app.chart.is_following(),
            "starts pinned to the newest candle"
        );

        app.pan_chart(-15);
        let text = frame_lines(&app, 140, 40).join("\n");
        assert!(!app.chart.is_following());
        assert!(
            text.contains("history"),
            "the pane says it is not following"
        );

        app.follow_chart();
        assert!(app.chart.is_following());
        assert!(!frame_lines(&app, 140, 40).join("\n").contains("history"));
    }

    #[test]
    fn zooming_changes_how_many_candles_are_shown() {
        let mut app = chart_app();
        let before = app.chart.visible().len();

        app.zoom_chart(1.25);
        assert!(app.chart.visible().len() > before, "zooming out shows more");

        app.zoom_chart(0.5);
        assert!(app.chart.visible().len() < before, "zooming in shows fewer");
    }

    #[test]
    fn hiding_the_averages_removes_their_lines_and_legend() {
        let mut app = chart_app();
        assert!(
            frame_lines(&app, 140, 40).join("\n").contains("MA7"),
            "the legend is there by default"
        );

        app.toggle_averages();
        let text = frame_lines(&app, 140, 40).join("\n");
        assert!(!text.contains("MA7"), "the legend is gone: {text}");
        assert!(
            text.contains("MAs off (m)"),
            "the title says they are hidden and how to get them back: {text}"
        );
        assert!(text.contains("BTCUSDT"), "the chart itself still renders");
    }

    #[test]
    fn the_entry_line_is_annotated_for_a_held_contract() {
        let title = title_line(&chart_app());
        assert!(
            title.contains("entry 85,000.00"),
            "the held contract's entry price is labelled: {title}"
        );
    }

    #[test]
    fn hiding_the_entry_line_removes_its_annotation() {
        let mut app = chart_app();
        app.toggle_entry_line();

        let title = title_line(&app);
        assert!(!title.contains("entry"), "got: {title}");
        assert!(title.contains("MA7"), "the averages are unaffected");
    }

    #[test]
    fn a_contract_that_is_not_held_has_no_entry_line() {
        let mut app = sample_app();
        app.set_chart_symbol("SOLUSDT".to_owned());
        app.chart.reset("SOLUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "SOLUSDT".to_owned(),
            interval: Interval::M15,
            candles: sample_candles(),
        });

        let title = title_line(&app);
        assert!(title.contains("SOLUSDT"), "the chart renders it: {title}");
        assert!(
            !title.contains("entry"),
            "there is no position, so no entry line: {title}"
        );
    }

    #[test]
    fn an_empty_chart_says_what_it_is_waiting_for() {
        let mut app = sample_app();
        app.chart.reset("ETHUSDT".to_owned(), Interval::M15);

        let text = frame_lines(&app, 140, 40).join("\n");
        assert!(text.contains("loading candles"), "got: {text}");
        assert!(text.contains("ETHUSDT"), "the target is still named");
    }

    #[test]
    fn a_failed_chart_feed_reports_the_reason() {
        let mut app = sample_app();
        app.chart.reset("ETHUSDT".to_owned(), Interval::M15);
        app.apply(Update::Failed {
            feed: crate::state::Feed::Chart,
            message: "invalid symbol".to_owned(),
        });

        let text = frame_lines(&app, 140, 40).join("\n");
        assert!(
            text.contains("candles unavailable: invalid symbol"),
            "got: {text}"
        );
    }

    #[test]
    fn a_flat_market_still_renders() {
        let mut app = sample_app();
        app.chart.reset("FLATUSDT".to_owned(), Interval::H1);
        let flat = vec![crate::venue::Kline {
            open_time_ms: 1_790_773_200_000,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 0.0,
            close_time_ms: 1_790_776_799_999,
            closed: true,
        }];
        app.apply(Update::History {
            symbol: "FLATUSDT".to_owned(),
            interval: Interval::H1,
            candles: flat,
        });

        let text = frame_lines(&app, 140, 40).join("\n");
        assert!(text.contains("FLATUSDT"), "still renders: {text}");
        assert!(text.contains("100.00"), "the single price is labelled");
    }

    #[test]
    fn tiny_terminals_do_not_panic_or_draw_outside_the_pane() {
        for height in 6..=20u16 {
            for width in 20..=90u16 {
                let lines = frame_lines(&chart_app(), width, height);
                assert_eq!(lines.len(), height as usize);
            }
        }
    }

    #[test]
    fn the_volume_pane_scales_to_the_largest_visible_bar() {
        let app = chart_app();
        let expected = app
            .chart
            .visible()
            .iter()
            .map(|candle| candle.volume)
            .fold(0.0, f64::max);
        assert!((app.chart.volume_max() - expected).abs() < f64::EPSILON);
        assert!(expected > 0.0, "the fixture has volume");

        let _ = sample_account();
    }
}
