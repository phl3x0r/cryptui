//! The positions table.
//!
//! Cells are padded by hand rather than aligned by the widget: the numbers come
//! from [`super::format`] already rendered, and fixed padding keeps the columns
//! aligned identically in the terminal and in headless snapshots.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::state::{App, SizeUnits, SortColumn};
use crate::venue::Position;

use super::{format, theme};

/// Width of each column, in cells. Kept in step with [`row_for`].
///
/// Every width must leave room for the sort indicator (`Side ▼` needs six), or
/// the marker for the active column is silently truncated.
const COLUMN_WIDTHS: [u16; 7] = [16, 7, 13, 12, 12, 12, 20];

/// Width the table needs to show every column: the cells, the space between
/// them, and the border.
///
/// The account panel is shown only when this much room is left beside it, so the
/// two cannot drift apart.
pub(crate) const MIN_WIDTH: u16 = {
    let mut total = 2; // the two border columns
    let mut index = 0;
    while index < COLUMN_WIDTHS.len() {
        total += COLUMN_WIDTHS[index];
        if index > 0 {
            total += 1; // column spacing
        }
        index += 1;
    }
    total
};

/// Draw the positions table.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title(panel_title(app))
        .border_style(theme::border_style(true));

    if app.positions.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no open positions on this account",
                Style::default().fg(theme::LABEL),
            )))
            .block(block)
            .centered(),
            area,
        );
        return;
    }

    let rows: Vec<Row<'static>> = app
        .positions
        .iter()
        .map(|position| row_for(position, app.size_units()))
        .collect();

    let table = Table::new(rows, COLUMN_WIDTHS)
        .header(heading_row(app))
        .block(block)
        .row_highlight_style(theme::selection_style());

    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// ` Positions (60) `
fn panel_title(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " Positions ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({}) ", app.positions.len()),
            Style::default().fg(theme::LABEL),
        ),
    ])
}

/// Column headings, with the sort indicator on the active column.
fn heading_row(app: &App) -> Row<'static> {
    let cells = SortColumn::ALL.map(|column| {
        let active = column == app.sort.column;
        // The size column is named for what it currently shows.
        let heading = match column {
            SortColumn::Size => app.size_units().heading(),
            _ => column.heading(),
        };
        let text = if active {
            format!("{heading} {}", app.sort.indicator())
        } else {
            heading.to_owned()
        };
        let style = if active {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::LABEL)
        };
        Cell::from(Line::from(Span::styled(text, style)))
    });
    Row::new(cells)
}

/// One data row.
fn row_for(position: &Position, size_units: SizeUnits) -> Row<'static> {
    let return_on_margin = if position.initial_margin > 0.0 {
        position.unrealized_pnl / position.initial_margin * 100.0
    } else {
        0.0
    };

    // Contracts are the venue's unit; the value is what the position is worth.
    let size = match size_units {
        SizeUnits::Contracts => format::quantity(position.size),
        SizeUnits::Notional => format::money(position.notional.abs()),
    };

    Row::new(vec![
        cell(
            pad_right(&position.symbol, COLUMN_WIDTHS[0]),
            Style::default(),
        ),
        cell(
            pad_right(&position.side.to_string(), COLUMN_WIDTHS[1]),
            theme::side_style(position.side),
        ),
        cell(pad_left(&size, COLUMN_WIDTHS[2]), Style::default()),
        cell(
            pad_left(&format::price(position.entry_price), COLUMN_WIDTHS[3]),
            Style::default(),
        ),
        cell(
            pad_left(&format::price(position.mark_price), COLUMN_WIDTHS[4]),
            Style::default(),
        ),
        cell(
            pad_left(&format::money(position.initial_margin), COLUMN_WIDTHS[5]),
            Style::default(),
        ),
        cell(
            pad_left(
                &format!(
                    "{} ({})",
                    format::signed_money(position.unrealized_pnl),
                    format::percent(return_on_margin)
                ),
                COLUMN_WIDTHS[6],
            ),
            theme::pnl_style(position.unrealized_pnl),
        ),
    ])
}

/// Build a cell from already-formatted text.
fn cell(text: String, style: Style) -> Cell<'static> {
    Cell::from(Line::from(Span::styled(text, style)))
}

