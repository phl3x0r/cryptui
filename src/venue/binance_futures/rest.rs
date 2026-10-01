//! Endpoint methods for the Binance USDⓈ-M futures client.
//!
//! These are thin: request shape here, payload interpretation in [`super::wire`].

use crate::auth::{Query, now_ms};

use super::BinanceFutures;
use super::income;
use super::wire;
use super::ws;
use crate::performance::{DAY_MS, EquitySeries};
use crate::venue::{
    AccountSnapshot, Interval, Kline, Position, StreamEvent, Symbol, UnboundedSender, Venue,
    VenueError, VenueFuture, VenueId,
};

/// Highest candle count the venue will return in one klines request.
const MAX_KLINES: u32 = 1_500;

/// Most income pages fetched for one equity curve.
///
/// Each page costs 30 weight, so a bounded fetch keeps opening the panel from
/// spending the whole minute's budget. On this account a page covers about a
/// week, which is far short of a year — the rest comes from local recording.
const MAX_INCOME_PAGES: usize = 12;
/// Records per income page: the venue's maximum.
const INCOME_PAGE_SIZE: u32 = 1_000;
/// The venue will not serve income older than this.
const INCOME_HISTORY_DAYS: i64 = 90;

/// One page of income, ending at `end`.
///
/// Only `endTime` is sent, and that is the whole point: when a request carries a
/// `startTime` the venue answers with the *oldest* records in the window and
/// drops the newest at the page limit. Asking for `[floor, now]` therefore
/// returned the first thousand records and none of the days since — a profitable
/// day vanished, and walking back from today's balance credited it to the first
/// day instead. Bounding the page from above and walking backwards is the shape
/// that keeps the recent end of the curve exact.
fn income_query(end: i64) -> Query {
    let mut query = Query::new();
    query
        .push("endTime", end.to_string())
        .push("limit", INCOME_PAGE_SIZE.to_string());
    query
}

/// Collect income backwards from `now`, stopping at the window floor or the page
/// budget.
///
/// Returns the records, newest last, and whether the page budget rather than the
/// venue's history ended the walk.
async fn collect_income<F, Fut>(
    now: i64,
    floor: i64,
    fetch: F,
) -> Result<(Vec<income::IncomeRecord>, bool), VenueError>
where
    F: FnMut(i64) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<income::IncomeRecord>, VenueError>>,
{
    let mut records = Vec::new();
    let mut end = now;
    let mut truncated = false;
    let mut fetch = fetch;

    for page_index in 0..MAX_INCOME_PAGES {
        if end <= floor {
            break;
        }

        let page = fetch(end).await?;
        let Some(oldest) = page.first().map(|record| record.time_ms) else {
            break; // nothing older to fetch
        };
        let full_page = page.len() >= INCOME_PAGE_SIZE as usize;
        records.extend(page);

        if !full_page {
            break; // the venue has no more history to give
        }
        if page_index + 1 == MAX_INCOME_PAGES {
            truncated = true; // the budget ran out, not the records
            break;
        }
        end = oldest - 1;
    }

    Ok((records, truncated))
}

impl Venue for BinanceFutures {
    fn id(&self) -> VenueId {
        VenueId::BinanceFutures
    }

