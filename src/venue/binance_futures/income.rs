//! Rebuilding a wallet-balance history from income records.
//!
//! The venue exposes no equity history, but it does expose every balance change
//! as an income record, and the current balance is known. Walking backwards from
//! today's balance therefore reconstructs what the account was worth at the end
//! of each day:
//!
//! ```text
//! wallet(end of day) = wallet(now) − Σ income after that day
//! ```
//!
//! Deposits and withdrawals are income records too, so they are part of the
//! balance walk but are also reported per day so returns can exclude them.

use crate::performance::{DAY_MS, EquityPoint, EquitySeries};

/// The income type that means money entered or left the account.
pub(crate) const TRANSFER: &str = "TRANSFER";

/// One balance change reported by the venue.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IncomeRecord {
    /// When it happened.
    pub(crate) time_ms: i64,
    /// The venue's classification, for example `REALIZED_PNL` or `COMMISSION`.
    pub(crate) income_type: String,
    /// Signed amount: positive credits the balance, negative debits it.
    pub(crate) amount: f64,
}

/// Reconstruct daily wallet balances from income records.
///
/// The series starts at the oldest record rather than at whatever was asked
/// for: inventing a flat line for a period the venue has no data for would be a
/// lie the metrics would then report on.
pub(crate) fn wallet_series(
    records: &[IncomeRecord],
    wallet_now: f64,
    now_ms: i64,
) -> EquitySeries {
    let Some(oldest) = records.iter().map(|record| record.time_ms).min() else {
        return EquitySeries::default();
    };

    let mut sorted: Vec<&IncomeRecord> = records.iter().collect();
    sorted.sort_by_key(|record| record.time_ms);

    let start_day = oldest.div_euclid(DAY_MS);
    let end_day = now_ms.div_euclid(DAY_MS);

    let mut points = Vec::with_capacity((end_day - start_day + 1) as usize);
    let mut after: f64 = 0.0;
    let mut cursor = sorted.len();

    // Newest day first, so each step only has to add the records that fall
    // between this day's end and the previous one's.
    for day in (start_day..=end_day).rev() {
        let end_of_day = (day + 1) * DAY_MS;
        while cursor > 0 && sorted[cursor - 1].time_ms >= end_of_day {
            cursor -= 1;
            after += sorted[cursor].amount;
        }

        let flow = sorted
            .iter()
            .filter(|record| {
                record.time_ms.div_euclid(DAY_MS) == day && record.income_type == TRANSFER
            })
            .map(|record| record.amount)
            .sum();

        points.push(EquityPoint {
            time_ms: (end_of_day - 1).min(now_ms),
            wallet: wallet_now - after,
            external_flow: flow,
        });
    }

    points.reverse();
    EquitySeries::new(points)
}

#[cfg(test)]
mod tests {
    use super::{IncomeRecord, TRANSFER, wallet_series};
    use crate::performance::DAY_MS;

    fn income(day: i64, kind: &str, amount: f64) -> IncomeRecord {
        IncomeRecord {
            time_ms: day * DAY_MS + 3_600_000,
            income_type: kind.to_owned(),
            amount,
        }
    }

    #[test]
    fn no_records_means_no_history() {
        assert!(wallet_series(&[], 1_000.0, 10 * DAY_MS).is_empty());
    }

    #[test]
    fn the_curve_is_anchored_on_the_current_balance() {
        // +100 profit on day 1, -10 fees on day 2, +500 deposited on day 3.
        let records = vec![
            income(1, "REALIZED_PNL", 100.0),
            income(2, "COMMISSION", -10.0),
            income(3, TRANSFER, 500.0),
        ];
        let series = wallet_series(&records, 1_090.0, 3 * DAY_MS + 7_200_000);

        let wallets: Vec<f64> = series.points().iter().map(|point| point.wallet).collect();
        assert_eq!(
            wallets,
            vec![500.0, 600.0, 590.0, 1_090.0],
            "each day's closing balance, oldest first"
        );
        assert_eq!(series.points().len(), 4, "one point per day, gaps included");
    }

    #[test]
    fn a_deposit_is_a_balance_change_but_not_a_return() {
        let records = vec![income(3, TRANSFER, 500.0)];
        let series = wallet_series(&records, 1_090.0, 3 * DAY_MS);

        assert_eq!(series.points()[0].wallet, 590.0);
        assert_eq!(
            series.points()[3].external_flow,
            500.0,
            "the flow is reported for its day"
        );

        let returns = series.daily_returns();
        assert_eq!(returns.len(), 3);
        assert!(
            returns.iter().all(|value| value.abs() < 1e-12),
            "paying money in is not performance: {returns:?}"
        );
    }

    #[test]
    fn fees_and_profit_show_up_as_returns() {
        let records = vec![
            income(1, "REALIZED_PNL", 100.0),
            income(2, "FUNDING_FEE", -10.0),
        ];
        let series = wallet_series(&records, 1_090.0, 2 * DAY_MS);
        let returns = series.daily_returns();

        // Day 1: 500 -> 600 is +20%. Day 2: 600 -> 590 is -1.67%.
        assert!((returns[0] - 0.2).abs() < 1e-12, "{returns:?}");
        assert!((returns[1] + 10.0 / 600.0).abs() < 1e-12, "{returns:?}");

        let metrics = series.metrics().expect("two returns");
        assert!(
            (metrics.max_drawdown - (10.0 / 600.0) / 1.2).abs() < 1e-12,
            "the fee is the drawdown: {metrics:?}"
        );
    }

    #[test]
    fn a_day_without_records_carries_the_balance_forward() {
        let records = vec![
            income(0, "REALIZED_PNL", 50.0),
            income(4, "COMMISSION", -5.0),
        ];
        let series = wallet_series(&records, 1_045.0, 4 * DAY_MS);

        let wallets: Vec<f64> = series.points().iter().map(|point| point.wallet).collect();
        assert_eq!(wallets, vec![1_000.0, 1_050.0, 1_050.0, 1_050.0, 1_045.0]);
    }

    #[test]
    fn the_newest_point_is_now_rather_than_a_future_midnight() {
        let records = vec![income(0, "REALIZED_PNL", 1.0)];
        let now = 2 * DAY_MS + 5_000;
        let series = wallet_series(&records, 101.0, now);

        assert_eq!(
            series.points().last().map(|point| point.time_ms),
            Some(now),
            "the curve ends at the balance we know"
        );
    }
}
