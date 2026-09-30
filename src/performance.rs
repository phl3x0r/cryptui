//! Account performance: a daily value series, the windows you can look at, and
//! the metrics computed from it.
//!
//! **What the series is.** The venue exposes no equity history, so the curve is
//! a reconstruction from income records anchored on the current wallet balance:
//! `wallet(t) = wallet(now) - Σ income after t`. That makes it a *wallet balance*
//! series — the account value excluding unrealized profit, which is not an income
//! event and cannot be recovered historically. Excluding it is also what makes
//! the curve measure realised performance rather than the mark-to-market wobble
//! of whatever happens to be open.
//!
//! **External flows.** Deposits and withdrawals move the balance without being
//! performance, so each point carries the net flow for its day and returns are
//! computed net of it. Without that, a deposit would read as a gain and a
//! withdrawal as a drawdown.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How much history a view covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The last month.
    Month,
    /// The last three months.
    Quarter,
    /// The last year.
    Year,
    /// Everything available.
    All,
}

/// A day, in milliseconds.
pub const DAY_MS: i64 = 86_400_000;
/// Convention for annualising: crypto markets never close.
pub const DAYS_PER_YEAR: f64 = 365.0;

impl Window {
    /// Every window, shortest first.
    pub const ALL: [Self; 4] = [Self::Month, Self::Quarter, Self::Year, Self::All];

    /// Short label, as shown in the panel.
    pub fn label(self) -> &'static str {
        match self {
            Self::Month => "1M",
            Self::Quarter => "3M",
            Self::Year => "1Y",
            Self::All => "All",
        }
    }

    /// Spoken form, for hints and errors.
    pub fn description(self) -> &'static str {
        match self {
            Self::Month => "1 month",
            Self::Quarter => "3 months",
            Self::Year => "1 year",
            Self::All => "all available history",
        }
    }

    /// The next window, wrapping around.
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|w| *w == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    /// The previous window, wrapping around.
    pub fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|w| *w == self).unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// Start of the window, or `None` for everything available.
    pub fn start_ms(self, now_ms: i64) -> Option<i64> {
        let days = match self {
            Self::Month => 30,
            Self::Quarter => 90,
            Self::Year => 365,
            Self::All => return None,
        };
        Some(now_ms - days * DAY_MS)
    }
}

/// The account's value at the end of one day.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EquityPoint {
    /// End of the day, in milliseconds since the epoch.
    pub time_ms: i64,
    /// Wallet balance: the account value excluding unrealized profit.
    pub wallet: f64,
    /// Net deposits and withdrawals during this day, excluded from returns.
    pub external_flow: f64,
}

impl EquityPoint {
    /// The UTC day this point belongs to.
    fn day(self) -> i64 {
        self.time_ms.div_euclid(DAY_MS)
    }
}

/// Daily observations, oldest first, at most one per day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EquitySeries {
    points: Vec<EquityPoint>,
}

impl EquitySeries {
    /// Build a series from arbitrary observations.
    ///
    /// Observations are sorted, and a day seen more than once keeps the last
    /// balance with its flows summed, so a series can be rebuilt from parts.
    pub fn new(points: Vec<EquityPoint>) -> Self {
        let mut series = Self::default();
        series.extend(points);
        series
    }

    /// Fold in more observations.
    pub fn extend(&mut self, points: Vec<EquityPoint>) {
        for point in points {
            match self
                .points
                .iter_mut()
                .find(|existing| existing.day() == point.day())
            {
                Some(existing) => {
                    if point.time_ms >= existing.time_ms {
                        existing.time_ms = point.time_ms;
                        existing.wallet = point.wallet;
                    }
                    existing.external_flow += point.external_flow;
                }
                None => self.points.push(point),
            }
        }
        self.points.sort_by_key(|point| point.time_ms);
    }

    /// Merge another series, preferring its points day by day.
    ///
    /// The venue's daily aggregates know about deposits and withdrawals and the
    /// locally recorded ones do not, so the other series wins where they overlap.
    pub fn merge_preferring(&mut self, other: Self) {
        for point in other.points {
            match self
                .points
                .iter_mut()
                .find(|existing| existing.day() == point.day())
            {
                Some(existing) => *existing = point,
                None => self.points.push(point),
            }
        }
        self.points.sort_by_key(|point| point.time_ms);
    }

    /// The observations.
    pub fn points(&self) -> &[EquityPoint] {
        &self.points
    }

    /// Whether there is nothing to show.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// How many days are observed.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// First and last observation times.
    pub fn span(&self) -> Option<(i64, i64)> {
        let first = self.points.first()?.time_ms;
        let last = self.points.last()?.time_ms;
        Some((first, last))
    }