    fn sync(&self) -> VenueFuture<'_, ()> {
        Box::pin(async move { self.sync_clock().await })
    }

    fn symbols(&self) -> VenueFuture<'_, Vec<Symbol>> {
        Box::pin(async move {
            let payload = self.get_json("/fapi/v1/exchangeInfo", None, false).await?;
            wire::symbols(payload).map_err(|detail| self.malformed(detail))
        })
    }

    fn positions(&self) -> VenueFuture<'_, Vec<Position>> {
        Box::pin(async move {
            let payload = self.get_json("/fapi/v3/positionRisk", None, true).await?;
            wire::positions(payload).map_err(|detail| self.malformed(detail))
        })
    }

    fn account(&self) -> VenueFuture<'_, AccountSnapshot> {
        Box::pin(async move {
            let payload = self.get_json("/fapi/v3/account", None, true).await?;
            let mut snapshot = wire::account(payload).map_err(|detail| self.malformed(detail))?;

            // The mode is not part of the account payload, and a monitor that
            // knows the balances but not the mode is still useful, so a failure
            // here leaves it unknown rather than failing the whole snapshot.
            match self
                .get_json("/fapi/v1/multiAssetsMargin", None, true)
                .await
            {
                Ok(payload) => match wire::multi_assets(&payload) {
                    Ok(multi_assets) => snapshot.multi_assets = Some(multi_assets),
                    Err(detail) => tracing::debug!(%detail, "could not read the account mode"),
                },
                Err(error) => tracing::debug!(%error, "could not read the account mode"),
            }

            Ok(snapshot)
        })
    }

    fn klines(&self, symbol: &str, interval: Interval, limit: u32) -> VenueFuture<'_, Vec<Kline>> {
        let symbol = symbol.to_uppercase();
        let limit = limit.clamp(1, MAX_KLINES);

        Box::pin(async move {
            let mut query = Query::new();
            query
                .push("symbol", symbol)
                .push("interval", interval.as_str())
                .push("limit", limit.to_string());

            let payload = self
                .get_json("/fapi/v1/klines", Some(&query), false)
                .await?;
            wire::klines(&payload, now_ms()).map_err(|detail| self.malformed(detail))
        })
    }

    fn equity_history(&self, since_ms: i64) -> VenueFuture<'_, EquitySeries> {
        Box::pin(async move {
            let now = now_ms();
            let floor = since_ms.max(now - INCOME_HISTORY_DAYS * DAY_MS);

            // The current balance is the anchor everything is walked back from.
            let account = self.get_json("/fapi/v3/account", None, true).await?;
            let wallet_now =
                wire::wallet_balance(&account).map_err(|detail| self.malformed(detail))?;

            let (records, truncated) = collect_income(now, floor, |end| async move {
                let payload = self
                    .get_json("/fapi/v1/income", Some(&income_query(end)), true)
                    .await?;
                wire::income(payload).map_err(|detail| self.malformed(detail))
            })
            .await?;

            let records = income::drop_incomplete_oldest_day(records, truncated);
            let assets = income::assets(&records);
            tracing::debug!(
                records = records.len(),
                truncated,
                assets = %assets.join(", "),
                from = %floor,
                "reconstructed the wallet history from income"
            );
            Ok(income::wallet_series(&records, wallet_now, now).with_assets(assets))
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn follow_klines(
        &self,
        symbol: &str,
        interval: Interval,
        updates: UnboundedSender<StreamEvent<Kline>>,
    ) -> VenueFuture<'_, ()> {
        let symbol = symbol.to_uppercase();
        Box::pin(async move {
            ws::follow_klines(self.stream_base, &symbol, interval, updates).await;
            Ok(())
        })
    }

    fn follow_marks(
        &self,
        symbols: Vec<String>,
        updates: UnboundedSender<StreamEvent<(String, f64)>>,
    ) -> VenueFuture<'_, ()> {
        Box::pin(async move {
            ws::follow_marks(self.stream_base, &symbols, updates).await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{INCOME_PAGE_SIZE, MAX_INCOME_PAGES, collect_income, income_query};
    use crate::performance::DAY_MS;
    use crate::venue::VenueError;
    use crate::venue::binance_futures::income::IncomeRecord;

    const NOW: i64 = 100 * DAY_MS;

    fn record(time_ms: i64, amount: f64) -> IncomeRecord {
        IncomeRecord {
            time_ms,
            income_type: "REALIZED_PNL".to_owned(),
            amount,
            asset: "USDC".to_owned(),
        }
    }

    /// The venue's paging rule, as measured against the live API.
    ///
    /// With a `startTime` it answers oldest-first from that time, so a full page
    /// silently drops everything after it; with `endTime` alone it answers with
    /// the newest page at or before that time. Requests are recorded so a test
    /// can assert the shape that was sent.
    fn venue(
        records: &[IncomeRecord],
        end: i64,
        start: Option<i64>,
        asked: &mut Vec<(Option<i64>, i64)>,
    ) -> Vec<IncomeRecord> {
        asked.push((start, end));
        let mut inside: Vec<IncomeRecord> = records
            .iter()
            .filter(|record| record.time_ms <= end && start.is_none_or(|s| record.time_ms >= s))
            .cloned()
            .collect();

        if start.is_some() {
            inside.truncate(INCOME_PAGE_SIZE as usize); // oldest first
        } else {
            let keep = inside.len().saturating_sub(INCOME_PAGE_SIZE as usize);
            inside.drain(..keep); // newest page
        }
        inside
    }

    /// A busy account: more records in the window than one page can hold.
    fn busy_account() -> Vec<IncomeRecord> {
        let mut records = Vec::new();
        for index in 0..2_500 {
            let day = 91 + index / 300; // nine days of heavy funding and trading
            records.push(record(day * DAY_MS + (index % 300) * 60_000, 1.0));
        }
        records
    }

    #[tokio::test]
    async fn the_walk_reaches_today_and_never_loses_the_newest_records() {
        let records = busy_account();
        let newest = records.last().expect("records").time_ms;
        let mut asked = Vec::new();

        let (collected, truncated) = collect_income(NOW, NOW - 90 * DAY_MS, |end| {
            let page = venue(&records, end, None, &mut asked);
            async move { Ok::<_, VenueError>(page) }
        })
        .await
        .expect("a fetch");

        assert!(!truncated, "two and a half pages fit the budget");
        assert_eq!(
            collected.iter().map(|r| r.time_ms).max(),
            Some(newest),
            "the newest record is in the set"
        );
        assert_eq!(collected.len(), records.len(), "and so is everything else");
        assert!(
            asked.iter().all(|(start, _)| start.is_none()),
            "no request may carry a startTime: {asked:?}"
        );
    }

    #[tokio::test]
    async fn a_window_asked_for_the_old_way_would_have_missed_the_newest_day() {
        // The same window, both ways round, on the same records. This is the bug
        // that made a profitable account look flat.
        let records = busy_account();
        let floor = NOW - 90 * DAY_MS;
        let mut asked = Vec::new();

        let with_start = venue(&records, NOW, Some(floor), &mut asked);
        let newest_with_start = with_start.iter().map(|r| r.time_ms).max();

        let mut asked = Vec::new();
        let without_start = venue(&records, NOW, None, &mut asked);
        let newest_without_start = without_start.iter().map(|r| r.time_ms).max();

        assert!(
            newest_with_start < newest_without_start,
            "startTime drops the recent end"
        );
        assert!(
            newest_with_start.expect("a page") < records.last().expect("records").time_ms,
            "which is exactly how the newest profit went missing"
        );
    }

    #[tokio::test]
    async fn the_page_budget_bounds_a_pathological_account() {
        // Every page full: the walk has to stop on the budget and say so, so the
        // caller can drop the day whose earlier records it never saw.
        let mut asked = Vec::new();
        let (collected, truncated) = collect_income(NOW, NOW - 90 * DAY_MS, |end| {
            asked.push(end);
            let page: Vec<IncomeRecord> = (0..INCOME_PAGE_SIZE)
                .map(|index| record(end - index as i64 * 1_000, 1.0))
                .collect();
            async move { Ok::<_, VenueError>(page) }
        })
        .await
        .expect("a fetch");

        assert!(truncated, "the budget ended the walk");
        assert_eq!(asked.len(), MAX_INCOME_PAGES);
        assert_eq!(
            collected.len(),
            MAX_INCOME_PAGES * INCOME_PAGE_SIZE as usize
        );
    }

    #[tokio::test]
    async fn the_walk_stops_at_the_window_floor() {
        let mut asked = Vec::new();
        let (collected, truncated) = collect_income(NOW, NOW - DAY_MS, |end| {
            asked.push(end);
            let page = vec![record(end - 1, 1.0), record(end - 2, 1.0)];
            async move { Ok::<_, VenueError>(page) }
        })
        .await
        .expect("a fetch");

        assert!(!truncated);
        assert_eq!(asked.len(), 1, "the floor stops the walk: {asked:?}");
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn an_income_page_is_bounded_from_above_only() {
        let encoded = income_query(1_790_000_000_000).encode();
        assert!(encoded.contains("endTime=1790000000000"), "{encoded}");
        assert!(encoded.contains("limit=1000"), "{encoded}");
        assert!(
            !encoded.contains("startTime"),
            "a startTime makes the venue answer oldest-first: {encoded}"
        );
    }
}
