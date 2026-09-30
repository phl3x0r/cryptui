//! The interactive event loop: terminal lifetime, key handling, and the feed
//! polling task.

use std::io;
use std::panic;
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::state::{App, Feed, SortColumn, Update};
use crate::ui;
use crate::venue::{Interval, Venue, VenueId};

/// How long the loop waits for a key before redrawing and draining updates.
const TICK: Duration = Duration::from_millis(200);

/// Everything the event loop needs from the configuration.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Account label shown in the header.
    pub account_label: String,
    /// Venue the account talks to.
    pub venue: VenueId,
    /// Candle interval the chart opens with.
    pub interval: Interval,
    /// How often the feeds refresh, in milliseconds.
    pub refresh_interval_ms: u64,
}

/// Run the interactive UI until the user quits.
///
/// Must be called from inside a Tokio runtime: the feed polling task is spawned
/// here. The terminal is restored on the way out, including while unwinding from
/// a panic.
pub fn run(venue: Box<dyn Venue>, options: RunOptions) -> io::Result<()> {
    let venue: Arc<dyn Venue> = Arc::from(venue);
    let refresh_interval_ms = options.refresh_interval_ms.max(250);

    let mut app = App::new(
        options.account_label,
        options.venue,
        options.interval,
        refresh_interval_ms,
    );

    let (updates_tx, mut updates_rx) = mpsc::channel(16);
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    tokio::spawn(poll(
        Arc::clone(&venue),
        Duration::from_millis(refresh_interval_ms),
        updates_tx,
        refresh_rx,
    ));

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    while !app.should_quit() {
        terminal.draw(|frame| ui::render(frame, &app))?;
        drain(&mut updates_rx, &mut app);

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => handle_key(key, &mut app, &refresh_tx),
                // The next draw picks up the new size from the backend.
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }

    Ok(())
}

/// Apply everything the feeds delivered since the last frame.
fn drain(updates: &mut mpsc::Receiver<Update>, app: &mut App) {
    while let Ok(update) = updates.try_recv() {
        app.apply(update);
    }
}

/// Translate a key press into a state change.
fn handle_key(key: KeyEvent, app: &mut App, refresh: &mpsc::Sender<()>) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.request_quit();
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.request_quit(),
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') => app.select_first(),
        KeyCode::Char('G') => app.select_last(),
        KeyCode::Char(',') => app.cycle_sort(false),
        KeyCode::Char('.') => app.cycle_sort(true),
        KeyCode::Char('R') => app.reverse_sort(),
        KeyCode::Char(']') => app.change_interval(true),
        KeyCode::Char('[') => app.change_interval(false),
        KeyCode::Char('r') => {
            // A full queue already means a refresh is pending.
            let _ = refresh.try_send(());
        }
        KeyCode::Char(digit @ '1'..='7') => {
            if let Some(index) = digit.to_digit(10).map(|digit| digit as usize - 1)
                && let Some(column) = SortColumn::ALL.get(index)
            {
                app.sort_by_column(*column);
            }
        }
        _ => {}
    }
}

/// Refresh positions and balances on an interval, or on demand.
///
/// Errors are reported to the UI rather than ending the task, so a flaky network
/// leaves the last good data on screen with a visible warning.
async fn poll(
    venue: Arc<dyn Venue>,
    interval: Duration,
    updates: mpsc::Sender<Update>,
    mut refresh: mpsc::Receiver<()>,
) {
    loop {
        let positions = match venue.positions().await {
            Ok(positions) => Update::Positions(positions),
            Err(error) => Update::Failed {
                feed: Feed::Positions,
                message: error.to_string(),
            },
        };
        if updates.send(positions).await.is_err() {
            return; // the UI is gone
        }

        let account = match venue.account().await {
            Ok(snapshot) => Update::Account(Box::new(snapshot)),
            Err(error) => Update::Failed {
                feed: Feed::Account,
                message: error.to_string(),
            },
        };
        if updates.send(account).await.is_err() {
            return;
        }

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            requested = refresh.recv() => {
                if requested.is_none() {
                    return;
                }
                tracing::debug!("refreshing on request");
            }
        }
    }
}

