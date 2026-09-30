//! Application state: everything the UI renders from, and nothing else.
//!
//! This module deliberately has no dependency on the terminal widgets, so the
//! ordering and selection rules can be tested without a terminal.

use std::cmp::Ordering;

use crate::auth::now_ms;
use crate::chart::{MOVING_AVERAGE_WINDOWS, Viewport, moving_average};
use crate::venue::{AccountSnapshot, Interval, Kline, Position, PositionSide, VenueId};

/// A column of the positions table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// Contract name.
    Symbol,
    /// Long or short.
    Side,
    /// Contract quantity.
    Size,
    /// Average entry price.
    Entry,
    /// Current mark price.
    Mark,
    /// Committed margin.
    Margin,
    /// Unrealized profit and loss.
    Pnl,
}

impl SortColumn {
    /// Every column, in display order.
    pub const ALL: [Self; 7] = [
        Self::Symbol,
        Self::Side,
        Self::Size,
        Self::Entry,
        Self::Mark,
        Self::Margin,
        Self::Pnl,
    ];

    /// Heading shown above the column.
    pub fn heading(self) -> &'static str {
        match self {
            Self::Symbol => "Symbol",
            Self::Side => "Side",
            Self::Size => "Size",
            Self::Entry => "Entry",
            Self::Mark => "Mark",
            Self::Margin => "Margin",
            Self::Pnl => "PnL",
        }
    }

    /// Number key that selects this column, as `1`…`7`.
    pub fn shortcut(self) -> char {
        char::from(b'1' + self.position() as u8)
    }

    /// Index of this column in [`SortColumn::ALL`].
    pub fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or_default()
    }

    /// The column after this one, wrapping around.
    pub fn next(self) -> Self {
        Self::ALL[(self.position() + 1) % Self::ALL.len()]
    }

    /// The column before this one, wrapping around.
    pub fn previous(self) -> Self {
        Self::ALL[(self.position() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Current ordering of the positions table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    /// Column the table is ordered by.
    pub column: SortColumn,
    /// Whether the order is descending.
    pub descending: bool,
}

impl Sort {
    /// Default ordering: largest unrealized profit first, the question the
    /// table exists to answer.
    pub fn by_pnl_descending() -> Self {
        Self {
            column: SortColumn::Pnl,
            descending: true,
        }
    }

    /// Move to the next column, keeping the direction.
    pub fn next_column(&mut self) {
        self.column = self.column.next();
    }

    /// Move to the previous column, keeping the direction.
    pub fn previous_column(&mut self) {
        self.column = self.column.previous();
    }

    /// Sort by `column`; selecting the active column again flips the direction.
    pub fn select_column(&mut self, column: SortColumn) {
        if self.column == column {
            self.descending = !self.descending;
        } else {
            self.column = column;
        }
    }

    /// Flip ascending/descending.
    pub fn reverse(&mut self) {
        self.descending = !self.descending;
    }

    /// Arrow shown next to the active column heading.
    pub fn indicator(self) -> &'static str {
        if self.descending { "▼" } else { "▲" }
    }
}

/// Which feed an update belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// Positions.
    Positions,
    /// Balances and account totals.
    Account,
    /// Candles for the focused symbol.
    Chart,
}

impl Feed {
    /// Name used in status lines and error messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Positions => "positions",
            Self::Account => "account",
            Self::Chart => "chart",
        }
    }
}

/// Health of one data feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedStatus {
    /// When the feed last delivered data, in milliseconds since the epoch.
    pub last_success_ms: Option<i64>,
    /// Most recent failure, cleared by the next success.
    pub last_error: Option<String>,
}

impl FeedStatus {
    /// Record a successful fetch.
    pub fn mark_success(&mut self, now_ms: i64) {
        self.last_success_ms = Some(now_ms);
        self.last_error = None;
    }

    /// Record a failed fetch, keeping the previous success time.
    pub fn mark_failure(&mut self, message: String) {
        self.last_error = Some(message);
    }

