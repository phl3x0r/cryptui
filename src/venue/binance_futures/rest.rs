//! Endpoint methods for the Binance USDⓈ-M futures client.
//!
//! These are thin: request shape here, payload interpretation in [`super::wire`].

use crate::auth::{Query, now_ms};

use super::BinanceFutures;
use super::wire;
use crate::venue::{
    AccountSnapshot, Interval, Kline, Position, Symbol, Venue, VenueFuture, VenueId,
};

/// Highest candle count the venue will return in one klines request.
const MAX_KLINES: u32 = 1_500;

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
}
