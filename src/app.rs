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

use crate::accounts::AccountHandle;
use crate::chart::Viewport;
use crate::state::{App, Feed, SortColumn, Update};
use crate::ui;
use crate::venue::{Interval, StreamEvent, Venue};

/// How long the loop waits for a key before redrawing and draining updates.
const TICK: Duration = Duration::from_millis(200);
/// How long a freshly subscribed stream may stay silent before it is abandoned.
///
/// Some networks accept the WebSocket upgrade and then never deliver market
/// data (observed on Binance's USDⓈ-M futures stream host). Rather than showing
/// a frozen chart, fall back to polling.
const STREAM_SILENCE_LIMIT: Duration = Duration::from_secs(15);

/// How long a chart target must stay unchanged before its data is fetched.
///
/// Scrolling the positions table changes the target on every key press; without
/// this, a held-down `j` would fire a history request and a WebSocket per row.
const CHART_DEBOUNCE: Duration = Duration::from_millis(300);

/// Everything the event loop needs from the configuration.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Configured accounts, in configuration order.
    pub accounts: Vec<AccountHandle>,
    /// Index of the account to open.
    pub active: usize,
    /// Candle interval the chart opens with.
    pub interval: Interval,
    /// How often the feeds refresh, in milliseconds.
    pub refresh_interval_ms: u64,
    /// How many historical candles to load for the chart.
    pub chart_history: u32,
}

/// Run the interactive UI until the user quits.
///
/// Must be called from inside a Tokio runtime: the feed polling task is spawned
/// here. The terminal is restored on the way out, including while unwinding from
/// a panic.
pub fn run(options: RunOptions) -> io::Result<()> {
    let refresh_interval_ms = options.refresh_interval_ms.max(250);
    let active = options.active.min(options.accounts.len().saturating_sub(1));

    let first = options
        .accounts
        .get(active)
        .ok_or_else(|| io::Error::other("no accounts configured"))?;
    let mut app = App::new(
        first.label().to_owned(),
        first.venue(),
        options.interval,
        refresh_interval_ms,
    );

    let (updates_tx, mut updates_rx) = mpsc::channel(16);
    let mut feeds = Feeds::default();
    let mut active = active;
    let mut refresh_tx = feeds.start(first, &mut app, &options, &updates_tx);

    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    while !app.should_quit() {
        terminal.draw(|frame| ui::render(frame, &app))?;
        drain(&mut updates_rx, &mut app);
        feeds.sync(&mut app, &options, &updates_tx);

        // Switching accounts rebuilds every feed: the previous client's tasks
        // are aborted so nothing keeps polling the old account.
        if app.take_account_switch() && options.accounts.len() > 1 {
            active = (active + 1) % options.accounts.len();
            tracing::info!(
                account = options.accounts[active].name(),
                "switching account"
            );
            refresh_tx = feeds.start(&options.accounts[active], &mut app, &options, &updates_tx);
        }

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    handle_key(key, &mut app, &refresh_tx);
                    feeds.sync(&mut app, &options, &updates_tx);
                }
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

    if app.picker_is_open() {
        handle_picker_key(key, app);
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.request_quit(),
        KeyCode::Char('s') => app.open_picker(),
        KeyCode::Char('a') => app.request_account_switch(),
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') => app.select_first(),
        KeyCode::Char('G') => app.select_last(),
        KeyCode::Char(',') => app.cycle_sort(false),
        KeyCode::Char('.') => app.cycle_sort(true),
        KeyCode::Char('R') => app.reverse_sort(),
        KeyCode::Char(']') => app.change_interval(true),
        KeyCode::Char('[') => app.change_interval(false),
        KeyCode::Left | KeyCode::Char('h') => app.pan_chart(-(Viewport::PAN_STEP as isize)),
        KeyCode::Right | KeyCode::Char('l') => app.pan_chart(Viewport::PAN_STEP as isize),
        KeyCode::Char('+') | KeyCode::Char('=') => app.zoom_chart(1.25),
        KeyCode::Char('-') | KeyCode::Char('_') => app.zoom_chart(0.8),
        KeyCode::Char('f') => app.follow_chart(),
        KeyCode::Char('m') => app.toggle_averages(),
        KeyCode::Char('n') => app.toggle_size_units(),
        KeyCode::Char('p') => app.toggle_entry_line(),
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

/// Translate a key press while the symbol picker is open.
///
/// Everything printable goes into the filter, so the global shortcuts are
/// deliberately suspended: `q` types a `q` instead of quitting.
fn handle_picker_key(key: KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Esc => app.close_picker(),
        KeyCode::Enter => {
            app.picker_confirm();
        }
        KeyCode::Backspace => app.picker_backspace(),
        KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => app.picker_move(-1),
        KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => app.picker_move(1),
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.picker_push(character);
        }
        _ => {}
    }
}