    /// Age of the last successful fetch, if there was one.
    pub fn age_ms(&self, now_ms: i64) -> Option<i64> {
        self.last_success_ms.map(|then| (now_ms - then).max(0))
    }

    /// Whether the feed has failed, or has not delivered within `tolerance_ms`.
    pub fn is_stale(&self, now_ms: i64, tolerance_ms: i64) -> bool {
        if self.last_error.is_some() {
            return true;
        }
        match self.age_ms(now_ms) {
            Some(age) => age > tolerance_ms,
            None => true,
        }
    }

    /// Short human-readable state, for example `live 2s` or `stale 41s`.
    pub fn summary(&self, now_ms: i64, tolerance_ms: i64) -> String {
        if let Some(error) = &self.last_error {
            return format!("error: {error}");
        }
        match self.age_ms(now_ms) {
            Some(age) if age > tolerance_ms => format!("stale {}s", age / 1_000),
            Some(age) => format!("live {}s", age / 1_000),
            None => "loading".to_owned(),
        }
    }
}

/// Something the event loop hands to the UI.
#[derive(Debug, Clone)]
pub enum Update {
    /// A fresh set of open positions.
    Positions(Vec<Position>),
    /// Fresh balances and account totals.
    Account(Box<AccountSnapshot>),
    /// A full candle history for the chart.
    ///
    /// Carries its target so a slow response for a symbol the user has already
    /// switched away from can be discarded.
    History {
        /// Symbol the history belongs to.
        symbol: String,
        /// Interval the history belongs to.
        interval: Interval,
        /// Oldest candle first.
        candles: Vec<Kline>,
    },
    /// One candle of the live series: either the forming candle or a new one.
    Kline(Kline),
    /// A feed failed; the previous data stays on screen.
    Failed {
        /// Which feed failed.
        feed: Feed,
        /// What went wrong.
        message: String,
    },
}

/// The candle chart: what it shows, and where its window sits.
///
/// The chart owns its own target (symbol and interval) so a reload triggered by
/// a key press is a state change the event loop can observe, not a side effect
/// hidden inside the key handler.
pub struct Chart {
    pub(crate) symbol: Option<String>,
    pub(crate) interval: Interval,
    pub(crate) candles: Vec<Kline>,
    pub(crate) averages: [Vec<Option<f64>>; MOVING_AVERAGE_WINDOWS.len()],
    pub(crate) viewport: Viewport,
    pub(crate) follow: bool,
    pub(crate) feed: FeedStatus,
}

impl Chart {
    /// A chart with no data, waiting for a symbol.
    pub fn new(interval: Interval) -> Self {
        Self {
            symbol: None,
            interval,
            candles: Vec::new(),
            averages: Default::default(),
            viewport: Viewport::new(Viewport::DEFAULT_VISIBLE),
            follow: true,
            feed: FeedStatus::default(),
        }
    }

    /// Whether the chart is already pointed at `symbol` with `interval`.
    pub fn is_target(&self, symbol: &str) -> bool {
        self.symbol.as_deref() == Some(symbol)
    }

    /// Whether data is still missing for the current target.
    pub fn needs_load(&self) -> bool {
        self.symbol.is_some() && self.candles.is_empty()
    }

    /// Point the chart at a new target and drop the old data.
    pub fn reset(&mut self, symbol: String, interval: Interval) {
        self.symbol = Some(symbol);
        self.interval = interval;
        self.candles.clear();
        self.averages = Default::default();
        self.follow = true;
        self.viewport = Viewport::new(self.viewport.visible());
        self.feed = FeedStatus::default();
    }

    /// Replace the candle history.
    pub fn set_history(&mut self, candles: Vec<Kline>) {
        self.candles = candles;
        self.recompute_averages();
        if self.follow {
            self.viewport.pin_to_end(self.candles.len());
        }
    }

