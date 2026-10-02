//! Application state: everything the UI renders from, and nothing else.
//!
//! This module deliberately has no dependency on the terminal widgets, so the
//! ordering and selection rules can be tested without a terminal.

use std::cell::Cell;
use std::cmp::Ordering;
use std::ops::Range;

use crate::auth::now_ms;
use crate::chart::{CandleLayout, MOVING_AVERAGE_WINDOWS, Viewport, moving_average};
use crate::performance::{CurveMode, EquitySeries, Metrics, Window};
use crate::venue::{AccountSnapshot, Interval, Kline, Position, PositionSide, Symbol, VenueId};

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

/// What the size column shows.
///
/// Contracts are the venue's own unit and comparable across contracts; the
/// notional value is comparable across an account, which is usually the more
/// useful question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeUnits {
    /// Contract quantity, as the venue reports it.
    Contracts,
    /// Position value in the quote asset.
    #[default]
    Notional,
}

impl SizeUnits {
    /// Swap between the two representations.
    pub fn toggle(&mut self) {
        *self = match self {
            Self::Contracts => Self::Notional,
            Self::Notional => Self::Contracts,
        };
    }

    /// Heading for the size column in this representation.
    pub fn heading(self) -> &'static str {
        match self {
            Self::Contracts => "Size",
            Self::Notional => "Notional",
        }
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
    /// The tradable-contract list used by the symbol picker.
    Symbols,
    /// The account's performance history.
    Performance,
}

impl Feed {
    /// Name used in status lines and error messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Positions => "positions",
            Self::Account => "account",
            Self::Chart => "chart",
            Self::Symbols => "symbols",
            Self::Performance => "performance",
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
    /// The venue's tradable contracts, for the symbol picker.
    Symbols(Vec<Symbol>),
    /// The account's daily history, for the performance panel.
    Equity(EquitySeries),
    /// Fresh mark prices, keyed by contract.
    Marks(Vec<(String, f64)>),
    /// A feed failed; the previous data stays on screen.
    Failed {
        /// Which feed failed.
        feed: Feed,
        /// What went wrong.
        message: String,
    },
}

/// Chart overlays the user can switch off.
///
/// These are preferences rather than data, so they survive switching symbol,
/// interval and account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlays {
    /// The moving-average lines and their legend.
    pub averages: bool,
    /// A line at the entry price of the contract being charted, when it is held.
    pub entry: bool,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            averages: true,
            entry: true,
        }
    }
}

impl Overlays {
    /// Show or hide the moving averages.
    pub fn toggle_averages(&mut self) {
        self.averages = !self.averages;
    }

