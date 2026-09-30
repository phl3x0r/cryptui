//! Application state: everything the UI renders from, and nothing else.
//!
//! This module deliberately has no dependency on the terminal widgets, so the
//! ordering and selection rules can be tested without a terminal.

use std::cmp::Ordering;

use crate::auth::now_ms;
use crate::venue::{AccountSnapshot, Interval, Position, PositionSide, VenueId};

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
}

impl Feed {
    /// Name used in status lines and error messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Positions => "positions",
            Self::Account => "account",
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
    /// A feed failed; the previous data stays on screen.
    Failed {
        /// Which feed failed.
        feed: Feed,
        /// What went wrong.
        message: String,
    },
}

/// All state the UI renders from.
pub struct App {
    pub(crate) account_label: String,
    pub(crate) venue: VenueId,
    pub(crate) interval: Interval,
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
            interval,
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
            Update::Failed { feed, message } => {
                let status = match feed {
                    Feed::Positions => &mut self.positions_feed,
                    Feed::Account => &mut self.account_feed,
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
    pub fn move_selection(&mut self, delta: isize) {
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
        self.selected = 0;
    }

    /// Select the last row.
    pub fn select_last(&mut self) {
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
    pub fn change_interval(&mut self, forward: bool) {
        self.interval = if forward {
            self.interval.next()
        } else {
            self.interval.previous()
        };
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

    /// Symbol the chart should show: the selection, or the first row.
    pub fn chart_symbol(&self) -> Option<&str> {
        self.selected_position()
            .or_else(|| self.positions.first())
            .map(|position| position.symbol.as_str())
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

    use super::{App, Feed, FeedStatus, Sort, SortColumn, Update};

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
        assert_eq!(app.chart_symbol(), Some("AAAUSDT"));
        app.move_selection(1);
        assert_eq!(app.chart_symbol(), Some("BBBUSDT"));
    }

    #[test]
    fn intervals_step_in_both_directions() {
        let mut app = app_with(Vec::new());
        assert_eq!(app.interval, Interval::M15);
        app.change_interval(true);
        assert_eq!(app.interval, Interval::H1);
        app.change_interval(false);
        assert_eq!(app.interval, Interval::M15);
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