    /// Apply one live candle: update the forming candle, or append a new one.
    ///
    /// Out-of-order candles (a stream that reconnects and replays) are ignored
    /// rather than corrupting the series.
    pub fn upsert(&mut self, kline: Kline) {
        match self.candles.last_mut() {
            Some(last) if last.open_time_ms == kline.open_time_ms => *last = kline,
            Some(last) if kline.open_time_ms > last.open_time_ms => self.candles.push(kline),
            Some(_) => return,
            None => self.candles.push(kline),
        }

        self.recompute_averages();
        if self.follow {
            self.viewport.pin_to_end(self.candles.len());
        }
    }

    /// Recompute every moving average over the full series.
    ///
    /// The whole series is recomputed rather than patched because the windows
    /// reach far back: one live candle changes the value at every later index
    /// that is still inside the window.
    fn recompute_averages(&mut self) {
        self.averages = MOVING_AVERAGE_WINDOWS.map(|window| moving_average(&self.candles, window));
    }

    /// The candles currently on screen.
    pub fn visible(&self) -> &[Kline] {
        &self.candles[self.viewport.range(self.candles.len())]
    }

    /// Moving-average values for the visible candles, aligned with [`Self::visible`].
    pub fn visible_average(&self, index: usize) -> &[Option<f64>] {
        let range = self.viewport.range(self.candles.len());
        self.averages
            .get(index)
            .map_or(&[][..], |values| &values[range])
    }

    /// Price range of the visible candles, including the moving averages.
    pub fn price_bounds(&self) -> Option<(f64, f64)> {
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for candle in self.visible() {
            low = low.min(candle.low);
            high = high.max(candle.high);
        }
        for index in 0..MOVING_AVERAGE_WINDOWS.len() {
            for value in self.visible_average(index).iter().flatten() {
                low = low.min(*value);
                high = high.max(*value);
            }
        }
        (low <= high).then_some((low, high))
    }

    /// Largest visible volume, used to scale the volume pane.
    pub fn volume_max(&self) -> f64 {
        self.visible()
            .iter()
            .map(|candle| candle.volume)
            .fold(0.0, f64::max)
    }

    /// Close of the newest candle, live or not.
    pub fn last_close(&self) -> Option<f64> {
        self.candles.last().map(|candle| candle.close)
    }

    /// Move the window, leaving follow mode when the user steps into history.
    pub fn pan(&mut self, delta: isize) {
        self.viewport.pan(delta, self.candles.len());
        self.follow = self.viewport.is_at_end(self.candles.len());
    }

    /// Widen or narrow the window.
    pub fn zoom(&mut self, factor: f64) {
        self.viewport.zoom(factor);
        self.follow = self.viewport.is_at_end(self.candles.len());
    }

    /// Jump back to the newest candle and follow it again.
    pub fn follow_end(&mut self) {
        self.follow = true;
        self.viewport.pin_to_end(self.candles.len());
    }

    /// How long the chart tolerates silence before it counts as stale.
    ///
    /// A 15-minute candle on a quiet contract legitimately updates rarely, so
    /// this follows the interval rather than the REST poll rate.
    pub fn stale_after_ms(&self, base_ms: i64) -> i64 {
        base_ms
            .max(60_000)
            .max(self.interval.duration_ms().saturating_mul(2))
    }

    /// Whether the window is pinned to the newest candle.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Values of every moving average at the newest candle, for the legend.
    pub fn latest_averages(&self) -> [Option<f64>; MOVING_AVERAGE_WINDOWS.len()] {
        let mut latest = [None; MOVING_AVERAGE_WINDOWS.len()];
        for (index, values) in self.averages.iter().enumerate() {
            latest[index] = values.last().copied().flatten();
        }
        latest
    }
}

