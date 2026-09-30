//! Layout composition.
//!
//! The screen is one column: a two-line header, the chart, the positions table,
//! and a two-line footer. Panels for order entry and the order book are
//! deliberately absent until those features exist.

pub(crate) mod chart;
pub(crate) mod footer;
pub mod format;
pub(crate) mod header;
pub(crate) mod picker;
pub(crate) mod positions;
pub(crate) mod theme;

use std::io;

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::state::App;

/// Height of the header, in rows.
const HEADER_HEIGHT: u16 = 2;
/// Height of the footer, in rows: one metrics line and two hint lines.
const FOOTER_HEIGHT: u16 = 3;
/// Smallest useful chart height.
const MIN_CHART_HEIGHT: u16 = 6;
/// Bounds for the positions table height.
const POSITIONS_HEIGHT_RANGE: (u16, u16) = (5, 16);

/// Draw one frame.
pub fn render(frame: &mut Frame, app: &App) {
    let areas = split(frame.area());
    header::render(frame, areas.header, app);
    chart::render(frame, areas.chart, app);
    positions::render(frame, areas.positions, app);
    footer::render(frame, areas.footer, app);

    // Drawn last so it sits above the panels.
    picker::render(frame, frame.area(), app);
}

/// Regions of the screen.
struct Areas {
    header: Rect,
    chart: Rect,
    positions: Rect,
    footer: Rect,
}

/// Divide the screen, giving the chart whatever is left over.
fn split(area: Rect) -> Areas {
    let chrome = HEADER_HEIGHT + FOOTER_HEIGHT;
    let available = area.height.saturating_sub(chrome);

    // Aim for roughly the reference layout: a third of the screen for positions,
    // but never so much that the chart is squeezed out.
    let preferred = (available / 3).clamp(POSITIONS_HEIGHT_RANGE.0, POSITIONS_HEIGHT_RANGE.1);
    let positions_height = preferred.min(available.saturating_sub(MIN_CHART_HEIGHT));
    let chart_height = available.saturating_sub(positions_height);

    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(chart_height),
        Constraint::Length(positions_height),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .split(area);

    Areas {
        header: chunks[0],
        chart: chunks[1],
        positions: chunks[2],
        footer: chunks[3],
    }
}

