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
    VenueFuture, VenueId,
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
            wire::account(payload).map_err(|detail| self.malformed(detail))
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

            let mut records = Vec::new();
            let mut end = now;
            for _ in 0..MAX_INCOME_PAGES {
                if end <= floor {
                    break;
                }

                let mut query = Query::new();
                query
                    .push("startTime", floor.to_string())
                    .push("endTime", end.to_string())
                    .push("limit", INCOME_PAGE_SIZE.to_string());

                let payload = self.get_json("/fapi/v1/income", Some(&query), true).await?;
                let page = wire::income(payload).map_err(|detail| self.malformed(detail))?;
                let Some(oldest) = page.first().map(|record| record.time_ms) else {
                    break;
                };
                let full_page = page.len() >= INCOME_PAGE_SIZE as usize;
                records.extend(page);

                if !full_page {
                    break; // the window is exhausted
                }
                end = oldest - 1;
            }

            tracing::debug!(
                records = records.len(),
                from = %floor,
                "reconstructed the wallet history from income"
            );
            Ok(income::wallet_series(&records, wallet_now, now))
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
