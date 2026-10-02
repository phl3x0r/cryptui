//! The candle chart: price pane with moving averages, volume pane, and axes.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Block, Paragraph};

use crate::auth::now_ms;
use crate::chart::{CandleLayout, MOVING_AVERAGE_WINDOWS};
use crate::state::{App, Chart};
use crate::venue::Kline;

use super::{format, theme};

/// Width of the price axis on the right, in cells.
const AXIS_WIDTH: u16 = 11;
/// Smallest price pane worth drawing.
const MIN_PRICE_HEIGHT: u16 = 3;
/// Braille dots per cell row, which is the vertical resolution the canvas maps
/// prices onto.
const DOTS_PER_CELL_ROW: usize = 4;
/// How far the price range may stretch to keep the entry line visible, as a
/// multiple of the visible candle range.
const ENTRY_RANGE_LIMIT: f64 = 3.0;
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

    // The pane tells the chart how many candles it can draw before anything is
    // measured from them: an even pitch may fit a few more or fewer than the
    // viewport asked for, and the bounds have to follow what is drawn.
    let plot_width = inner.width - AXIS_WIDTH;
    chart.set_columns(plot_width);

    let Some((low, high)) = chart.price_bounds_with(entry, ENTRY_RANGE_LIMIT) else {
        return;
    };
    let (low, high) = pad_bounds(low, high);

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

    let candles = chart.visible();
    let count = candles.len().max(1) as f64;
    let slots = Slots::of(chart.layout(), plot_width, count);
    let rows = Rows::new(low, high, panes[0].height);
    let last_close = chart.last_close();

    let price = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([low, high])
        .marker(Marker::Braille)
        .paint(|context| {
            // A Braille cell holds one colour, so whatever is drawn last wins
            // it. The reference lines go first and the candles last: an overlay
            // must never recolour the bars it crosses, which reads as the bars
            // changing direction.
            draw_last_price(context, last_close, count);
            if chart.overlays.averages {
                draw_averages(context, chart, &slots);
            }
            draw_entry(context, entry, count);
            draw_candles(context, candles, &slots, rows);
        });
    frame.render_widget(price, panes[0]);

    let volume_max = chart.volume_max().max(f64::MIN_POSITIVE);
    let volume_rows = Rows::new(0.0, volume_max, panes[1].height);
    let volume = Canvas::default()
        .x_bounds([0.0, count])
        .y_bounds([0.0, volume_max])
        .marker(Marker::Braille)
        .paint(|context| draw_volumes(context, candles, &slots, volume_rows));
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
            entry,
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

/// Where each candle sits across the plot, in canvas x units.
///
/// The cells come from the chart's [`Layout`]; this only converts them to the
/// units the canvas speaks. The conversion is done per **dot** rather than per
/// cell because the canvas grid is `2 * columns - 1` dots wide, so a cell is not
/// a whole number of x units and anything computed per cell drifts across the
/// pane.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Slots {
    /// The cells each candle occupies.
    layout: CandleLayout,
    /// Canvas x units per dot.
    dot: f64,
}

impl Slots {
    /// Place `layout` across a domain `width` wide.
    fn of(layout: CandleLayout, columns: u16, width: f64) -> Self {
        let dots = 2.0 * f64::from(columns) - 1.0;
        Self {
            layout,
            dot: width / dots.max(1.0),
        }
    }

    /// The body of candle `index`: left edge and width, in canvas x units.
    ///
    /// Both ends land exactly on dots, and the last one is included, so a body
    /// covers its own cells and never one dot of the next.
    fn body_of(&self, index: usize) -> (f64, f64) {
        let cells = self.layout.cells(index);
        let left = 2.0 * f64::from(cells.start);
        let dots = 2.0 * f64::from(cells.end - cells.start) - 1.0;
        (left * self.dot, dots * self.dot)
    }

    /// The wick of candle `index`: the dot it is drawn on, in canvas x units.
    ///
    /// Always one dot wide. A whole cell of wick fits exactly on the middle of an
    /// odd body, but that made the wick's width change from one zoom level to the
    /// next, which reads as flicker rather than as a candle. A body spans an even
    /// number of dots whatever its width, so no wick can land exactly on the
    /// middle; it goes on the dot just left of it, half a dot out.
    fn wick(&self, index: usize) -> f64 {
        let body = f64::from(self.layout.body);
        (2.0 * f64::from(self.layout.cells(index).start) + body - 1.0) * self.dot
    }