    /// Show or hide the entry-price line.
    pub fn toggle_entry(&mut self) {
        self.entry = !self.entry;
    }
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
    /// Which overlays are drawn.
    pub(crate) overlays: Overlays,
    pub(crate) feed: FeedStatus,
    /// Columns the price pane had in the last frame.
    ///
    /// Layout feedback rather than chart data: a Braille cell holds one colour,
    /// so one candle per column is the most that can be drawn without two of
    /// them erasing each other. Set by the renderer, which is the only thing
    /// that knows how wide the pane is.
    columns: Cell<usize>,
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
            overlays: Overlays::default(),
            feed: FeedStatus::default(),
            columns: Cell::new(Self::COLUMNS_UNKNOWN),
        }
    }

    /// How many candles could be drawn if nothing were known about the screen.
    const COLUMNS_UNKNOWN: usize = 0;

    /// Tell the chart how many columns the price pane has.
    pub fn set_columns(&self, columns: u16) {
        self.columns.set(usize::from(columns));
    }

    /// How the candles are spread across those columns.
    pub fn layout(&self) -> CandleLayout {
        let columns = self.columns.get();
        let columns = u16::try_from(columns).ok().filter(|columns| *columns > 0);
        CandleLayout::fit(columns, self.viewport.visible())
    }

    /// The candles on screen: the window the viewport asked for, narrowed to the
    /// candles that fit across the pane at an even pitch.
    ///
    /// Zooming out past the width of the pane cannot show more candles than it
    /// has room for, so the window keeps its right-hand edge — the live end — and
    /// gives up its oldest candles instead of drawing two into the same cell.
    fn visible_range(&self) -> Range<usize> {
        let width = self.layout().candles;
        self.viewport.range(self.candles.len(), width)
    }

    /// Whether the chart is already pointed at `symbol` with `interval`.
    pub fn is_target(&self, symbol: &str) -> bool {
        self.symbol.as_deref() == Some(symbol)
    }

    /// Whether data is still missing for the current target.
    pub fn needs_load(&self) -> bool {
        self.symbol.is_some() && self.candles.is_empty()
    }

    /// Forget the target and its data, for example when the account changes.
    pub fn clear(&mut self) {
        self.symbol = None;
        self.candles.clear();
        self.averages = Default::default();
        self.follow = true;
        self.viewport = Viewport::new(self.viewport.visible());
        self.feed = FeedStatus::default();
    }

    /// Point the chart at a new target and drop the old data.
    pub fn reset(&mut self, symbol: String, interval: Interval) {
        self.clear();
        self.symbol = Some(symbol);
        self.interval = interval;
    }

    /// Replace the candle history.
    pub fn set_history(&mut self, candles: Vec<Kline>) {
        self.candles = candles;
        self.recompute_averages();
        if self.follow {
            let width = self.layout().candles;
            self.viewport.pin_to_end(self.candles.len(), width);
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
            let width = self.layout().candles;
            self.viewport.pin_to_end(self.candles.len(), width);
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
        &self.candles[self.visible_range()]
    }

    /// Moving-average values for the visible candles, aligned with [`Self::visible`].
    pub fn visible_average(&self, index: usize) -> &[Option<f64>] {
        let range = self.visible_range();
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

    /// Price range for the chart, widened to keep `extra` on screen.
    ///
    /// An entry price far from the current price would otherwise fall outside the
    /// pane, which is exactly when it is most worth seeing. It is only included
    /// while it stays within `limit` times the candle range, so one extreme
    /// position cannot squash the candles into a line.
    pub fn price_bounds_with(&self, extra: Option<f64>, limit: f64) -> Option<(f64, f64)> {
        let (low, high) = self.price_bounds()?;
        let Some(price) = extra.filter(|price| price.is_finite()) else {
            return Some((low, high));
        };

        let allowed = (high - low).max(f64::MIN_POSITIVE) * limit;
        if price >= low - allowed && price <= high + allowed {
            Some((low.min(price), high.max(price)))
        } else {
            Some((low, high))
        }
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
        let len = self.candles.len();
        let width = self.layout().candles;
        self.viewport.pan(delta, len, width);
        self.follow = self.viewport.is_at_end(len, width);
    }

    /// Widen or narrow the window.
    ///
    /// Zooming is not panning: a chart that was following the newest candle goes
    /// on following it, and one the user had stepped back into stays where it is.
    /// Re-pinning is what keeps a following chart's window exact — the width it
    /// was pinned to is not the width the new pitch draws.
    pub fn zoom(&mut self, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }

        // Nothing has said how wide the pane is — or it said something no pane
        // could be — so the raw factor is all there is to go on.
        let columns = usize::from(u16::try_from(self.columns.get()).unwrap_or(0));
        if columns == 0 {
            self.viewport.zoom(factor);
            return;
        }
        let columns = columns as u16;

        // One pitch per press, in the direction the factor points. Stepping the
        // count by the factor instead crossed a level or two depending on where
        // the request happened to sit, which made the bar width jump unevenly and
        // left `+` followed by `-` somewhere other than where it started.
        let current = self.layout().pitch;
        let target = if factor < 1.0 {
            current.saturating_add(1)
        } else {
            current.saturating_sub(1).max(1)
        };

        // The count that draws this pitch: one candle per column at that width,
        // which sits in the middle of the band `round(columns / count) == pitch`.
        let requested =
            (u32::from(columns) / u32::from(target)).clamp(1, u32::from(u16::MAX)) as usize;
        let requested = requested.clamp(Viewport::MIN_VISIBLE, Viewport::MAX_VISIBLE);

        // Near the zoom limits the count that would draw this pitch is not
        // available, and the press does nothing rather than snapping elsewhere.
        if CandleLayout::fit(Some(columns), requested).pitch != target {
            return;
        }

        self.viewport.set_visible(requested);
        if self.follow {
            let width = self.layout().candles;
            self.viewport.pin_to_end(self.candles.len(), width);
        }
    }

    /// Jump back to the newest candle and follow it again.
    pub fn follow_end(&mut self) {
        self.follow = true;
        let width = self.layout().candles;
        self.viewport.pin_to_end(self.candles.len(), width);
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

/// The symbol picker overlay: a filter query plus the highlighted row.
#[derive(Debug, Default)]
pub struct PickerState {
    pub(crate) query: String,
    pub(crate) selected: usize,
}

/// The account performance panel: whether it is open, what it shows, and the
/// history behind it.
#[derive(Debug, Default)]
pub struct Performance {
    pub(crate) open: bool,
    pub(crate) window: Window,
    pub(crate) mode: CurveMode,
    pub(crate) series: EquitySeries,
    pub(crate) feed: FeedStatus,
}

impl Performance {
    /// The series as the current window sees it.
    pub fn windowed(&self, now_ms: i64) -> EquitySeries {
        self.series.window(self.window, now_ms)
    }

    /// Metrics for the window, if there is enough history.
    pub fn metrics(&self, now_ms: i64) -> Option<Metrics> {
        self.windowed(now_ms).metrics()
    }
}

/// All state the UI renders from.
pub struct App {
    pub(crate) account_label: String,
    pub(crate) venue: VenueId,
    pub(crate) chart: Chart,
    /// Symbol chosen from the picker, which overrides the table selection.
    pub(crate) chart_override: Option<String>,
    /// Tradeable contracts, cached for the picker.
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) symbols_feed: FeedStatus,
    /// Open symbol picker, if any.
    pub(crate) picker: Option<PickerState>,
    /// Account performance panel.
    pub(crate) performance: Performance,
    /// Set when the history should be (re)fetched.
    pub(crate) history_requested: bool,
    pub(crate) positions: Vec<Position>,
    pub(crate) sort: Sort,
    /// What the size column shows.
    pub(crate) size_units: SizeUnits,
    pub(crate) selected: usize,
    pub(crate) account: Option<AccountSnapshot>,
    pub(crate) positions_feed: FeedStatus,
    pub(crate) account_feed: FeedStatus,
    pub(crate) stale_after_ms: i64,
    /// Set when the user asks to move to the next configured account.
    pub(crate) switch_requested: bool,
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
            symbols: Vec::new(),
            symbols_feed: FeedStatus::default(),
            picker: None,
            performance: Performance::default(),
            history_requested: false,
            positions: Vec::new(),
            sort: Sort::by_pnl_descending(),
            size_units: SizeUnits::default(),
            selected: 0,
            account: None,
            positions_feed: FeedStatus::default(),
            account_feed: FeedStatus::default(),
            stale_after_ms,
            switch_requested: false,
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
            Update::Marks(marks) => {
                self.apply_marks(&marks);
            }
            Update::Equity(series) => {
                tracing::debug!(points = series.len(), "account history loaded");
                self.performance.series = series;
                self.performance.feed.mark_success(now);
            }
            Update::Symbols(symbols) => {
                tracing::debug!(count = symbols.len(), "tradable contracts loaded");
                self.symbols = symbols;
                if let Some(picker) = &mut self.picker {
                    picker.selected = 0;
                }
                self.symbols_feed.mark_success(now);
            }
            Update::Failed { feed, message } => {
                let status = match feed {
                    Feed::Positions => &mut self.positions_feed,
                    Feed::Account => &mut self.account_feed,
                    Feed::Chart => &mut self.chart.feed,
                    Feed::Symbols => &mut self.symbols_feed,
                    Feed::Performance => &mut self.performance.feed,
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
            let primary = compare(left, right, column, self.size_units);
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

    /// Reset every feed for a newly selected account.
    ///
    /// Old data is dropped rather than blended: showing yesterday's positions
    /// under a new account's name would be worse than showing nothing.
    pub fn begin_account(&mut self, label: String, venue: VenueId) {
        self.account_label = label;
        self.venue = venue;
        self.positions.clear();
        self.selected = 0;
        self.account = None;
        self.positions_feed = FeedStatus::default();
        self.account_feed = FeedStatus::default();
        self.symbols.clear();
        self.symbols_feed = FeedStatus::default();
        self.chart_override = None;
        self.chart.clear();
        self.picker = None;
        // Another account has another history, but the panel stays as the user
        // left it — open or closed — and refetches in the background.
        let (open, window, mode) = (
            self.performance.open,
            self.performance.window,
            self.performance.mode,
        );
        self.performance = Performance {
            open,
            window,
            mode,
            ..Performance::default()
        };
        self.history_requested = open;
    }

    /// Ask the event loop to move to the next configured account.
    pub fn request_account_switch(&mut self) {
        self.switch_requested = true;
    }

    /// Consume a pending account switch.
    pub fn take_account_switch(&mut self) -> bool {
        std::mem::take(&mut self.switch_requested)
    }

    /// Re-price positions from fresh mark prices.
    ///
    /// Uses the venue's own formula — the price move times the signed size — so
    /// a short gains when the mark falls.
    fn apply_marks(&mut self, marks: &[(String, f64)]) {
        let mut changed = false;
        for position in &mut self.positions {
            let Some((_, mark)) = marks.iter().find(|(symbol, _)| *symbol == position.symbol)
            else {
                continue;
            };

            let signed_size = match position.side {
                PositionSide::Long => position.size,
                PositionSide::Short => -position.size,
            };
            position.mark_price = *mark;
            position.unrealized_pnl = (*mark - position.entry_price) * signed_size;
            position.notional = mark * signed_size;
            changed = true;
        }

        if changed {
            // The table may be ordered by a value that just moved.
            self.resort_keeping_selection();
        }
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

    /// Open the symbol picker.
    pub fn open_picker(&mut self) {
        self.picker = Some(PickerState::default());
    }

    /// Close the symbol picker without choosing anything.
    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Whether the picker is open.
    pub fn picker_is_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Hidden query and highlighted row, for the UI.
    pub fn picker_state(&self) -> Option<&PickerState> {
        self.picker.as_ref()
    }

    /// Whether the contract list still has to be fetched for the picker.
    pub fn picker_needs_symbols(&self) -> bool {
        self.picker.is_some() && self.symbols.is_empty()
    }

    /// Contracts matching the current query.
    ///
    /// Name-prefix matches rank above mid-name matches, which is what someone
    /// typing `btc` expects to see first.
    pub fn picker_matches(&self) -> Vec<&Symbol> {
        let query = self
            .picker
            .as_ref()
            .map(|picker| picker.query.trim().to_uppercase())
            .unwrap_or_default();

        let mut matches: Vec<&Symbol> = self
            .symbols
            .iter()
            .filter(|symbol| query.is_empty() || symbol.name.contains(&query))
            .collect();

        if !query.is_empty() {
            matches.sort_by_key(|symbol| (!symbol.name.starts_with(&query), symbol.name.clone()));
        }
        matches
    }

    /// Append a character to the filter query.
    pub fn picker_push(&mut self, character: char) {
        if let Some(picker) = &mut self.picker {
            picker.query.push(character.to_ascii_uppercase());
            picker.selected = 0;
        }
    }

    /// Remove the last character of the filter query.
    pub fn picker_backspace(&mut self) {
        if let Some(picker) = &mut self.picker {
            picker.query.pop();
            picker.selected = 0;
        }
    }

    /// Move the highlight, stopping at both ends.
    pub fn picker_move(&mut self, delta: isize) {
        let count = self.picker_matches().len();
        if count == 0 {
            return;
        }
        if let Some(picker) = &mut self.picker {
            let last = count - 1;
            let current = picker.selected.min(last) as isize;
            picker.selected = current.saturating_add(delta).clamp(0, last as isize) as usize;
        }
    }

    /// Choose the highlighted contract, point the chart at it, and close.
    ///
    /// Returns the symbol, or `None` when nothing matched — in which case the
    /// picker stays open so the query can be corrected.
    pub fn picker_confirm(&mut self) -> Option<String> {
        let symbol = {
            let matches = self.picker_matches();
            let selected = self.picker.as_ref().map_or(0, |picker| picker.selected);
            matches.get(selected).map(|symbol| symbol.name.clone())
        };

        if let Some(symbol) = &symbol {
            self.chart_override = Some(symbol.clone());
            self.picker = None;
        }
        symbol
    }

    /// Symbol highlighted in the picker, for the UI.
    pub fn picker_selected_symbol(&self) -> Option<&Symbol> {
        let selected = self.picker.as_ref().map_or(0, |picker| picker.selected);
        self.picker_matches().get(selected).copied()
    }

    /// Show or hide the performance panel, fetching history the first time.
    pub fn toggle_performance(&mut self) {
        self.performance.open = !self.performance.open;
        if self.performance.open {
            self.request_history();
        }
    }

    /// Whether the panel is open.
    pub fn performance_is_open(&self) -> bool {
        self.performance.open
    }

    /// Close the panel if it is open.
    pub fn close_performance(&mut self) {
        self.performance.open = false;
    }

    /// Select a window for the panel.
    pub fn set_performance_window(&mut self, window: Window) {
        if self.performance.window != window {
            tracing::debug!(?window, "performance window changed");
            self.performance.window = window;
        }
    }

    /// Move to the next or previous window.
    pub fn cycle_performance_window(&mut self, forward: bool) {
        let window = if forward {
            self.performance.window.next()
        } else {
            self.performance.window.previous()
        };
        self.set_performance_window(window);
    }

    /// Label of the active account, for the panel's title.
    pub fn account_label(&self) -> &str {
        &self.account_label
    }

    /// Swap the curve between the performance index and the wallet balance.
    pub fn toggle_curve_mode(&mut self) {
        self.performance.mode.toggle();
    }

    /// The panel's state, for the UI.
    pub fn performance(&self) -> &Performance {
        &self.performance
    }

    /// Ask for the history to be fetched.
    pub fn request_history(&mut self) {
        self.history_requested = true;
    }

    /// Consume a pending history request.
    pub fn take_history_request(&mut self) -> bool {
        std::mem::take(&mut self.history_requested)
    }

    /// Swap the size column between contracts and notional value.
    pub fn toggle_size_units(&mut self) {
        self.size_units.toggle();
        self.resort_keeping_selection();
    }

    /// What the size column currently shows.
    pub fn size_units(&self) -> SizeUnits {
        self.size_units
    }

    /// Show or hide the chart's moving averages.
    pub fn toggle_averages(&mut self) {
        self.chart.overlays.toggle_averages();
    }

    /// Show or hide the entry-price line on the chart.
    pub fn toggle_entry_line(&mut self) {
        self.chart.overlays.toggle_entry();
    }

    /// Whether the chart is showing moving averages.
    pub fn shows_averages(&self) -> bool {
        self.chart.overlays.averages
    }

    /// Whether the chart is showing the entry-price line.
    pub fn shows_entry_line(&self) -> bool {
        self.chart.overlays.entry
    }

    /// The open position for the contract the chart is showing, if there is one.
    ///
    /// Looked up by symbol rather than by the selected row: the chart can be
    /// pointed at a contract from the picker, and the selection can move while
    /// the chart keeps its own target.
    pub fn chart_position(&self) -> Option<&Position> {
        let symbol = self.chart.symbol.as_deref()?;
        self.positions
            .iter()
            .find(|position| position.symbol == symbol)
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
fn compare(
    left: &Position,
    right: &Position,
    column: SortColumn,
    size_units: SizeUnits,
) -> Ordering {
    match column {
        SortColumn::Symbol => left.symbol.cmp(&right.symbol),
        SortColumn::Side => side_rank(left.side).cmp(&side_rank(right.side)),
        // Sorting follows what the column shows, so the visible order always
        // matches the visible numbers.
        SortColumn::Size => match size_units {
            SizeUnits::Contracts => left.size.total_cmp(&right.size),
            SizeUnits::Notional => value(left).total_cmp(&value(right)),
        },
        SortColumn::Entry => left.entry_price.total_cmp(&right.entry_price),
        SortColumn::Mark => left.mark_price.total_cmp(&right.mark_price),
        SortColumn::Margin => left.initial_margin.total_cmp(&right.initial_margin),
        SortColumn::Pnl => left.unrealized_pnl.total_cmp(&right.unrealized_pnl),
    }
}

/// Position value at the mark price, always positive.
///
/// The venue reports a negative notional for shorts; direction belongs to the
/// side column, not to the size.
fn value(position: &Position) -> f64 {
    position.notional.abs()
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

    use crate::chart::Viewport;

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

    fn symbol(name: &str) -> crate::venue::Symbol {
        crate::venue::Symbol {
            name: name.to_owned(),
            base_asset: name.trim_end_matches("USDT").to_owned(),
            quote_asset: "USDT".to_owned(),
        }
    }

    fn picker_app() -> App {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.apply(Update::Symbols(vec![
            symbol("BTCUSDT"),
            symbol("ETHUSDT"),
            symbol("WBTCUSDT"),
            symbol("1000BONKUSDT"),
        ]));
        app.open_picker();
        app
    }

    #[test]
    fn the_picker_filters_and_ranks_prefix_matches_first() {
        let mut app = picker_app();
        assert_eq!(
            app.picker_matches().len(),
            4,
            "an empty query lists everything"
        );

        for character in "btc".chars() {
            app.picker_push(character);
        }
        let names: Vec<&str> = app
            .picker_matches()
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["BTCUSDT", "WBTCUSDT"],
            "the prefix match comes before the mid-name match"
        );
        assert_eq!(app.picker_state().map(|p| p.query.as_str()), Some("BTC"));
    }

    #[test]
    fn backspace_widens_the_filter_again() {
        let mut app = picker_app();
        app.picker_push('z');
        assert!(app.picker_matches().is_empty(), "no contract contains Z");

        app.picker_backspace();
        assert_eq!(app.picker_matches().len(), 4);
    }

    #[test]
    fn picker_selection_stops_at_both_ends() {
        let mut app = picker_app();
        assert_eq!(
            app.picker_selected_symbol().map(|s| s.name.as_str()),
            Some("BTCUSDT")
        );

        app.picker_move(-5);
        assert_eq!(
            app.picker_selected_symbol().map(|s| s.name.as_str()),
            Some("BTCUSDT")
        );
        app.picker_move(99);
        assert_eq!(
            app.picker_selected_symbol().map(|s| s.name.as_str()),
            Some("1000BONKUSDT"),
            "the last match is the end of the list"
        );
    }

    #[test]
    fn typing_resets_the_highlight_to_the_best_match() {
        let mut app = picker_app();
        app.picker_move(3);
        app.picker_push('e');
        assert_eq!(
            app.picker_selected_symbol().map(|s| s.name.as_str()),
            Some("ETHUSDT")
        );
    }

    #[test]
    fn confirming_points_the_chart_at_the_chosen_contract() {
        let mut app = picker_app();
        for character in "wbtc".chars() {
            app.picker_push(character);
        }

        let chosen = app.picker_confirm();
        assert_eq!(chosen.as_deref(), Some("WBTCUSDT"));
        assert!(!app.picker_is_open(), "choosing closes the picker");
        assert_eq!(app.effective_symbol(), Some("WBTCUSDT"), "chart follows it");
    }

    #[test]
    fn confirming_with_no_match_keeps_the_picker_open() {
        let mut app = picker_app();
        app.picker_push('z');

        assert_eq!(app.picker_confirm(), None);
        assert!(app.picker_is_open(), "the query can still be corrected");
        assert_eq!(app.effective_symbol(), None, "the chart is left alone");
    }

    #[test]
    fn opening_the_picker_asks_for_the_contract_list_only_when_missing() {
        let mut app = App::new(
            "main".to_owned(),
            VenueId::BinanceFutures,
            Interval::M15,
            3_000,
        );
        app.open_picker();
        assert!(app.picker_needs_symbols(), "nothing cached yet");

        app.apply(Update::Symbols(vec![symbol("BTCUSDT")]));
        assert!(!app.picker_needs_symbols());
        assert!(
            app.symbols_feed.last_error.is_none(),
            "the fetch is recorded"
        );
    }

    #[test]
    fn closing_the_picker_leaves_the_chart_alone() {
        let mut app = picker_app();
        app.close_picker();
        assert!(!app.picker_is_open());
        assert_eq!(app.effective_symbol(), None);
    }

    #[test]
    fn overlays_start_on_and_toggle_independently() {
        let mut app = app_with(Vec::new());
        assert!(app.shows_averages(), "moving averages start visible");
        assert!(app.shows_entry_line(), "the entry line starts visible");

        app.toggle_averages();
        assert!(!app.shows_averages());
        assert!(app.shows_entry_line(), "the other overlay is untouched");

        app.toggle_entry_line();
        assert!(!app.shows_entry_line());
        app.toggle_averages();
        assert!(app.shows_averages(), "toggling back restores it");
    }

    #[test]
    fn overlay_preferences_survive_data_changes() {
        let mut app = app_with(vec![position("AAAUSDT", 1.0, 1.0)]);
        app.toggle_averages();
        app.toggle_entry_line();

        // Symbol change, interval change and account switch all drop data.
        app.chart.reset("BBBUSDT".to_owned(), Interval::H1);
        assert!(!app.shows_averages(), "a new symbol keeps the preference");
        assert!(!app.shows_entry_line());

        app.begin_account("Paper".to_owned(), VenueId::BinanceFutures);
        assert!(!app.shows_averages(), "a new account keeps the preference");
        assert!(!app.shows_entry_line());
    }

    #[test]
    fn the_charted_contract_is_looked_up_by_symbol() {
        let mut app = app_with(vec![
            position("AAAUSDT", 1.0, 1.0),
            position("BBBUSDT", 2.0, 1.0),
        ]);
        assert!(app.chart_position().is_none(), "no chart target yet");

        app.chart.reset("BBBUSDT".to_owned(), Interval::M15);
        assert_eq!(
            app.chart_position().map(|p| p.symbol.as_str()),
            Some("BBBUSDT"),
            "found even though it is not the selected row"
        );

        // The picker can point the chart at something that is not held.
        app.set_chart_symbol("ZZZUSDT".to_owned());
        app.chart.reset("ZZZUSDT".to_owned(), Interval::M15);
        assert!(app.chart_position().is_none());
    }

    #[test]
    fn zooming_steps_one_pitch_at_a_time_and_comes_back() {
        let mut app = app_with(Vec::new());
        app.chart.reset("AAAUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "AAAUSDT".to_owned(),
            interval: Interval::M15,
            candles: candle_series(300),
        });
        app.chart.set_columns(195);

        let start = (app.chart.layout().pitch, app.chart.visible().len());
        let mut steps = 0;
        let mut last = start.0;

        for _ in 0..16 {
            app.zoom_chart(0.8);
            let pitch = app.chart.layout().pitch;

            assert!(
                pitch == last || pitch == last + 1,
                "one pitch per press, not {last} to {pitch}: {:?}",
                app.chart.layout()
            );
            if pitch == last {
                break; // the deepest zoom: there is nothing narrower to draw
            }
            last = pitch;
            steps += 1;
        }
        assert!(steps >= 5, "zooming in has levels to step through: {steps}");

        for _ in 0..steps {
            app.zoom_chart(1.25);
        }
        assert_eq!(
            (app.chart.layout().pitch, app.chart.visible().len()),
            start,
            "stepping back out returns exactly where it started"
        );
    }

    #[test]
    fn zooming_keeps_following_the_newest_candle() {
        let mut app = app_with(Vec::new());
        app.chart.reset("AAAUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "AAAUSDT".to_owned(),
            interval: Interval::M15,
            candles: candle_series(300),
        });
        app.chart.set_columns(195);

        for factor in [0.8, 0.8, 1.25, 1.25] {
            app.zoom_chart(factor);

            assert!(app.chart.follow, "a zoom is not a pan: {factor}");
            assert!(
                app.chart.visible().last().map(|candle| candle.open_time_ms)
                    == app.chart.candles.last().map(|candle| candle.open_time_ms),
                "the newest candle stays on screen after zooming by {factor}"
            );
        }

        // And stepping back into history still turns following off.
        app.zoom_chart(1.25);
        app.pan_chart(-(Viewport::PAN_STEP as isize));
        assert!(!app.chart.follow);
    }

    /// A rising series, oldest first.
    fn candle_series(count: i64) -> Vec<crate::venue::Kline> {
        (0..count)
            .map(|index| crate::venue::Kline {
                open_time_ms: index * 900_000,
                open: 100.0 + index as f64,
                high: 101.0 + index as f64,
                low: 99.0 + index as f64,
                close: 100.0 + index as f64,
                volume: 1.0,
                close_time_ms: index * 900_000 + 899_999,
                closed: true,
            })
            .collect()
    }

    #[test]
    fn every_zoom_press_changes_what_is_drawn() {
        let mut app = app_with(Vec::new());
        app.chart.reset("AAAUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "AAAUSDT".to_owned(),
            interval: Interval::M15,
            candles: candle_series(300),
        });
        // A 195-column pane draws 195 candles at a pitch of one cell and 97 at
        // two: a zoom step landing between the two would draw exactly the same
        // chart, which is what made the key look broken.
        app.chart.set_columns(195);

        let mut counts = vec![app.chart.visible().len()];
        for _ in 0..12 {
            app.zoom_chart(0.8);
            counts.push(app.chart.visible().len());
        }

        assert!(
            counts.windows(2).filter(|pair| pair[0] != pair[1]).count() >= 4,
            "zooming in has several real levels: {counts:?}"
        );
        for pair in counts.windows(2) {
            if pair[0] == pair[1] {
                assert!(
                    pair[0] <= 25,
                    "only the deepest zoom may draw the same chart twice: {counts:?}"
                );
            } else {
                assert!(pair[1] < pair[0], "zooming in draws fewer: {counts:?}");
            }
        }

        let mut out = vec![app.chart.visible().len()];
        for _ in 0..12 {
            app.zoom_chart(1.25);
            out.push(app.chart.visible().len());
        }

        for pair in out.windows(2) {
            if pair[0] == pair[1] {
                assert_eq!(pair[0], 195, "the pane is full: {out:?}");
            } else {
                assert!(pair[1] > pair[0], "zooming out draws more: {out:?}");
            }
        }
    }

    #[test]
    fn the_entry_price_is_kept_on_screen_within_reason() {
        let mut app = app_with(Vec::new());
        app.chart.reset("AAAUSDT".to_owned(), Interval::M15);
        app.apply(Update::History {
            symbol: "AAAUSDT".to_owned(),
            interval: Interval::M15,
            candles: (0..20)
                .map(|index| crate::venue::Kline {
                    open_time_ms: index * 900_000,
                    open: 100.0,
                    high: 101.0,
                    low: 99.0,
                    close: 100.0,
                    volume: 1.0,
                    close_time_ms: index * 900_000 + 899_999,
                    closed: true,
                })
                .collect(),
        });
        let (low, high) = app.chart.price_bounds().expect("bounds");
        assert_eq!((low, high), (99.0, 101.0));

        // Just below the range: widened so the line is visible.
        let (low, high) = app
            .chart
            .price_bounds_with(Some(97.0), 3.0)
            .expect("bounds");
        assert_eq!((low, high), (97.0, 101.0), "the entry is included");

        // Far below: the candles win, and the line simply falls outside.
        let (low, high) = app.chart.price_bounds_with(Some(1.0), 3.0).expect("bounds");
        assert_eq!((low, high), (99.0, 101.0), "an extreme entry is ignored");

        // No entry at all.
        let (low, high) = app.chart.price_bounds_with(None, 3.0).expect("bounds");
        assert_eq!((low, high), (99.0, 101.0));

        // Nonsense values must not corrupt the range.
        let (low, high) = app
            .chart
            .price_bounds_with(Some(f64::NAN), 3.0)
            .expect("bounds");
        assert_eq!((low, high), (99.0, 101.0));
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
    fn mark_prices_reprice_positions_with_the_venues_formula() {
        let mut long = position("AAAUSDT", 1.0, 2.0);
        long.entry_price = 100.0;
        let mut short = position("BBBUSDT", -1.0, 3.0);
        short.entry_price = 100.0;
        let mut app = app_with(vec![long, short]);

        app.apply(Update::Marks(vec![
            ("AAAUSDT".to_owned(), 110.0),
            ("BBBUSDT".to_owned(), 110.0),
            ("UNKNOWNUSDT".to_owned(), 1.0),
        ]));

        let long = app
            .positions
            .iter()
            .find(|p| p.symbol == "AAAUSDT")
            .expect("long survives");
        assert_eq!(long.mark_price, 110.0);
        assert_eq!(long.unrealized_pnl, 20.0, "(110-100) * 2 contracts");
        assert_eq!(long.notional, 220.0);

        let short = app
            .positions
            .iter()
            .find(|p| p.symbol == "BBBUSDT")
            .expect("short survives");
        assert_eq!(
            short.unrealized_pnl, -30.0,
            "(110-100) * -3 contracts: a short loses when the mark rises"
        );
        assert_eq!(short.notional, -330.0);
    }

    #[test]
    fn repricing_keeps_the_table_ordered_and_the_selection_intact() {
        // Both are longs (the helper derives the side from the PnL sign), with
        // AAA starting behind BBB.
        let mut rising = position("AAAUSDT", 10.0, 2.0);
        rising.entry_price = 100.0;
        let mut falling = position("BBBUSDT", 50.0, 2.0);
        falling.entry_price = 100.0;
        let mut app = app_with(vec![rising, falling]);
        app.select_last();
        let selected = app.selected_position().map(|p| p.symbol.clone());
        assert_eq!(selected.as_deref(), Some("AAAUSDT"));

        // AAA jumps ahead of BBB.
        app.apply(Update::Marks(vec![("AAAUSDT".to_owned(), 200.0)]));

        assert_eq!(
            app.positions.first().map(|p| p.symbol.as_str()),
            Some("AAAUSDT"),
            "the table re-sorts on the column it is ordered by"
        );
        assert_eq!(
            app.selected_position().map(|p| p.symbol.as_str()),
            Some("AAAUSDT"),
            "the selected contract stays selected"
        );
    }

    #[test]
    fn switching_accounts_clears_every_feed() {
        let mut app = picker_app();
        app.set_positions(vec![position("AAAUSDT", 1.0, 1.0)]);
        app.apply(Update::Account(Box::new(crate::venue::AccountSnapshot {
            balances: Vec::new(),
            wallet_balance: 1.0,
            equity: 1.0,
            unrealized_pnl: 0.0,
            available_balance: 1.0,
            initial_margin: 0.0,
            maintenance_margin: 0.0,
            multi_assets: None,
        })));
        app.focus_chart("AAAUSDT".to_owned());

        app.begin_account("Paper".to_owned(), VenueId::BinanceFutures);

        assert_eq!(
            app.account_label, "Paper",
            "the header names the new account"
        );
        assert!(app.positions.is_empty(), "old positions are dropped");
        assert!(app.account.is_none(), "old balances are dropped");
        assert!(app.chart_symbol().is_none(), "the chart forgets its target");
        assert!(app.effective_symbol().is_none());
        assert!(app.symbols.is_empty(), "the contract list is refetched");
        assert!(!app.picker_is_open(), "an open picker is closed");
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn an_account_switch_is_requested_once_and_consumed_once() {
        let mut app = app_with(Vec::new());
        assert!(!app.take_account_switch(), "nothing pending by default");

        app.request_account_switch();
        assert!(app.take_account_switch(), "the request is delivered");
        assert!(!app.take_account_switch(), "and only once");
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
            multi_assets: None,
        })));
        assert_eq!(app.account.as_ref().map(|a| a.equity), Some(2.0));
        assert!(app.account_feed.last_error.is_none());
    }
}