/// Fetches the tradable-contract list the first time the picker needs it.
#[derive(Default)]
struct SymbolsFeed {
    loading: bool,
    last_attempt: Option<std::time::Instant>,
}

impl SymbolsFeed {
    /// How long to wait before retrying a failed contract-list fetch.
    const RETRY_AFTER: Duration = Duration::from_secs(15);

    fn sync(&mut self, app: &App, venue: &Arc<dyn Venue>, updates: &mpsc::Sender<Update>) {
        if !app.picker_needs_symbols() || self.loading {
            return;
        }
        // A failed attempt must not turn into a request loop.
        if let Some(attempt) = self.last_attempt
            && attempt.elapsed() < Self::RETRY_AFTER
        {
            return;
        }

        self.loading = true;
        self.last_attempt = Some(std::time::Instant::now());
        let venue = Arc::clone(venue);
        let updates = updates.clone();
        tokio::spawn(async move {
            let update = match venue.symbols().await {
                Ok(symbols) => Update::Symbols(symbols),
                Err(error) => Update::Failed {
                    feed: Feed::Symbols,
                    message: error.to_string(),
                },
            };
            let _ = updates.send(update).await;
        });
    }
}

/// Every background feed for the active account.
///
/// Account switching tears all of them down and builds them again, which is why
/// they live behind one owner rather than as loose tasks.
#[derive(Default)]
struct Feeds {
    venue: Option<Arc<dyn Venue>>,
    poller: Option<tokio::task::JoinHandle<()>>,
    marks: Option<tokio::task::JoinHandle<()>>,
    mark_symbols: Vec<String>,
    chart: ChartFeed,
    symbols: SymbolsFeed,
}

impl Feeds {
    /// Point every feed at a new account and return the channel used to request
    /// an immediate refresh.
    fn start(
        &mut self,
        handle: &AccountHandle,
        app: &mut App,
        options: &RunOptions,
        updates: &mpsc::Sender<Update>,
    ) -> mpsc::Sender<()> {
        self.stop();
        app.begin_account(handle.label().to_owned(), handle.venue());

        let (refresh_tx, refresh_rx) = mpsc::channel(1);

        match handle.connect() {
            Ok(venue) => {
                let venue: Arc<dyn Venue> = Arc::from(venue);
                let interval = Duration::from_millis(options.refresh_interval_ms.max(250));

                self.poller = Some(tokio::spawn(poll(
                    Arc::clone(&venue),
                    interval,
                    updates.clone(),
                    refresh_rx,
                )));
                self.venue = Some(Arc::clone(&venue));
                self.chart.sync(app, &venue, options, updates);
            }
            Err(error) => {
                // A broken account must not take the UI down: report it and let
                // the user switch to another one.
                tracing::warn!(%error, "account could not be opened");
                app.apply(Update::Failed {
                    feed: Feed::Positions,
                    message: error.to_string(),
                });
                app.apply(Update::Failed {
                    feed: Feed::Account,
                    message: error.to_string(),
                });
                app.apply(Update::Failed {
                    feed: Feed::Chart,
                    message: error.to_string(),
                });
            }
        }

        refresh_tx
    }

    /// Advance the feeds for the current state.
    fn sync(&mut self, app: &mut App, options: &RunOptions, updates: &mpsc::Sender<Update>) {
        let Some(venue) = self.venue.clone() else {
            return;
        };
        self.chart.sync(app, &venue, options, updates);
        self.symbols.sync(app, &venue, updates);
        self.sync_marks(app, &venue, updates);
    }