    /// Just the part of the series inside a window.
    pub fn window(&self, window: Window, now_ms: i64) -> Self {
        let Some(start) = window.start_ms(now_ms) else {
            return self.clone();
        };
        Self {
            points: self
                .points
                .iter()
                .copied()
                .filter(|point| point.time_ms >= start)
                .collect(),
        }
    }

    /// Performance over the series, or `None` with too little to measure.
    pub fn metrics(&self) -> Option<Metrics> {
        let returns = self.daily_returns();
        if returns.is_empty() {
            return None;
        }

        let (from_ms, to_ms) = self.span()?;
        let days = ((to_ms - from_ms) as f64 / DAY_MS as f64).max(1.0);
        let count = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / count;

        // Sample standard deviation; a single return has no spread.
        let variance = if returns.len() > 1 {
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (count - 1.0)
        } else {
            0.0
        };
        let deviation = variance.sqrt();

        // Compounding the daily returns gives the time-weighted return: what the
        // account did, independent of how much was deposited along the way.
        let growth = returns.iter().fold(1.0, |value, r| value * (1.0 + r));
        let total_return = growth - 1.0;

        let annualise = |value: f64| value * DAYS_PER_YEAR.sqrt();
        let sharpe = (deviation > 0.0).then(|| annualise(mean / deviation));
        let volatility = (deviation > 0.0).then(|| annualise(deviation));

        let downside = (returns
            .iter()
            .filter(|r| **r < 0.0)
            .map(|r| r * r)
            .sum::<f64>()
            / count)
            .sqrt();
        let sortino = (downside > 0.0).then(|| annualise(mean / downside));

        let max_drawdown = max_drawdown(&returns);
        let cagr = (growth > 0.0).then(|| growth.powf(DAYS_PER_YEAR / days) - 1.0);
        let calmar = match (cagr, max_drawdown) {
            (Some(cagr), drawdown) if drawdown > 0.0 => Some(cagr / drawdown),
            _ => None,
        };

        let wins = returns.iter().filter(|r| **r > 0.0).count() as f64;

        Some(Metrics {
            days,
            samples: returns.len(),
            total_return,
            cagr,
            volatility,
            sharpe,
            sortino,
            max_drawdown,
            calmar,
            win_rate: Some(wins / count),
            best_day: returns.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            worst_day: returns.iter().copied().fold(f64::INFINITY, f64::min),
            from_ms,
            to_ms,
        })
    }

    /// Day-over-day returns, adjusted for deposits and withdrawals.
    ///
    /// Days whose opening balance is not positive are skipped rather than
    /// divided by.
    pub fn daily_returns(&self) -> Vec<f64> {
        self.points
            .windows(2)
            .filter_map(|pair| {
                let (previous, next) = (&pair[0], &pair[1]);
                if previous.wallet <= 0.0 {
                    return None;
                }
                Some((next.wallet - previous.wallet - next.external_flow) / previous.wallet)
            })
            .collect()
    }
}

/// What a series says about performance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Calendar days the window covers.
    pub days: f64,
    /// Returns the metrics were computed from.
    pub samples: usize,
    /// Time-weighted return over the window.
    pub total_return: f64,
    /// Annualised total return, `None` when the window grew by nothing.
    pub cagr: Option<f64>,
    /// Annualised standard deviation of daily returns.
    pub volatility: Option<f64>,
    /// Annualised excess return per unit of deviation, risk-free rate assumed 0.
    pub sharpe: Option<f64>,
    /// Annualised excess return per unit of downside deviation.
    pub sortino: Option<f64>,
    /// Deepest peak-to-trough decline of the return index, as a positive fraction.
    pub max_drawdown: f64,
    /// Annualised return per unit of drawdown.
    pub calmar: Option<f64>,
    /// Share of days that ended higher.
    pub win_rate: Option<f64>,
    /// Best single day.
    pub best_day: f64,
    /// Worst single day.
    pub worst_day: f64,
    /// Start of the measured window.
    pub from_ms: i64,
    /// End of the measured window.
    pub to_ms: i64,
}

impl Metrics {
    /// How long the measured window is, in whole days.
    pub fn window_days(&self) -> u64 {
        (self.days.round() as u64).max(1)
    }

    /// Whether the window is long enough for annualised figures to mean much.
    pub fn annualised_is_meaningful(&self) -> bool {
        self.days >= 30.0
    }
}