    /// The candle's centre, in canvas x units.
    fn centre(&self, index: usize) -> f64 {
        let body = f64::from(self.layout.body);
        (2.0 * f64::from(self.layout.cells(index).start) + body - 0.5) * self.dot
    }
}

/// Draw wicks and bodies, coloured by direction.
fn draw_candles(context: &mut Context, candles: &[Kline], slots: &Slots, rows: Rows) {
    for (index, candle) in candles.iter().enumerate() {
        let color = candle_color(candle);
        let (x, width) = slots.body_of(index);
        // A fill of no width is a one-dot vertical line, which is a wick.
        rows.fill(
            context,
            slots.wick(index),
            0.0,
            candle.low,
            candle.high,
            color,
        );

        let (bottom, top) = (candle.open.min(candle.close), candle.open.max(candle.close));
        if top - bottom <= f64::EPSILON {
            // A doji still deserves a visible line.
            context.draw(&CanvasLine::new(x, top, x + width, top, color));
        } else {
            // Filled row by row rather than outlined: four lines leave a body
            // hollow once it is more than a couple of cells wide.
            rows.fill(context, x, width, bottom, top, color);
        }
    }
}

/// One dot row of the price pane, in price units.
///
/// The canvas maps prices onto Braille dots, four to a cell row, so this is the
/// step that paints every row exactly once.
#[derive(Debug, Clone, Copy)]
struct Rows {
    /// Price per dot row.
    step: f64,
}

impl Rows {
    /// The rows for a pane `height` cells tall covering `low..high`.
    fn new(low: f64, high: f64, height: u16) -> Self {
        let dots = f64::from(height) * DOTS_PER_CELL_ROW as f64;
        let step = if dots > 1.0 {
            (high - low) / (dots - 1.0)
        } else {
            (high - low).max(f64::MIN_POSITIVE)
        };
        Self {
            step: step.abs().max(f64::MIN_POSITIVE),
        }
    }

    /// Paint `bottom..top` as horizontal lines one dot row apart, so the shape is
    /// solid rather than an outline of four lines.
    fn fill(&self, context: &mut Context, x: f64, width: f64, bottom: f64, top: f64, color: Color) {
        let mut price = bottom;
        while price < top {
            context.draw(&CanvasLine::new(x, price, x + width, price, color));
            price += self.step;
        }
        context.draw(&CanvasLine::new(x, top, x + width, top, color));
    }
}

/// Draw the moving-average lines, skipping the stretch before each is defined.
fn draw_averages(context: &mut Context, chart: &Chart, slots: &Slots) {
    for (index, color) in AVERAGE_COLORS.into_iter().enumerate() {
        let values = chart.visible_average(index);
        // Follows the candle centres, so the line crosses the bars it describes.
        let x = |position: usize| slots.centre(position);
        let mut previous: Option<(f64, f64)> = None;
        for (position, value) in values.iter().enumerate() {
            match (previous, value) {
                (Some((x1, y1)), Some(current)) => {
                    let x2 = x(position);
                    context.draw(&CanvasLine::new(x1, y1, x2, *current, color));
                    previous = Some((x2, *current));
                }
                (_, Some(current)) => previous = Some((x(position), *current)),
                (_, None) => previous = None,
            }
        }
    }
}