/// Write a single frame to stdout as ANSI escape sequences.
///
/// Used by `--dump-frame` so the rendering can be inspected without an
/// interactive terminal: `cryptui --dump-frame > frame.ans && cat frame.ans`.
pub fn dump_frame(app: &App, width: u16, height: u16) -> io::Result<()> {
    use std::io::Write;

    let mut stdout = io::stdout();
    // Home the cursor and clear, so `cat`ing the dump redraws in place.
    stdout.write_all(b"\x1b[2J\x1b[H")?;

    let backend = CrosstermBackend::new(stdout);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.draw(|frame| render(frame, app))?;
    terminal.backend_mut().flush()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::state::App;
    use crate::venue::{
        AccountSnapshot, Balance, Interval, Kline, Position, PositionSide, VenueId,
    };

    use super::render;

    /// Render a frame and return it as plain text lines.
    pub(crate) fn frame_lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw succeeds");

        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    pub(crate) fn sample_position(symbol: &str, side: PositionSide, pnl: f64) -> Position {
        let size = 12.5;
        let mark_price = 85_500.0;
        let signed = match side {
            PositionSide::Long => size,
            PositionSide::Short => -size,
        };

        Position {
            symbol: symbol.to_owned(),
            side,
            size,
            entry_price: 85_000.0,
            mark_price,
            unrealized_pnl: pnl,
            initial_margin: 1_000.0,
            maintenance_margin: 50.0,
            // Kept consistent with size and mark price, as the venue reports it.
            notional: mark_price * signed,
            liquidation_price: Some(80_000.0),
        }
    }

    /// The rendered frame as `(x, y, symbol, foreground)` per cell.
    ///
    /// Layout snapshots compare text; this exists for the cases where the
    /// *colour* is the thing under test.
    pub(crate) fn frame_cells(
        app: &App,
        width: u16,
        height: u16,
    ) -> Vec<(u16, u16, String, Option<ratatui::style::Color>)> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw succeeds");

        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| {
                let cell = &buffer[(x, y)];
                (x, y, cell.symbol().to_owned(), cell.style().fg)
            })
            .collect()
    }

    /// Deterministic candles for chart snapshots: a slow wave plus a drift,
    /// starting at 2026-09-30 00:00 UTC with 15-minute steps.
    pub(crate) fn sample_candles() -> Vec<Kline> {
        const START_MS: i64 = 1_790_726_400_000;
        const STEP_MS: i64 = 900_000;
        const COUNT: i64 = 120;

        let close_at =
            |index: i64| 85_000.0 + (index as f64 * 0.35).sin() * 700.0 + index as f64 * 12.0;

        (0..COUNT)
            .map(|index| {
                let close = close_at(index);
                let open = if index == 0 {
                    close - 40.0
                } else {
                    close_at(index - 1)
                };
                Kline {
                    open_time_ms: START_MS + index * STEP_MS,
                    open,
                    high: open.max(close) + 120.0,
                    low: open.min(close) - 120.0,
                    close,
                    volume: 500.0 + (index % 7) as f64 * 90.0,
                    close_time_ms: START_MS + (index + 1) * STEP_MS - 1,
                    closed: index + 1 < COUNT,
                }
            })
            .collect()
    }

    pub(crate) fn sample_account() -> AccountSnapshot {
        AccountSnapshot {
            balances: vec![Balance {
                asset: "USDT".to_owned(),
                total: 4_041.70,
                available: 3_203.16,
            }],
            wallet_balance: 4_041.70,
            equity: 4_038.14,
            unrealized_pnl: -3.56,
            available_balance: 3_203.16,
            initial_margin: 834.58,
            maintenance_margin: 97.49,
        }
    }

    /// An app with two positions and account data, as the feeds would leave it.
    pub(crate) fn sample_app() -> App {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.set_positions(vec![
            sample_position("BTCUSDT", PositionSide::Long, 141.25),
            sample_position("ETHUSDT", PositionSide::Short, -52.75),
        ]);
        app.apply(crate::state::Update::Account(Box::new(sample_account())));
        app
    }

    #[test]
    fn header_shows_account_venue_and_feed_state() {
        let app = sample_app();
        let lines = frame_lines(&app, 120, 40);

        assert!(lines[0].contains("cryptui"), "line 1: {}", lines[0]);
        assert!(lines[0].contains("main"), "account label: {}", lines[0]);
        assert!(lines[0].contains("binance_futures"), "venue: {}", lines[0]);
        assert!(lines[0].contains("positions"), "feed state: {}", lines[0]);
        assert!(lines[1].contains("15m"), "interval: {}", lines[1]);
        assert!(lines[1].contains("2 open positions"), "count: {}", lines[1]);
    }

    #[test]
    fn positions_table_is_sorted_and_marks_the_selection() {
        let app = sample_app();
        let lines = frame_lines(&app, 120, 40);
        let text = lines.join("\n");

        assert!(text.contains("Symbol"), "column headings present");
        assert!(
            text.contains("PnL ▼"),
            "active sort column is marked: {text}"
        );

        let btc = lines.iter().position(|line| line.contains("BTCUSDT"));
        let eth = lines.iter().position(|line| line.contains("ETHUSDT"));
        assert!(btc < eth, "bigger winner sorts above the loser");
    }

    #[test]
    fn footer_shows_account_totals_and_key_hints() {
        let app = sample_app();
        let lines = frame_lines(&app, 120, 40);
        let footer = &lines[lines.len() - 3..];

        assert!(footer[0].contains("Equity"), "metrics line: {}", footer[0]);
        assert!(
            footer[0].contains("4,038.14"),
            "equity value: {}",
            footer[0]
        );
        assert!(footer[1].contains("quit"), "key hints: {}", footer[1]);
        assert!(footer[1].contains("sort"), "sort hint: {}", footer[1]);
    }

    #[test]
    fn empty_positions_render_an_explanation_instead_of_a_blank_table() {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::H1,
            3_000,
        );
        app.apply(crate::state::Update::Account(Box::new(sample_account())));

        let lines = frame_lines(&app, 120, 40);
        let text = lines.join("\n");
        assert!(text.contains("no open positions"), "got: {text}");
        assert!(text.contains("1h"), "interval still shown: {text}");
    }

    #[test]
    fn selection_moves_down_the_table() {
        let mut app = sample_app();
        let before = frame_lines(&app, 120, 40);
        let first_selected = before
            .iter()
            .position(|line| line.contains("BTCUSDT"))
            .expect("BTCUSDT row");

        app.move_selection(1);
        let after = frame_lines(&app, 120, 40);
        // Both rows are rendered either way; what changes is which one carries
        // the selection marker, which the plain-text extraction cannot show.
        assert!(
            after.iter().any(|line| line.contains("ETHUSDT")),
            "the other row is still rendered"
        );
        assert!(first_selected < after.len());
        assert_eq!(
            app.selected_position().map(|p| p.symbol.as_str()),
            Some("ETHUSDT")
        );
    }

    #[test]
    fn a_degraded_feed_is_visible_in_the_header() {
        let mut app = sample_app();
        app.apply(crate::state::Update::Failed {
            feed: crate::state::Feed::Positions,
            message: "network error talking to binance_futures".to_owned(),
        });

        let lines = frame_lines(&app, 120, 40);
        assert!(
            lines[0].contains("error"),
            "feed problem is surfaced: {}",
            lines[0]
        );
    }

    #[test]
    fn a_short_terminal_still_renders_without_panicking() {
        for height in 8..=24u16 {
            for width in 40..=100u16 {
                let app = sample_app();
                let lines = frame_lines(&app, width, height);
                assert_eq!(lines.len(), height as usize);
            }
        }
    }
}
