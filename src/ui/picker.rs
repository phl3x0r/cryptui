//! The symbol picker overlay.
//!
//! A filter line plus a scrolling contract list, drawn on top of the panels so
//! the chart and table stay visible underneath.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::state::App;

use super::theme;

/// Overlay width, as a percentage of the screen.
const WIDTH_PERCENT: u16 = 46;
/// Overlay height, as a percentage of the screen.
const HEIGHT_PERCENT: u16 = 60;
/// Narrowest useful overlay and list.
const MIN_WIDTH: u16 = 28;
const MIN_HEIGHT: u16 = 5;

/// Draw the picker, if it is open.
pub(crate) fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.picker_state() else {
        return;
    };

    let overlay = centered(area);
    // Without this the list would be drawn over whatever it covers.
    frame.render_widget(Clear, overlay);

    let matches = app.picker_matches();
    let block = Block::bordered()
        .title(Line::from(vec![
            Span::styled(
                " Symbol ",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} of {} ", matches.len(), app.symbols.len()),
                Style::default().fg(theme::LABEL),
            ),
        ]))
        .border_style(theme::border_style(true));

    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    if inner.height < 2 || inner.width == 0 {
        return;
    }

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter ", Style::default().fg(theme::LABEL)),
            Span::styled(
                picker.query.clone(),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("▏", Style::default().fg(theme::ACCENT)),
        ])),
        rows[0],
    );

    if matches.is_empty() {
        // Distinguish "nothing matched" from "the list never arrived".
        let (message, color) = match (&app.symbols_feed.last_error, app.symbols.is_empty()) {
            (Some(error), true) => (format!("could not load contracts: {error}"), theme::ERROR),
            _ => ("no contract matches".to_owned(), theme::WARNING),
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(color),
            )))
            .centered(),
            rows[1],
        );
        return;
    }

    let items = matches.iter().map(|symbol| {
        ListItem::new(Line::from(vec![
            Span::raw(format!("{:<16}", symbol.name)),
            Span::styled(
                format!("{}/{}", symbol.base_asset, symbol.quote_asset),
                Style::default().fg(theme::LABEL),
            ),
        ]))
    });

    let list = List::new(items)
        .highlight_style(theme::selection_style())
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list, rows[1], &mut state);
}

/// Per-centre the overlay on the screen, bounded by the screen itself.
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

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use crate::state::{App, Update};
    use crate::venue::Symbol;

    use super::super::tests::{frame_lines, sample_app};
    use super::centered;

    fn symbol(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            base_asset: name.trim_end_matches("USDT").to_owned(),
            quote_asset: "USDT".to_owned(),
        }
    }

    fn app_with_picker() -> App {
        let mut app = sample_app();
        app.apply(Update::Symbols(vec![
            symbol("BTCUSDT"),
            symbol("ETHUSDT"),
            symbol("SOLUSDT"),
            symbol("WBTCUSDT"),
        ]));
        app.open_picker();
        app
    }

    /// Only the cells the overlay itself covers.
    ///
    /// The chart and table stay visible around it, so a whole-frame assertion
    /// would pass or fail for reasons that have nothing to do with the picker.
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
    fn the_overlay_lists_contracts_and_counts_them() {
        let text = overlay_text(&app_with_picker(), 120, 40);

        assert!(text.contains("Symbol"), "overlay title: {text}");
        assert!(text.contains("4 of 4"), "match count: {text}");
        assert!(text.contains("filter"), "filter line: {text}");
        for expected in ["BTCUSDT", "ETHUSDT", "SOLUSDT", "WBTCUSDT"] {
            assert!(text.contains(expected), "{expected} missing from: {text}");
        }
    }

    #[test]
    fn typing_narrows_the_list_and_shows_the_query() {
        let mut app = app_with_picker();
        for character in "eth".chars() {
            app.picker_push(character);
        }

        let text = overlay_text(&app, 120, 40);
        assert!(text.contains("filter ETH"), "query is echoed: {text}");
        assert!(text.contains("1 of 4"), "count narrows: {text}");
        assert!(text.contains("ETHUSDT"), "the match is listed: {text}");
        assert!(
            !text.contains("BTCUSDT"),
            "non-matches leave the overlay: {text}"
        );
        assert!(
            !text.contains("WBTCUSDT"),
            "non-matches leave the overlay: {text}"
        );
    }

    #[test]
    fn prefixes_rank_above_mid_name_matches_in_the_overlay() {
        let mut app = app_with_picker();
        for character in "btc".chars() {
            app.picker_push(character);
        }

        let text = overlay_text(&app, 120, 40);
        let exact = text.find("BTCUSDT").expect("BTCUSDT listed");
        let mid = text.find("WBTCUSDT").expect("WBTCUSDT listed");
        assert!(exact < mid, "prefix match is listed first: {text}");
    }

    #[test]
    fn an_unmatched_query_says_so_instead_of_showing_an_empty_box() {
        let mut app = app_with_picker();
        app.picker_push('z');

        let text = overlay_text(&app, 120, 40);
        assert!(text.contains("no contract matches"), "got: {text}");
        assert!(text.contains("0 of 4"), "got: {text}");
    }

    #[test]
    fn the_overlay_covers_what_is_underneath_it() {
        let text = overlay_text(&app_with_picker(), 120, 40);
        assert!(
            !text.contains("MA7"),
            "the chart title does not bleed through: {text}"
        );
        assert!(
            !text.contains("Long") && !text.contains("Short"),
            "the positions table does not bleed through: {text}"
        );
    }

    #[test]
    fn a_failed_contract_fetch_is_reported_in_the_overlay() {
        let mut app = sample_app();
        app.open_picker();
        app.apply(Update::Failed {
            feed: crate::state::Feed::Symbols,
            message: "network error".to_owned(),
        });

        let text = overlay_text(&app, 120, 40);
        assert!(text.contains("could not load contracts"), "got: {text}");
        assert!(
            text.contains("network error"),
            "the reason is shown: {text}"
        );
    }

    #[test]
    fn a_closed_picker_draws_nothing() {
        let app = sample_app();
        let text = frame_lines(&app, 120, 40).join("\n");
        assert!(!text.contains("filter"), "no overlay when closed: {text}");
    }

    #[test]
    fn the_overlay_stays_inside_a_small_screen() {
        for (width, height) in [(30u16, 8u16), (20, 6), (24, 5)] {
            let app = app_with_picker();
            let lines = frame_lines(&app, width, height);
            assert_eq!(lines.len(), height as usize);

            let area = Rect::new(0, 0, width, height);
            let overlay = centered(area);
            assert!(
                overlay.right() <= area.right(),
                "width fits at {width}x{height}"
            );
            assert!(
                overlay.bottom() <= area.bottom(),
                "height fits at {width}x{height}"
            );
        }
    }
}