/// All state the UI renders from.
pub struct App {
    pub(crate) account_label: String,
    pub(crate) venue: VenueId,
    pub(crate) chart: Chart,
    /// Symbol chosen from the picker, which overrides the table selection.
    pub(crate) chart_override: Option<String>,
    pub(crate) positions: Vec<Position>,
    pub(crate) sort: Sort,
    pub(crate) selected: usize,
    pub(crate) account: Option<AccountSnapshot>,
    pub(crate) positions_feed: FeedStatus,
    pub(crate) account_feed: FeedStatus,
    pub(crate) stale_after_ms: i64,
    pub(crate) quit: bool,
}

impl App {
    /// Start with no data; feeds fill the state in as they arrive.
    ///
    /// `refresh_interval_ms` also decides when a feed counts as stale: after
    /// four missed refreshes, so a single slow response raises no alarm.
    pub fn new(
        account_label: String,
        venue: VenueId,
        interval: Interval,
        refresh_interval_ms: u64,
    ) -> Self {
        let stale_after_ms = refresh_interval_ms.saturating_mul(4).min(i64::MAX as u64) as i64;

        Self {
            account_label,
            venue,
            chart: Chart::new(interval),
            chart_override: None,
            positions: Vec::new(),
            sort: Sort::by_pnl_descending(),
            selected: 0,
            account: None,
            positions_feed: FeedStatus::default(),
            account_feed: FeedStatus::default(),
            stale_after_ms,
            quit: false,
        }
    }

    /// Apply one update from a feed.
    pub fn apply(&mut self, update: Update) {
        let now = now_ms();
        match update {
            Update::Positions(positions) => {
                self.set_positions(positions);
                self.positions_feed.mark_success(now);
            }
            Update::Account(snapshot) => {
                self.account = Some(*snapshot);
                self.account_feed.mark_success(now);
            }
            Update::History {
                symbol,
                interval,
                candles,
            } => {
                // Drop a response for a target the user has moved away from.
                if self.chart.symbol.as_deref() != Some(symbol.as_str())
                    || self.chart.interval != interval
                {
                    tracing::debug!(%symbol, %interval, "discarding stale candle history");
                    return;
                }
                self.chart.set_history(candles);
                self.chart.feed.mark_success(now);
            }
            Update::Kline(kline) => {
                self.chart.upsert(kline);
                self.chart.feed.mark_success(now);
            }
            Update::Failed { feed, message } => {
                let status = match feed {
                    Feed::Positions => &mut self.positions_feed,
                    Feed::Account => &mut self.account_feed,
                    Feed::Chart => &mut self.chart.feed,
                };
                status.mark_failure(message);
            }
        }
    }

    /// Replace the positions, re-apply the ordering, and keep the selected
    /// contract selected if it is still open.
    pub fn set_positions(&mut self, positions: Vec<Position>) {
        // Read the remembered symbol from the old data: the new vector may be
        // shorter, or ordered differently.
        let remembered = self
            .selected_position()
            .map(|position| position.symbol.clone());
        self.positions = positions;
        self.resort(remembered);
    }

    /// Re-apply the current ordering, restoring `remembered` as the selection
    /// when that contract is still present.
    ///
    /// Ties break on the symbol so the table never reshuffles between refreshes.
    fn resort(&mut self, remembered: Option<String>) {
        let column = self.sort.column;
        let descending = self.sort.descending;
        self.positions.sort_by(|left, right| {
            let primary = compare(left, right, column);
            let primary = if descending {
                primary.reverse()
            } else {
                primary
            };
            primary.then_with(|| left.symbol.cmp(&right.symbol))
        });

        if let Some(symbol) = remembered
            && let Some(index) = self
                .positions
                .iter()
                .position(|position| position.symbol == symbol)
        {
            self.selected = index;
        }
        self.clamp_selection();
    }

    /// Re-apply the ordering while keeping the selected *contract* selected.
    ///
    /// Without this, re-sorting would keep the row index and silently move the
    /// selection onto a different contract.
    fn resort_keeping_selection(&mut self) {
        let remembered = self
            .selected_position()
            .map(|position| position.symbol.clone());
        self.resort(remembered);
    }