/// Draw volumes as bars from the baseline of the volume pane.
fn draw_volumes(context: &mut Context, candles: &[Kline], slots: &Slots, rows: Rows) {
    for (index, candle) in candles.iter().enumerate() {
        let (x, width) = slots.body_of(index);
        rows.fill(context, x, width, 0.0, candle.volume, candle_color(candle));
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
fn price_axis(
    low: f64,
    high: f64,
    last_close: Option<f64>,
    entry: Option<f64>,
    height: u16,
) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut lines = vec![Line::default(); height];
    if height == 0 {
        return lines;
    }

    let label = |price: f64, style: Style| Line::from(Span::styled(format::price(price), style));
    let plain = Style::default().fg(theme::LABEL);
    let marked = |colour| Style::default().fg(colour).add_modifier(Modifier::BOLD);

    lines[0] = label(high, plain);
    if height >= 3 {
        lines[height / 2] = label((low + high) / 2.0, plain);
        lines[height - 1] = label(low, plain);
    }

    // Annotations replace grid labels rather than being hidden behind them:
    // knowing where the position's entry sits is the point of the pane. The
    // live price keeps its row when the two would collide, because the entry is
    // already drawn as a line across the pane.
    let mut close_row = None;
    if high > low {
        if let Some(close) = last_close.filter(|close| low <= *close && *close <= high) {
            let row = axis_row(close, low, high, height);
            lines[row] = label(close, marked(theme::ACCENT));
            close_row = Some(row);
        }
        if let Some(price) = entry.filter(|price| low <= *price && *price <= high) {
            let row = axis_row(price, low, high, height);
            if Some(row) != close_row {
                lines[row] = label(price, marked(theme::ENTRY));
            }
        }
    }

    lines
}

/// Axis row a price falls on: the top row is the highest price.
///
/// Mirrors the canvas exactly. The canvas maps a price onto Braille *dots* —
/// four per cell row — so a label computed from cell rows alone lands a row away
/// from the line drawn at the same price.
fn axis_row(price: f64, low: f64, high: f64, height: usize) -> usize {
    if height == 0 || high <= low {
        return 0;
    }

    let dots = (height * DOTS_PER_CELL_ROW) as f64;
    let dot = ((high - price) * (dots - 1.0) / (high - low)).round();
    let dot = dot.clamp(0.0, dots - 1.0) as usize;
    (dot / DOTS_PER_CELL_ROW).min(height - 1)
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

    use super::super::tests::{
        frame_cells, frame_lines, sample_account, sample_app, sample_candles,
    };
    use ratatui::layout::Rect;

    use super::{AVERAGE_COLORS, Slots, axis_row, theme};
    use crate::chart::CandleLayout;

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
        // Rendered first: the pane tells the chart how many candles it can draw,
        // and the bounds follow from that.
        let text = frame_lines(&app, 140, 40).join("\n");
        // The axis is padded so candles do not touch the pane edge, and widened
        // to keep the entry line on screen, so the labels carry bounds rather than
        // the raw extremes.
        let entry = super::entry_price(&app);
        let (raw_low, raw_high) = app
            .chart
            .price_bounds_with(entry, super::ENTRY_RANGE_LIMIT)
            .expect("bounds");
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

    /// The right-hand axis column of every rendered row.
    ///
    /// Rendered just narrow enough that the account panel is not in the column:
    /// this is about the chart's own axis, which only reaches the screen edge
    /// when nothing sits beside it.
    fn axis_column(app: &App) -> String {
        frame_lines(app, crate::ui::ACCOUNT_MIN_WIDTH - 1, 40)
            .iter()
            .map(|line| {
                let reversed: String = line.chars().rev().take(11).collect();
                reversed.chars().rev().collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Columns of the price pane that carry a candle body.
    ///
    /// Scoped to the pane's own rows and columns: the title, the axis and the
    /// account panel beside the chart all carry coloured text of their own, and a
    /// whole-frame sweep picks those up instead of the bars.
    fn bar_columns(app: &App, width: u16, height: u16) -> Vec<u16> {
        let chart = super::super::split(Rect::new(0, 0, width, height)).chart;
        let inner = Rect::new(
            chart.x + 1,
            chart.y + 1,
            chart.width.saturating_sub(2),
            chart.height.saturating_sub(2),
        );
        // Mirrors the renderer: the price pane is what is left above the volume
        // pane and the time axis.
        let volume_height = (inner.height / 4).clamp(3, 6);
        let price_height = inner.height.saturating_sub(volume_height + 1);
        let plot_width = inner.width.saturating_sub(super::AXIS_WIDTH);
        let cells = frame_cells(app, width, height);

        (inner.x..inner.x + plot_width)
            .filter(|x| {
                cells.iter().any(|(cx, cy, _, colour)| {
                    cx == x
                        && *cy >= inner.y
                        && *cy < inner.y + price_height
                        && matches!(colour, Some(colour)
                            if *colour == theme::POSITIVE || *colour == theme::NEGATIVE)
                })
            })
            .collect()
    }

    /// The widths of the bars and the gaps between them, in cells.
    fn bar_widths_and_gaps(app: &App, width: u16, height: u16) -> (Vec<usize>, Vec<usize>) {
        let mut widths: Vec<usize> = Vec::new();
        let mut gaps: Vec<usize> = Vec::new();
        let mut previous: Option<u16> = None;

        for column in bar_columns(app, width, height) {
            match previous {
                Some(last) if last + 1 == column => {
                    *widths.last_mut().expect("a run is open") += 1;
                }
                Some(last) => {
                    gaps.push(usize::from(column - last - 1));
                    widths.push(1);
                }
                None => widths.push(1),
            }
            previous = Some(column);
        }

        (widths, gaps)
    }

    #[test]
    fn every_bar_has_the_same_width_and_the_same_gap_at_every_zoom() {
        // A Braille cell holds two dots and one colour, so a bar that shares a
        // cell with its neighbour erases part of it, and a body that is not a
        // whole number of cells comes out uneven. The chart gives up a few
        // candles of the count for an even pitch, at every zoom level.
        for zoom in [0.25, 0.4, 0.6, 0.8, 1.0, 1.5, 2.0, 3.0] {
            let mut app = chart_app();
            app.zoom_chart(zoom);
            // The tightest pitch leaves no room for daylight, so the bars form
            // one block; everywhere else they are separated.
            let (widths, gaps) = bar_widths_and_gaps(&app, 240, 40);

            assert!(!widths.is_empty(), "zoom {zoom}: bars are drawn");
            assert!(
                widths.iter().all(|width| *width == widths[0]),
                "zoom {zoom}: every bar is one width: {widths:?}"
            );
            assert!(
                gaps.iter().all(|gap| *gap == gaps[0]),
                "zoom {zoom}: and evenly spaced: {gaps:?}"
            );
        }
    }

    #[test]
    fn the_forming_candle_repaints_as_the_price_moves() {
        // The report: "the latest candle does not redraw with price changes".
        // The window had stopped following the live edge, so the candle at it was
        // a closed one; with following intact the forming candle is the one drawn
        // last, and it has to move as the price does.
        let mut app = chart_app();
        let before = candle_cells(&app);

        let forming = app.chart.candles.last().copied().expect("a last candle");
        let mut moved = forming;
        moved.close = forming.close + 500.0;
        moved.high = moved.close + 100.0;
        app.chart.upsert(moved);
        let after = candle_cells(&app);

        assert_ne!(before, after, "the forming candle redraws when it moves");
    }

    #[test]
    fn a_wick_is_centred_on_its_bar() {
        for columns in [40u16, 95, 195, 240] {
            for requested in [8usize, 20, 33, 50, 80, 120] {
                let layout = CandleLayout::fit(Some(columns), requested);
                let slots = Slots::of(layout, columns, requested as f64);

                for index in [0, 1, layout.candles / 2, layout.candles - 1] {
                    let cells = layout.cells(index);
                    let left = 2.0 * f64::from(cells.start);
                    let right = 2.0 * f64::from(cells.end) - 1.0;
                    let wick = slots.wick(index) / slots.dot;

                    assert!(
                        wick >= left - 1e-9 && wick <= right + 1e-9,
                        "{layout:?}: the wick leaves its bar"
                    );
                    // Dot positions are floats by the time they are drawn, and the
                    // canvas rounds them, so half a dot is half a dot give or take.
                    let body_centre = (left + right) / 2.0;
                    assert!(
                        (wick - body_centre).abs() <= 0.5 + 1e-9,
                        "{layout:?}: wick at {wick} against body centre {body_centre}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_wick_is_the_same_width() {
        // One dot, at every zoom. A wick twice as wide fits an odd body exactly,
        // but its width changed with the pitch, so the wicks flickered as the
        // chart was zoomed.
        for columns in [40u16, 95, 195, 240] {
            for requested in [8usize, 20, 33, 50, 80, 120] {
                let layout = CandleLayout::fit(Some(columns), requested);
                let slots = Slots::of(layout, columns, requested as f64);

                for index in 0..layout.candles.min(5) {
                    let wick = slots.wick(index) / slots.dot;
                    let body = layout.cells(index);
                    let body_left = 2.0 * f64::from(body.start);
                    let body_dots = 2.0 * f64::from(body.end - body.start);

                    // A whole dot, never a fraction: it is drawn as a fill of no
                    // width at a dot the canvas rounds to.
                    assert!(
                        (wick - wick.round()).abs() < 1e-9,
                        "{layout:?}: a fraction of a dot of wick at {wick}"
                    );
                    assert!(wick >= body_left, "{layout:?}: the wick leaves its bar");
                    assert!(
                        wick < body_left + body_dots,
                        "{layout:?}: the wick leaves its bar"
                    );
                }
            }
        }
    }

    #[test]
    fn a_bar_is_filled_rather_than_outlined() {
        // An outline of four lines leaves the middle of a wide body empty, which
        // reads as a hollow box rather than a candle.
        let mut app = chart_app();
        app.zoom_chart(0.25);
        let bars = bar_columns(&app, 240, 40);
        let cells = frame_cells(&app, 240, 40);

        let tallest = bars
            .iter()
            .map(|x| {
                cells
                    .iter()
                    .filter(|(cx, _, _, colour)| {
                        cx == x
                            && matches!(colour, Some(colour)
                                if *colour == theme::POSITIVE || *colour == theme::NEGATIVE)
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);

        assert!(
            tallest >= 3,
            "a filled body paints more than its outline does: {tallest}"
        );
    }

    #[test]
    fn the_entry_price_is_marked_on_the_axis_like_the_live_price() {
        let app = chart_app();
        let axis = axis_column(&app);

        assert!(
            axis.contains("85,000.00"),
            "the entry price is labelled on the axis: {axis}"
        );
        let close = app.chart.last_close().expect("a close");
        assert!(
            axis.contains(&crate::ui::format::price(close)),
            "the live price is still labelled: {axis}"
        );
    }

    #[test]
    fn hiding_the_entry_line_also_removes_its_axis_marker() {
        let mut app = chart_app();
        app.toggle_entry_line();

        let axis = axis_column(&app);
        assert!(!axis.contains("85,000.00"), "got: {axis}");
    }

    #[test]
    fn a_contract_that_is_not_held_marks_no_entry_on_the_axis() {
        let mut app = sample_app();
        app.chart.reset("SOLUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "SOLUSDT".to_owned(),
            interval: Interval::M15,
            candles: sample_candles(),
        });

        let axis = axis_column(&app);
        assert!(!axis.contains("85,000.00"), "got: {axis}");
    }

    #[test]
    fn an_entry_outside_the_visible_range_is_not_marked() {
        // The marker must not claim a row for a price that is off-screen, or it
        // would point at the wrong level.
        let mut app = chart_app();
        let mut position = app.chart_position().cloned().expect("the held contract");
        position.entry_price = 1.0;
        app.set_positions(vec![position]);

        let axis = axis_column(&app);
        assert!(!axis.contains("1.00"), "got: {axis}");
    }

    #[test]
    fn axis_rows_follow_the_canvas_dot_grid() {
        // Ten rows are forty dots, so a price a couple of dots below the top is
        // still in the top row rather than in the one below it.
        assert_eq!(axis_row(100.0, 0.0, 100.0, 10), 0);
        assert_eq!(axis_row(97.5, 0.0, 100.0, 10), 0, "one dot down is row 0");
        assert_eq!(axis_row(90.0, 0.0, 100.0, 10), 1, "four dots down is row 1");
    }

    #[test]
    fn axis_rows_map_prices_high_to_low() {
        assert_eq!(
            axis_row(100.0, 0.0, 100.0, 10),
            0,
            "the high is the top row"
        );
        assert_eq!(
            axis_row(0.0, 0.0, 100.0, 10),
            9,
            "the low is the bottom row"
        );
        // Ten rows map 0..9, so the midpoint rounds to row 5 — which is the
        // same row the grid's middle label uses.
        assert_eq!(axis_row(50.0, 0.0, 100.0, 10), 5, "the middle lines up");
        assert_eq!(
            axis_row(1_000.0, 0.0, 100.0, 10),
            0,
            "a price above the range clamps instead of overflowing"
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

    /// Positions and colours of every cell the candles paint.
    fn candle_cells(
        app: &App,
    ) -> std::collections::BTreeMap<(u16, u16), Option<ratatui::style::Color>> {
        frame_cells(app, 120, 40)
            .into_iter()
            .filter(|(_, _, text, fg)| {
                let braille = text.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
                braille
                    && matches!(fg, Some(colour) if *colour == theme::POSITIVE || *colour == theme::NEGATIVE)
            })
            .map(|(x, y, _, fg)| ((x, y), fg))
            .collect()
    }

    /// One candle whose range spans the entry price, so a line must cross it.
    fn crossing_app() -> App {
        let mut app = sample_app();
        app.toggle_averages();
        app.chart.reset("BTCUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "BTCUSDT".to_owned(),
            interval: Interval::M15,
            candles: vec![crate::venue::Kline {
                open_time_ms: 1_790_726_400_000,
                open: 84_000.0,
                high: 86_000.0,
                low: 83_000.0,
                close: 85_500.0,
                volume: 1.0,
                close_time_ms: 1_790_727_299_999,
                closed: true,
            }],
        });
        app
    }

    /// Candles where MA7 rises into a tall candle, so the average crosses it.
    fn ma_crossing_app() -> App {
        let close_at = |index: usize| {
            if (6..9).contains(&index) {
                88_000.0
            } else {
                84_000.0
            }
        };
        let mut candles: Vec<crate::venue::Kline> = (0..10)
            .map(|index| crate::venue::Kline {
                open_time_ms: 1_790_726_400_000 + index as i64 * 900_000,
                open: close_at(index),
                high: close_at(index) + 100.0,
                low: close_at(index) - 100.0,
                close: close_at(index),
                volume: 1.0,
                close_time_ms: 1_790_726_400_000 + (index as i64 + 1) * 900_000 - 1,
                closed: true,
            })
            .collect();
        // A tall candle the average has to pass through.
        candles[9].high = 89_000.0;
        candles[9].low = 83_000.0;

        let mut app = sample_app();
        app.toggle_entry_line();
        app.chart.reset("BTCUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "BTCUSDT".to_owned(),
            interval: Interval::M15,
            candles,
        });
        app
    }

    #[test]
    fn the_entry_line_does_not_recolour_the_candles_it_crosses() {
        let mut app = crossing_app();
        app.toggle_entry_line();
        let painted_before = candle_cells(&app);
        assert!(!painted_before.is_empty(), "the candle is drawn");

        app.toggle_entry_line();
        let cells = frame_cells(&app, 120, 40);
        assert!(
            cells.iter().any(|(_, _, _, fg)| *fg == Some(theme::ENTRY)),
            "the entry line is drawn"
        );

        for (position, colour) in &painted_before {
            let after = cells
                .iter()
                .find(|(x, y, _, _)| (*x, *y) == *position)
                .map(|(_, _, _, fg)| *fg);
            assert_eq!(
                after,
                Some(*colour),
                "a cell painted by a candle changed colour at {position:?}"
            );
        }
    }

    #[test]
    fn the_moving_average_does_not_recolour_the_candles_it_crosses() {
        let mut app = ma_crossing_app();
        app.toggle_averages();
        let painted_before = candle_cells(&app);
        assert!(!painted_before.is_empty(), "the candles are drawn");

        app.toggle_averages();
        let cells = frame_cells(&app, 120, 40);
        assert!(
            cells
                .iter()
                .any(|(_, _, _, fg)| *fg == Some(AVERAGE_COLORS[0])),
            "the fast average is drawn"
        );

        for (position, colour) in &painted_before {
            let after = cells
                .iter()
                .find(|(x, y, _, _)| (*x, *y) == *position)
                .map(|(_, _, _, fg)| *fg);
            assert_eq!(
                after,
                Some(*colour),
                "a cell painted by a candle changed colour at {position:?}"
            );
        }
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