    /// Subscribe to mark prices for exactly the contracts the account holds.
    fn sync_marks(&mut self, app: &App, venue: &Arc<dyn Venue>, updates: &mpsc::Sender<Update>) {
        if !venue.supports_streaming() {
            return;
        }
        let symbols: Vec<String> = app
            .positions
            .iter()
            .map(|position| position.symbol.clone())
            .collect();
        if symbols == self.mark_symbols {
            return;
        }

        if let Some(task) = self.marks.take() {
            task.abort();
        }
        self.mark_symbols = symbols.clone();
        if symbols.is_empty() {
            return;
        }

        tracing::debug!(count = symbols.len(), "subscribing to mark prices");
        let venue = Arc::clone(venue);
        let updates = updates.clone();
        self.marks = Some(tokio::spawn(async move {
            let (marks_tx, mut marks_rx) = mpsc::unbounded_channel();
            let feeding = Arc::clone(&venue);
            let stream = tokio::spawn(async move {
                if let Err(error) = feeding.follow_marks(symbols, marks_tx).await {
                    tracing::warn!(%error, "mark-price stream stopped");
                }
            });

            while let Some(event) = marks_rx.recv().await {
                let StreamEvent::Data((symbol, price)) = event else {
                    continue;
                };
                if updates
                    .send(Update::Marks(vec![(symbol, price)]))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            stream.abort();
        }));
    }

    /// Stop every task.
    fn stop(&mut self) {
        if let Some(task) = self.poller.take() {
            task.abort();
        }
        if let Some(task) = self.marks.take() {
            task.abort();
        }
        self.chart = ChartFeed::default();
        self.symbols = SymbolsFeed::default();
        self.mark_symbols.clear();
        self.venue = None;
    }
}

impl Drop for Feeds {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owns the chart's data task, restarting it when the target settles on a new
/// symbol or interval.
#[derive(Default)]
struct ChartFeed {
    /// Target the running task is loading.
    running: Option<(String, Interval)>,
    /// Target that has been requested but has not settled yet.
    pending: Option<((String, Interval), std::time::Instant)>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ChartFeed {
    /// Start (or restart) the chart task when the target has been stable long
    /// enough to be worth a request.
    fn sync(
        &mut self,
        app: &mut App,
        venue: &Arc<dyn Venue>,
        options: &RunOptions,
        updates: &mpsc::Sender<Update>,
    ) {
        let Some(symbol) = app.effective_symbol().map(str::to_owned) else {
            return;
        };
        let desired = (symbol, app.chart.interval);

        if self.running.as_ref() == Some(&desired) {
            self.pending = None;
            return;
        }

        match &self.pending {
            Some((target, since)) if *target == desired => {
                if since.elapsed() < CHART_DEBOUNCE {
                    return; // still settling
                }
            }
            _ => {
                self.pending = Some((desired, std::time::Instant::now()));
                return;
            }
        }

        self.pending = None;
        if let Some(task) = self.task.take() {
            task.abort();
        }

        let (symbol, interval) = desired.clone();
        app.chart.reset(symbol.clone(), interval);
        self.running = Some(desired);
        self.task = Some(tokio::spawn(chart_feed(
            Arc::clone(venue),
            symbol,
            interval,
            options.chart_history,
            Duration::from_millis(options.refresh_interval_ms.max(250)),
            STREAM_SILENCE_LIMIT,
            updates.clone(),
        )));
    }
}

impl Drop for ChartFeed {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Load one target's history, then follow it live.
///
/// Venues that cannot push updates are polled instead, so the same code path
/// serves a live account and an offline fixture.
async fn chart_feed(
    venue: Arc<dyn Venue>,
    symbol: String,
    interval: Interval,
    history: u32,
    refresh: Duration,
    silence_limit: Duration,
    updates: mpsc::Sender<Update>,
) {
    match venue.klines(&symbol, interval, history).await {
        Ok(candles) => {
            let update = Update::History {
                symbol: symbol.clone(),
                interval,
                candles,
            };
            if updates.send(update).await.is_err() {
                return;
            }
        }
        Err(error) => {
            let update = Update::Failed {
                feed: Feed::Chart,
                message: error.to_string(),
            };
            if updates.send(update).await.is_err() {
                return;
            }
        }
    }

    if !venue.supports_streaming() {
        poll_klines(&venue, &symbol, interval, refresh, &updates).await;
        return;
    }

    let (klines_tx, mut klines_rx) = mpsc::unbounded_channel();
    let streaming = Arc::clone(&venue);
    let streamed_symbol = symbol.clone();
    let stream = tokio::spawn(async move {
        if let Err(error) = streaming
            .follow_klines(&streamed_symbol, interval, klines_tx)
            .await
        {
            tracing::warn!(%error, "kline stream stopped");
        }
    });

    let mut silence = tokio::time::interval(silence_limit);
    silence.tick().await; // the first tick fires immediately
    let mut delivered = false;

    loop {
        tokio::select! {
            event = klines_rx.recv() => {
                let Some(event) = event else { break };
                delivered = true;
                let update = match event {
                    StreamEvent::Data(kline) => Update::Kline(kline),
                    // Surfaced rather than swallowed: a socket that keeps
                    // dropping must be visible, not merely quiet.
                    StreamEvent::Disconnected(message) => Update::Failed {
                        feed: Feed::Chart,
                        message,
                    },
                };
                if updates.send(update).await.is_err() {
                    stream.abort();
                    return;
                }
            }
            _ = silence.tick() => {
                // Once data has flowed, silence is normal on a quiet contract;
                // only a stream that never delivered anything is abandoned.
                if !delivered {
                    tracing::warn!("market stream stayed silent; polling instead");
                    break;
                }
            }
        }
    }
    stream.abort();

    if !delivered {
        poll_klines(&venue, &symbol, interval, refresh, &updates).await;
    }
}

/// Follow the forming candle by polling, for venues without streams.
async fn poll_klines(
    venue: &Arc<dyn Venue>,
    symbol: &str,
    interval: Interval,
    refresh: Duration,
    updates: &mpsc::Sender<Update>,
) {
    loop {
        tokio::time::sleep(refresh).await;
        if updates.is_closed() {
            return;
        }
        match venue.klines(symbol, interval, 2).await {
            Ok(candles) => {
                for candle in candles {
                    if updates.send(Update::Kline(candle)).await.is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let update = Update::Failed {
                    feed: Feed::Chart,
                    message: error.to_string(),
                };
                if updates.send(update).await.is_err() {
                    return;
                }
            }
        }
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
    use std::sync::Arc;
    use std::time::Duration;

    use crate::venue::Venue;

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
        assert_eq!(app.chart.interval, Interval::H1);
        press(&mut app, KeyCode::Char('['), &refresh);
        assert_eq!(app.chart.interval, Interval::M15);
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
    fn s_opens_the_picker_and_redirects_the_keys() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();

        press(&mut app, KeyCode::Char('s'), &refresh);
        assert!(app.picker_is_open(), "s opens the picker");

        press(&mut app, KeyCode::Char('q'), &refresh);
        assert!(!app.should_quit(), "q is a filter character while picking");
        assert_eq!(app.picker_state().map(|p| p.query.as_str()), Some("Q"));

        press(&mut app, KeyCode::Backspace, &refresh);
        assert_eq!(app.picker_state().map(|p| p.query.as_str()), Some(""));

        press(&mut app, KeyCode::Esc, &refresh);
        assert!(!app.picker_is_open(), "escape closes it");
        assert_eq!(app.selected, 0, "the table selection is untouched");
    }

    #[test]
    fn picker_navigation_and_confirmation_work_from_the_keyboard() {
        use crate::state::Update;
        use crate::venue::Symbol;

        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();
        app.apply(Update::Symbols(vec![
            Symbol {
                name: "BTCUSDT".to_owned(),
                base_asset: "BTC".to_owned(),
                quote_asset: "USDT".to_owned(),
            },
            Symbol {
                name: "ETHUSDT".to_owned(),
                base_asset: "ETH".to_owned(),
                quote_asset: "USDT".to_owned(),
            },
        ]));

        press(&mut app, KeyCode::Char('s'), &refresh);
        press(&mut app, KeyCode::Char('e'), &refresh);
        assert_eq!(
            app.picker_selected_symbol().map(|s| s.name.as_str()),
            Some("ETHUSDT")
        );

        press(&mut app, KeyCode::Enter, &refresh);
        assert!(!app.picker_is_open(), "confirming closes the picker");
        assert_eq!(
            app.effective_symbol(),
            Some("ETHUSDT"),
            "chart target moves"
        );
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

    /// A venue whose stream connects and then never delivers anything, which is
    /// what Binance's futures stream host does from some networks.
    struct SilentStreamVenue;

    fn test_candle(index: i64) -> crate::venue::Kline {
        crate::venue::Kline {
            open_time_ms: 1_790_726_400_000 + index * 900_000,
            open: 100.0 + index as f64,
            high: 101.0 + index as f64,
            low: 99.0 + index as f64,
            close: 100.5 + index as f64,
            volume: 10.0,
            close_time_ms: 1_790_726_400_000 + (index + 1) * 900_000 - 1,
            closed: true,
        }
    }

    impl Venue for SilentStreamVenue {
        fn id(&self) -> VenueId {
            VenueId::BinanceFutures
        }

        fn sync(&self) -> crate::venue::VenueFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn symbols(&self) -> crate::venue::VenueFuture<'_, Vec<crate::venue::Symbol>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn positions(&self) -> crate::venue::VenueFuture<'_, Vec<crate::venue::Position>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn account(&self) -> crate::venue::VenueFuture<'_, crate::venue::AccountSnapshot> {
            Box::pin(async {
                Ok(crate::venue::AccountSnapshot {
                    balances: Vec::new(),
                    wallet_balance: 0.0,
                    equity: 0.0,
                    unrealized_pnl: 0.0,
                    available_balance: 0.0,
                    initial_margin: 0.0,
                    maintenance_margin: 0.0,
                })
            })
        }

        fn klines(
            &self,
            _symbol: &str,
            _interval: Interval,
            limit: u32,
        ) -> crate::venue::VenueFuture<'_, Vec<crate::venue::Kline>> {
            Box::pin(async move { Ok((0..i64::from(limit.min(2))).map(test_candle).collect()) })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn follow_klines(
            &self,
            _symbol: &str,
            _interval: Interval,
            _updates: crate::venue::UnboundedSender<crate::venue::StreamEvent<crate::venue::Kline>>,
        ) -> crate::venue::VenueFuture<'_, ()> {
            // Subscription accepted, then nothing, forever.
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn a_silent_stream_falls_back_to_polling() {
        let venue: Arc<dyn Venue> = Arc::new(SilentStreamVenue);
        let (updates, mut received) = mpsc::channel(8);
        let feed = tokio::spawn(super::chart_feed(
            venue,
            "BTCUSDT".to_owned(),
            Interval::M15,
            10,
            Duration::from_millis(30),
            Duration::from_millis(60),
            updates,
        ));

        let mut saw_history = false;
        let mut saw_polled_candle = false;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(update) = received.recv().await {
                match update {
                    crate::state::Update::History { .. } => saw_history = true,
                    crate::state::Update::Kline(_) if saw_history => {
                        saw_polled_candle = true;
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await;

        assert!(outcome.is_ok(), "the fallback must deliver candles");
        assert!(saw_history, "history is fetched before anything else");
        assert!(
            saw_polled_candle,
            "a silent stream must not leave the chart frozen"
        );
        feed.abort();
    }

    #[test]
    fn unrelated_keys_do_nothing() {
        let (refresh, _rx) = mpsc::channel(1);
        let mut app = app();
        let before = (app.selected, app.sort, app.chart.interval);

        for code in [KeyCode::Char('z'), KeyCode::Enter, KeyCode::F(5)] {
            press(&mut app, code, &refresh);
        }

        assert_eq!((app.selected, app.sort, app.chart.interval), before);
        assert!(!app.should_quit());
    }
}