    /// Move the selection by `delta` rows, stopping at either end.
    ///
    /// Moving the selection returns the chart to the table: an explicit pick
    /// from the symbol picker only lasts until the user navigates again.
    pub fn move_selection(&mut self, delta: isize) {
        self.chart_override = None;
        if self.positions.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.positions.len() - 1;
        let current = self.selected.min(last) as isize;
        self.selected = current.saturating_add(delta).clamp(0, last as isize) as usize;
    }

    /// Select the first row.
    pub fn select_first(&mut self) {
        self.chart_override = None;
        self.selected = 0;
    }

    /// Select the last row.
    pub fn select_last(&mut self) {
        self.chart_override = None;
        self.selected = self.positions.len().saturating_sub(1);
    }

    /// Cycle the sort column.
    pub fn cycle_sort(&mut self, forward: bool) {
        if forward {
            self.sort.next_column();
        } else {
            self.sort.previous_column();
        }
        self.resort_keeping_selection();
    }

    /// Sort by a specific column; pressing the same shortcut again flips direction.
    pub fn sort_by_column(&mut self, column: SortColumn) {
        self.sort.select_column(column);
        self.resort_keeping_selection();
    }

    /// Flip the current ordering.
    pub fn reverse_sort(&mut self) {
        self.sort.reverse();
        self.resort_keeping_selection();
    }

    /// Step the candle interval up or down.
    ///
    /// The chart drops its candles, so the event loop notices that the target
    /// changed and reloads it.
    pub fn change_interval(&mut self, forward: bool) {
        let interval = if forward {
            self.chart.interval.next()
        } else {
            self.chart.interval.previous()
        };
        self.chart.interval = interval;
        self.chart.candles.clear();
        self.chart.averages = Default::default();
        self.chart.follow = true;
        self.chart.feed = FeedStatus::default();
    }

    /// Ask the event loop to exit.
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    /// Whether the event loop should exit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// The row the user has selected, if any.
    pub fn selected_position(&self) -> Option<&Position> {
        self.positions.get(self.selected)
    }

    /// Symbol the chart should follow: an explicit pick wins over the table.
    pub fn effective_symbol(&self) -> Option<&str> {
        self.chart_override.as_deref().or_else(|| {
            self.selected_position()
                .or_else(|| self.positions.first())
                .map(|position| position.symbol.as_str())
        })
    }

    /// Symbol the chart currently holds data for.
    pub fn chart_symbol(&self) -> Option<&str> {
        self.chart.symbol.as_deref()
    }

    /// Point the chart at a symbol that need not have an open position.
    pub fn set_chart_symbol(&mut self, symbol: String) {
        self.chart_override = Some(symbol);
    }

    /// Interval the chart is currently showing.
    pub fn chart_interval(&self) -> Interval {
        self.chart.interval
    }

    /// Point the chart at `symbol` at the current interval and drop its data, so
    /// the next history update for that target is accepted.
    ///
    /// Used by the headless `--dump-frame` path, which has no event loop to do
    /// this on its behalf.
    pub fn focus_chart(&mut self, symbol: String) {
        let interval = self.chart.interval;
        self.chart_override = Some(symbol.clone());
        self.chart.reset(symbol, interval);
    }

    /// Pan the chart window, leaving follow mode when stepping into history.
    pub fn pan_chart(&mut self, delta: isize) {
        self.chart.pan(delta);
    }

    /// Zoom the chart window.
    pub fn zoom_chart(&mut self, factor: f64) {
        self.chart.zoom(factor);
    }

    /// Return the chart to the newest candle.
    pub fn follow_chart(&mut self) {
        self.chart.follow_end();
    }

    /// Keep the selection inside the table.
    fn clamp_selection(&mut self) {
        if self.positions.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.positions.len() - 1);
        }
    }
}