/// Deepest decline of the compounded return index, as a positive fraction.
fn max_drawdown(returns: &[f64]) -> f64 {
    let mut index = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut worst = 0.0_f64;

    for value in returns {
        index *= 1.0 + value;
        peak = peak.max(index);
        if peak > 0.0 {
            worst = worst.max((peak - index) / peak);
        }
    }
    worst
}

/// Human-readable duration, for the coverage line.
pub fn describe_duration(milliseconds: i64) -> String {
    let duration = Duration::from_millis(milliseconds.unsigned_abs());
    let days = duration.as_secs() / 86_400;
    match days {
        0 => "under a day".to_owned(),
        1 => "1 day".to_owned(),
        days if days < 60 => format!("{days} days"),
        days => format!("{:.1} months", days as f64 / 30.44),
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, EquityPoint, EquitySeries, Metrics, Window, describe_duration};

    fn point(day: i64, wallet: f64) -> EquityPoint {
        EquityPoint {
            time_ms: day * DAY_MS,
            wallet,
            external_flow: 0.0,
        }
    }

    fn series(wallets: &[f64]) -> EquitySeries {
        EquitySeries::new(
            wallets
                .iter()
                .enumerate()
                .map(|(index, wallet)| point(index as i64, *wallet))
                .collect(),
        )
    }

    fn metrics(wallets: &[f64]) -> Metrics {
        series(wallets).metrics().expect("enough history")
    }

    #[test]
    fn one_point_is_not_enough_to_measure() {
        assert!(series(&[100.0]).metrics().is_none());
        assert!(EquitySeries::default().metrics().is_none());
    }

    #[test]
    fn a_flat_series_has_no_drawdown_and_no_risk_adjusted_figures() {
        let metrics = metrics(&[100.0, 100.0, 100.0]);

        assert_eq!(metrics.total_return, 0.0);
        assert_eq!(metrics.max_drawdown, 0.0);
        assert_eq!(metrics.sharpe, None, "no deviation, no Sharpe");
        assert_eq!(metrics.volatility, None);
        assert_eq!(metrics.calmar, None, "no drawdown to divide by");
        assert_eq!(metrics.win_rate, Some(0.0), "no day ended higher");
    }

    #[test]
    fn a_steady_doubling_compounds_over_the_window() {
        // 100 -> 200 over 365 days.
        let metrics = metrics(&[
            100.0, 110.0, 121.0, 133.1, 146.41, 161.05, 177.16, 194.87, 214.36,
        ]);

        assert!((metrics.total_return - 1.1436).abs() < 1e-3, "{metrics:?}");
        assert_eq!(metrics.win_rate, Some(1.0), "every day gained");
        assert_eq!(metrics.max_drawdown, 0.0);
        assert!(
            (metrics.worst_day - 0.1).abs() < 1e-4,
            "every day gained about a tenth: {metrics:?}"
        );
    }

    #[test]
    fn drawdown_is_measured_peak_to_trough() {
        // 100 -> 120 -> 90 -> 99: the worst decline is 90/120 - 1 = -25%.
        let metrics = metrics(&[100.0, 120.0, 90.0, 99.0]);

        assert!((metrics.max_drawdown - 0.25).abs() < 1e-9, "{metrics:?}");
        assert!(
            metrics.calmar.is_some(),
            "a drawdown makes Calmar computable"
        );
    }

    #[test]
    fn deposits_are_not_performance() {
        // A deposit of 100 without any trading must show no return at all.
        let deposited = EquitySeries::new(vec![
            point(0, 100.0),
            EquityPoint {
                time_ms: DAY_MS,
                wallet: 200.0,
                external_flow: 100.0,
            },
        ]);
        let metrics = deposited.metrics().expect("two points");
        assert!(
            metrics.total_return.abs() < 1e-12,
            "a deposit is not a gain: {metrics:?}"
        );

        // The same balance without the flow is a 100% gain.
        let traded = series(&[100.0, 200.0]).metrics().expect("two points");
        assert!((traded.total_return - 1.0).abs() < 1e-12);
    }

    #[test]
    fn withdrawals_are_not_drawdowns() {
        let withdrawn = EquitySeries::new(vec![
            point(0, 100.0),
            EquityPoint {
                time_ms: DAY_MS,
                wallet: 50.0,
                external_flow: -50.0,
            },
        ]);
        let metrics = withdrawn.metrics().expect("two points");
        assert_eq!(metrics.total_return, 0.0, "taking money out is not a loss");
        assert_eq!(metrics.max_drawdown, 0.0);
    }

    #[test]
    fn sharpe_uses_the_spread_of_daily_returns() {
        // Returns +10%, -10%, +10%: mean 3.33%, sample deviation 11.55%.
        let metrics = metrics(&[100.0, 110.0, 99.0, 108.9]);

        let expected_mean: f64 = (0.1 - 0.1 + 0.1) / 3.0;
        let expected_deviation = (((0.1 - expected_mean).powi(2)
            + (-0.1 - expected_mean).powi(2)
            + (0.1 - expected_mean).powi(2))
            / 2.0)
            .sqrt();
        let expected = expected_mean / expected_deviation * 365f64.sqrt();

        let sharpe = metrics.sharpe.expect("deviation is non-zero");
        assert!(
            (sharpe - expected).abs() < 1e-9,
            "got {sharpe}, want {expected}"
        );
        assert!(
            metrics.sortino.is_some(),
            "a losing day gives downside risk"
        );
    }

    #[test]
    fn a_series_without_losses_has_no_sortino() {
        let metrics = metrics(&[100.0, 101.0, 102.0, 103.0]);
        assert_eq!(metrics.sortino, None, "no downside to measure");
    }

    #[test]
    fn cagr_annualises_the_window() {
        // Doubling over half a year is roughly 4x annualised.
        let days = 182;
        let points: Vec<EquityPoint> = (0..=days)
            .map(|day| point(day, 100.0 * 2f64.powf(day as f64 / days as f64)))
            .collect();
        let metrics = EquitySeries::new(points).metrics().expect("history");

        let cagr = metrics.cagr.expect("grew");
        assert!((cagr - 3.0).abs() < 0.05, "expected about 300%, got {cagr}");
        assert!(metrics.annualised_is_meaningful());
        assert_eq!(
            metrics.calmar, None,
            "a curve that never fell has no drawdown to divide by"
        );
    }

    #[test]
    fn a_short_window_is_flagged_as_too_short_to_annualise() {
        let metrics = metrics(&[100.0, 101.0, 102.0]);
        assert!(!metrics.annualised_is_meaningful());
        assert_eq!(metrics.window_days(), 2);
    }

    #[test]
    fn repeated_observations_of_a_day_collapse_into_one() {
        let series = EquitySeries::new(vec![
            point(0, 100.0),
            EquityPoint {
                time_ms: DAY_MS / 2,
                wallet: 101.0,
                external_flow: 0.0,
            },
            point(1, 102.0),
        ]);

        assert_eq!(series.len(), 2, "one point per day");
        assert_eq!(series.points()[0].wallet, 101.0, "the latest balance wins");
    }

    #[test]
    fn merging_prefers_the_deposit_aware_source() {
        let mut local = EquitySeries::new(vec![point(0, 100.0), point(1, 105.0)]);
        let venue = EquitySeries::new(vec![EquityPoint {
            time_ms: DAY_MS,
            wallet: 105.0,
            external_flow: -5.0,
        }]);

        local.merge_preferring(venue);
        assert_eq!(local.len(), 2);
        assert_eq!(
            local.points()[1].external_flow,
            -5.0,
            "the venue's flow is kept"
        );
    }

    #[test]
    fn windows_cut_the_series_and_wrap_in_order() {
        let now = 400 * DAY_MS;
        let points: Vec<EquityPoint> = (0..400).map(|day| point(day, 100.0 + day as f64)).collect();
        let series = EquitySeries::new(points);

        assert_eq!(series.window(Window::Month, now).len(), 30);
        assert_eq!(series.window(Window::Quarter, now).len(), 90);
        assert_eq!(series.window(Window::Year, now).len(), 365);
        assert_eq!(series.window(Window::All, now).len(), 400);

        assert_eq!(Window::All.next(), Window::Month, "windows wrap");
        assert_eq!(Window::Month.previous(), Window::All);
        assert_eq!(Window::Month.label(), "1M");
        assert_eq!(Window::All.start_ms(now), None, "everything has no start");
        assert_eq!(Window::Month.start_ms(now), Some(now - 30 * DAY_MS));
    }

    #[test]
    fn a_zero_balance_is_skipped_rather_than_divided_by() {
        let metrics = metrics(&[0.0, 100.0, 110.0]);
        assert_eq!(
            metrics.samples, 1,
            "the return out of a zero balance is not a return"
        );
        assert!((metrics.total_return - 0.1).abs() < 1e-9);
    }

    #[test]
    fn durations_are_described_for_the_coverage_line() {
        assert_eq!(describe_duration(0), "under a day");
        assert_eq!(describe_duration(DAY_MS), "1 day");
        assert_eq!(describe_duration(5 * DAY_MS), "5 days");
        assert_eq!(describe_duration(100 * DAY_MS), "3.3 months");
        assert_eq!(describe_duration(-DAY_MS), "1 day", "sign is not the point");
    }
}