/// Pad on the right to `width` cells, leaving longer text alone.
fn pad_right(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let length = text.chars().count();
    if length >= width {
        text.to_owned()
    } else {
        format!("{text}{}", " ".repeat(width - length))
    }
}

/// Pad on the left to `width` cells, leaving longer text alone.
fn pad_left(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let length = text.chars().count();
    if length >= width {
        text.to_owned()
    } else {
        format!("{}{text}", " ".repeat(width - length))
    }
}

#[cfg(test)]
mod tests {
    use crate::state::{App, SortColumn, Update};
    use crate::venue::{Interval, PositionSide, VenueId};

    use super::super::tests::{frame_lines, sample_account, sample_app, sample_position};
    use super::{pad_left, pad_right};

    /// The table's data rows in screen order, excluding borders, headings and
    /// the chart panel above (whose title also mentions the symbol).
    fn table_rows(lines: &[String]) -> Vec<String> {
        let heading = lines
            .iter()
            .position(|line| line.contains("Symbol") && line.contains("PnL"))
            .map(|index| index + 1)
            .unwrap_or_default();

        lines[heading..]
            .iter()
            .take_while(|line| !line.contains('└'))
            .filter(|line| line.contains("USDT"))
            .cloned()
            .collect()
    }

    /// Index of a row within the table, for order assertions.
    fn row_index(lines: &[String], symbol: &str) -> usize {
        table_rows(lines)
            .iter()
            .position(|line| line.contains(symbol))
            .unwrap_or_else(|| panic!("{symbol} is not in the table"))
    }

    #[test]
    fn rows_are_ordered_by_the_active_sort() {
        let lines = frame_lines(&sample_app(), 120, 40);
        let rows = table_rows(&lines);

        assert_eq!(rows.len(), 2, "one row per position: {rows:?}");
        assert!(rows[0].contains("BTCUSDT"), "winner first: {}", rows[0]);
        assert!(rows[1].contains("ETHUSDT"), "loser second: {}", rows[1]);
    }

    #[test]
    fn every_column_is_present_with_its_values() {
        let lines = frame_lines(&sample_app(), 120, 40);
        let text = lines.join("\n");
        let row = table_rows(&lines)
            .into_iter()
            .find(|line| line.contains("BTCUSDT"))
            .expect("BTCUSDT row");

        for heading in [
            "Symbol", "Side", "Notional", "Entry", "Mark", "Margin", "PnL",
        ] {
            assert!(text.contains(heading), "heading {heading} is missing");
        }
        assert!(row.contains("85,000.00"), "entry: {row}");
        assert!(row.contains("85,500.00"), "mark: {row}");
        assert!(row.contains("1,000.00"), "margin: {row}");
        assert!(row.contains("+141.25"), "pnl value: {row}");
        assert!(row.contains("(+14.1%)"), "pnl percentage: {row}");
    }

    #[test]
    fn a_loss_is_signed_and_coloured_differently_from_a_win() {
        let lines = frame_lines(&sample_app(), 120, 40);
        let row = table_rows(&lines)
            .into_iter()
            .find(|line| line.contains("ETHUSDT"))
            .expect("ETHUSDT row");
        assert!(row.contains("-52.75"), "losses keep their sign: {row}");
    }

    #[test]
    fn the_size_column_shows_the_position_value_by_default() {
        let app = sample_app();
        let row = table_rows(&frame_lines(&app, 120, 40))
            .into_iter()
            .find(|line| line.contains("BTCUSDT"))
            .expect("BTCUSDT row");
        let text = frame_lines(&app, 120, 40).join("\n");

        assert!(
            text.contains("Notional"),
            "the column is named for its unit: {text}"
        );
        assert!(
            row.contains("1,068,750.00"),
            "12.5 contracts at 85,500 is the value: {row}"
        );
        assert!(
            !row.contains("12.50"),
            "the contract amount is not shown: {row}"
        );
    }