/// Order two positions by one column.
fn compare(left: &Position, right: &Position, column: SortColumn) -> Ordering {
    match column {
        SortColumn::Symbol => left.symbol.cmp(&right.symbol),
        SortColumn::Side => side_rank(left.side).cmp(&side_rank(right.side)),
        SortColumn::Size => left.size.total_cmp(&right.size),
        SortColumn::Entry => left.entry_price.total_cmp(&right.entry_price),
        SortColumn::Mark => left.mark_price.total_cmp(&right.mark_price),
        SortColumn::Margin => left.initial_margin.total_cmp(&right.initial_margin),
        SortColumn::Pnl => left.unrealized_pnl.total_cmp(&right.unrealized_pnl),
    }
}

/// Rank a side so longs and shorts order predictably.
fn side_rank(side: PositionSide) -> u8 {
    match side {
        PositionSide::Long => 0,
        PositionSide::Short => 1,
    }
}

#[cfg(test)]
mod tests {
    use crate::venue::{AccountSnapshot, Interval, Position, PositionSide, VenueId};

    use super::{App, Chart, Feed, FeedStatus, Sort, SortColumn, Update};

    fn position(symbol: &str, pnl: f64, size: f64) -> Position {
        Position {
            symbol: symbol.to_owned(),
            side: if pnl >= 0.0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            },
            size,
            entry_price: 100.0,
            mark_price: 101.0,
            unrealized_pnl: pnl,
            initial_margin: 10.0,
            maintenance_margin: 0.5,
            notional: 1000.0,
            liquidation_price: None,
        }
    }

    fn app_with(positions: Vec<Position>) -> App {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.set_positions(positions);
        app
    }

    #[test]
    fn defaults_to_biggest_winner_first() {
        let app = app_with(vec![
            position("AAAUSDT", -5.0, 1.0),
            position("BBBUSDT", 12.0, 2.0),
            position("CCCUSDT", 3.0, 3.0),
        ]);

        let symbols: Vec<&str> = app.positions.iter().map(|p| p.symbol.as_str()).collect();
        assert_eq!(symbols, ["BBBUSDT", "CCCUSDT", "AAAUSDT"]);
    }

    #[test]
    fn sorting_by_a_column_keeps_the_selected_contract_selected() {
        let mut app = app_with(vec![
            position("AAAUSDT", 1.0, 3.0),
            position("BBBUSDT", 2.0, 2.0),
            position("CCCUSDT", 3.0, 1.0),
        ]);
        app.move_selection(2);
        assert_eq!(
            app.selected_position().map(|p| p.symbol.as_str()),
            Some("AAAUSDT")
        );

        app.sort_by_column(SortColumn::Size);
        assert_eq!(
            app.selected_position().map(|p| p.symbol.as_str()),
            Some("AAAUSDT"),
            "the selected contract stays selected"
        );
        assert_eq!(app.selected, 0, "AAAUSDT holds the largest size");
        assert!(app.sort.descending, "size sorts largest first by default");
    }

    #[test]
    fn pressing_the_same_column_again_flips_direction() {
        let mut app = app_with(vec![position("AAAUSDT", 1.0, 1.0)]);
        assert_eq!(app.sort.column, SortColumn::Pnl);

        app.sort_by_column(SortColumn::Pnl);
        assert!(!app.sort.descending, "second press reverses the order");
        assert_eq!(app.sort.indicator(), "▲");

        app.sort_by_column(SortColumn::Symbol);
        assert_eq!(app.sort.column, SortColumn::Symbol);
        assert!(!app.sort.descending, "a new column keeps the direction");
    }

    #[test]
    fn cycling_columns_wraps_in_both_directions() {
        let mut sort = Sort::by_pnl_descending();
        sort.next_column();
        assert_eq!(sort.column, SortColumn::Symbol, "PnL is last, so it wraps");
        sort.previous_column();
        assert_eq!(sort.column, SortColumn::Pnl);
    }

    #[test]
    fn selection_stops_at_both_ends() {
        let mut app = app_with(vec![
            position("AAAUSDT", 1.0, 1.0),
            position("BBBUSDT", 2.0, 1.0),
        ]);

        app.move_selection(-5);
        assert_eq!(app.selected, 0, "cannot move above the first row");
        app.move_selection(99);
        assert_eq!(app.selected, 1, "cannot move past the last row");
        app.select_last();
        assert_eq!(app.selected, 1);
        app.select_first();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selection_clamps_when_positions_disappear() {
        let mut app = app_with(vec![
            position("AAAUSDT", 1.0, 1.0),
            position("BBBUSDT", 2.0, 1.0),
        ]);
        app.select_last();

        app.set_positions(vec![position("AAAUSDT", 1.0, 1.0)]);
        assert_eq!(app.selected, 0, "selection stays inside the table");
    }

    #[test]
    fn the_chart_follows_the_selection() {
        let mut app = app_with(vec![
            position("AAAUSDT", 9.0, 1.0),
            position("BBBUSDT", 1.0, 1.0),
        ]);
        assert_eq!(app.effective_symbol(), Some("AAAUSDT"));
        app.move_selection(1);
        assert_eq!(app.effective_symbol(), Some("BBBUSDT"));
    }

    #[test]
    fn chart_staleness_follows_the_interval() {
        let mut chart = Chart::new(Interval::M1);
        assert_eq!(
            chart.stale_after_ms(3_000),
            120_000,
            "two one-minute candles"
        );

        chart.interval = Interval::D1;
        assert_eq!(
            chart.stale_after_ms(3_000),
            172_800_000,
            "a daily candle is not stale after seconds"
        );

        chart.interval = Interval::M1;
        assert_eq!(
            chart.stale_after_ms(600_000),
            600_000,
            "a longer base tolerance still wins"
        );
    }

    #[test]
    fn intervals_step_in_both_directions() {
        let mut app = app_with(Vec::new());
        assert_eq!(app.chart.interval, Interval::M15);
        app.change_interval(true);
        assert_eq!(app.chart.interval, Interval::H1);
        app.change_interval(false);
        assert_eq!(app.chart.interval, Interval::M15);
    }

    #[test]
    fn feed_status_reports_loading_stale_and_error() {
        let mut status = FeedStatus::default();
        assert_eq!(status.summary(1_000, 5_000), "loading");
        assert!(
            status.is_stale(1_000, 5_000),
            "a feed that never delivered is stale"
        );

        status.mark_success(1_000);
        assert_eq!(status.summary(3_000, 5_000), "live 2s");
        assert!(!status.is_stale(3_000, 5_000));

        assert_eq!(status.summary(9_000, 5_000), "stale 8s");
        assert!(status.is_stale(9_000, 5_000));

        status.mark_failure("network error".to_owned());
        assert_eq!(status.summary(9_000, 5_000), "error: network error");
    }

    #[test]
    fn updates_fill_state_and_clear_errors() {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.apply(Update::Failed {
            feed: Feed::Positions,
            message: "boom".to_owned(),
        });
        assert_eq!(app.positions_feed.last_error.as_deref(), Some("boom"));

        app.apply(Update::Positions(vec![position("AAAUSDT", 1.0, 1.0)]));
        assert_eq!(
            app.positions_feed.last_error, None,
            "a success clears the error"
        );
        assert_eq!(app.positions.len(), 1);

        app.apply(Update::Account(Box::new(AccountSnapshot {
            balances: Vec::new(),
            wallet_balance: 1.0,
            equity: 2.0,
            unrealized_pnl: 1.0,
            available_balance: 1.0,
            initial_margin: 0.5,
            maintenance_margin: 0.1,
        })));
        assert_eq!(app.account.as_ref().map(|a| a.equity), Some(2.0));
        assert!(app.account_feed.last_error.is_none());
    }
}