/// Owns the terminal for the duration of a run and always gives it back.
struct TerminalGuard;

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        install_panic_hook();
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Leave raw mode and the alternate screen.
///
/// Errors are deliberately ignored: this also runs while unwinding from a panic,
/// where a failure must not replace the original message.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Restore the terminal before the default panic message is printed.
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use tokio::sync::mpsc;

    use crate::state::{App, SortColumn};
    use crate::venue::{Interval, PositionSide, VenueId};

    use super::handle_key;

    fn app() -> App {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.set_positions(vec![
            super::super::ui::tests::sample_position("AAAUSDT", PositionSide::Long, 9.0),
            super::super::ui::tests::sample_position("BBBUSDT", PositionSide::Long, 1.0),
            super::super::ui::tests::sample_position("CCCUSDT", PositionSide::Short, -5.0),
        ]);
        app
    }

    fn press(app: &mut App, code: KeyCode, refresh: &mpsc::Sender<()>) {
        handle_key(KeyEvent::new(code, KeyModifiers::NONE), app, refresh);
    }

    #[test]
    fn navigation_keys_move_the_selection() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char('j'), &refresh);
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Down, &refresh);
        assert_eq!(app.selected, 2);
        press(&mut app, KeyCode::Char('k'), &refresh);
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Up, &refresh);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn g_and_shift_g_jump_to_the_ends() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char('G'), &refresh);
        assert_eq!(app.selected, 2);
        press(&mut app, KeyCode::Char('g'), &refresh);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn number_keys_select_a_sort_column() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char('1'), &refresh);
        assert_eq!(app.sort.column, SortColumn::Symbol);
        assert!(app.sort.descending, "a new column keeps the direction");

        press(&mut app, KeyCode::Char('1'), &refresh);
        assert!(
            !app.sort.descending,
            "re-pressing the active column reverses it"
        );

        press(&mut app, KeyCode::Char('7'), &refresh);
        assert_eq!(app.sort.column, SortColumn::Pnl);
    }

    #[test]
    fn comma_and_dot_cycle_the_sort_column_and_shift_r_reverses_it() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char(','), &refresh);
        assert_eq!(app.sort.column, SortColumn::Margin, "PnL steps backwards");
        press(&mut app, KeyCode::Char('.'), &refresh);
        assert_eq!(app.sort.column, SortColumn::Pnl);

        let before = app.sort.descending;
        press(&mut app, KeyCode::Char('R'), &refresh);
        assert_eq!(app.sort.descending, !before);
    }

    #[test]
    fn brackets_step_the_interval() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char(']'), &refresh);
        assert_eq!(app.interval, Interval::H1);
        press(&mut app, KeyCode::Char('['), &refresh);
        assert_eq!(app.interval, Interval::M15);
    }

    #[test]
    fn r_requests_a_refresh_without_changing_the_screen() {
        let (refresh, mut rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char('r'), &refresh);
        assert_eq!(rx.try_recv(), Ok(()), "the poller is asked to refresh now");
        assert!(!app.should_quit());
        assert_eq!(app.selected, 0, "refresh does not move the selection");
    }

    #[test]
    fn quit_keys_stop_the_loop() {
        let (refresh, _rx) = mpsc::channel(1);

        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut app = app();
            press(&mut app, code, &refresh);
            assert!(app.should_quit(), "{code:?} should quit");
        }

        let mut app = app();
        handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut app,
            &refresh,
        );
        assert!(app.should_quit(), "ctrl-c should quit");
    }

    #[test]
    fn key_releases_are_ignored() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        handle_key(
            KeyEvent::new_with_kind(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ),
            &mut app,
            &refresh,
        );
        assert_eq!(app.selected, 0, "a key release is not a key press");
    }

    #[test]
    fn unrelated_keys_do_nothing() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();
        let before = (app.selected, app.sort, app.interval);

        for code in [KeyCode::Char('z'), KeyCode::Enter, KeyCode::F(5)] {
            press(&mut app, code, &refresh);
        }

        assert_eq!((app.selected, app.sort, app.interval), before);
        assert!(!app.should_quit());
    }
}