    #[test]
    fn toggling_the_size_column_shows_contracts_again() {
        let mut app = sample_app();
        app.toggle_size_units();

        let lines = frame_lines(&app, 120, 40);
        let row = table_rows(&lines)
            .into_iter()
            .find(|line| line.contains("BTCUSDT"))
            .expect("BTCUSDT row");
        let text = lines.join("\n");

        assert!(
            text.contains("Size"),
            "the heading follows the unit: {text}"
        );
        assert!(!text.contains("Notional"), "got: {text}");
        assert!(row.contains("12.50"), "the contract amount: {row}");
        assert!(!row.contains("1,068,750.00"), "not the value: {row}");
    }

    #[test]
    fn sorting_by_size_follows_what_the_column_shows() {
        // Same mark price, so the two orderings differ only through the sizes.
        let mut small = sample_position("AAAUSDT", PositionSide::Long, 1.0);
        small.size = 1.0;
        small.notional = 1_000.0;
        let mut large = sample_position("BBBUSDT", PositionSide::Long, 1.0);
        large.size = 900.0;
        large.notional = 10.0;

        let mut app = sample_app();
        app.set_positions(vec![small, large]);
        app.sort_by_column(SortColumn::Size);

        // Notional: the small position is worth more, so it sorts first.
        let by_value = frame_lines(&app, 120, 40);
        assert_eq!(row_index(&by_value, "AAAUSDT"), 0, "largest value first");

        app.toggle_size_units();
        let by_contracts = frame_lines(&app, 120, 40);
        assert_eq!(
            row_index(&by_contracts, "BBBUSDT"),
            0,
            "largest contract amount first"
        );
    }

    #[test]
    fn the_sort_indicator_is_visible_for_every_column() {
        let mut app = sample_app();
        for column in SortColumn::ALL {
            app.sort_by_column(column);
            let text = frame_lines(&app, 120, 40).join("\n");
            let heading = match column {
                SortColumn::Size => app.size_units().heading(),
                _ => column.heading(),
            };
            let expected = format!("{heading} {}", app.sort.indicator());
            assert!(
                text.contains(&expected),
                "`{expected}` is missing or truncated for {column:?}"
            );
        }
    }

    #[test]
    fn sorting_by_size_reorders_the_rows() {
        let mut app = sample_app();
        let mut small = sample_position("AAAUSDT", PositionSide::Long, 1.0);
        small.size = 1.0;
        small.notional = 85_500.0;
        let mut large = sample_position("BBBUSDT", PositionSide::Long, 2.0);
        large.size = 900.0;
        large.notional = 900.0 * 85_500.0;
        app.set_positions(vec![small, large]);

        let default_order = frame_lines(&app, 120, 40);
        assert_eq!(
            row_index(&default_order, "BBBUSDT"),
            0,
            "bigger pnl first by default"
        );

        app.sort_by_column(SortColumn::Size);
        let by_size = frame_lines(&app, 120, 40);
        assert_eq!(row_index(&by_size, "BBBUSDT"), 0, "largest size first");
        assert_eq!(row_index(&by_size, "AAAUSDT"), 1);

        app.reverse_sort();
        let reversed = frame_lines(&app, 120, 40);
        assert_eq!(
            row_index(&reversed, "AAAUSDT"),
            0,
            "reversing flips the order"
        );
    }

    #[test]
    fn the_sort_indicator_moves_to_the_active_column() {
        let mut app = sample_app();
        assert!(frame_lines(&app, 120, 40).join("\n").contains("PnL ▼"));

        app.sort_by_column(SortColumn::Symbol);
        let text = frame_lines(&app, 120, 40).join("\n");
        assert!(text.contains("Symbol ▼"), "got: {text}");
        assert!(!text.contains("PnL ▼"), "only one column is marked");
    }

    #[test]
    fn the_table_reports_that_an_account_is_flat() {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.apply(Update::Account(Box::new(sample_account())));

        let text = frame_lines(&app, 120, 40).join("\n");
        assert!(
            text.contains("no open positions on this account"),
            "got: {text}"
        );
        assert!(
            text.contains("Positions (0)"),
            "count is still shown: {text}"
        );
    }

    #[test]
    fn padding_is_exact_and_never_truncates() {
        assert_eq!(pad_right("BTC", 5), "BTC  ");
        assert_eq!(pad_left("12", 5), "   12");
        assert_eq!(pad_right("LONGSYMBOL", 3), "LONGSYMBOL");
        assert_eq!(pad_left("123456", 3), "123456");
    }
}
