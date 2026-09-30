//! Wire formats for the Binance USDⓈ-M futures REST API.
//!
//! Every venue quirk lives here so the rest of the application only ever sees
//! the domain types from [`crate::venue`]. The venue encodes decimals as either
//! JSON strings or numbers depending on the endpoint, hence [`Num`].

use serde::Deserialize;
use serde_json::Value;

use crate::venue::binance_futures::income::IncomeRecord;
use crate::venue::{AccountSnapshot, Balance, Kline, Position, PositionSide, Symbol};

/// Error body returned for failed requests.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    pub(crate) code: i64,
    pub(crate) msg: String,
}

/// A decimal that may arrive as a JSON string or a number.
#[derive(Debug, Clone)]
pub(crate) struct Num(pub(crate) String);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Text(String),
        }

        Ok(match Raw::deserialize(deserializer)? {
            Raw::Number(number) => Self(number.to_string()),
            Raw::Text(text) => Self(text),
        })
    }
}

impl Num {
    /// Parse as a float; an empty value counts as zero.
    fn f64(&self) -> Result<f64, String> {
        let trimmed = self.0.trim();
        if trimmed.is_empty() {
            return Ok(0.0);
        }
        trimmed
            .parse::<f64>()
            .map_err(|error| format!("`{trimmed}` is not a number: {error}"))
    }
}

/// `GET /fapi/v1/exchangeInfo`
#[derive(Debug, Deserialize)]
pub(crate) struct ExchangeInfo {
    symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SymbolInfo {
    symbol: String,
    status: String,
    base_asset: String,
    quote_asset: String,
    contract_type: String,
}

/// `GET /fapi/v1/income` row.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IncomeRow {
    time: i64,
    income_type: String,
    income: Num,
    /// The asset the venue credited, for example `BNFCR` or `USDC`. Absent on
    /// records the venue predates, which are treated as unknown.
    #[serde(default)]
    asset: String,
}

/// Balance changes, oldest first.
pub(crate) fn income(payload: Value) -> Result<Vec<IncomeRecord>, String> {
    let mut rows: Vec<IncomeRow> =
        serde_json::from_value(payload).map_err(|error| format!("income: {error}"))?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        records.push(IncomeRecord {
            time_ms: row.time,
            income_type: row.income_type,
            amount: row.income.f64()?,
            asset: row.asset,
        });
    }
    records.sort_by_key(|record| record.time_ms);
    Ok(records)
}

/// The wallet balance from an account payload: the anchor for a reconstructed
/// curve, and what the local recorder stores each day.
pub(crate) fn wallet_balance(payload: &Value) -> Result<f64, String> {
    account(payload.clone()).map(|snapshot| snapshot.wallet_balance)
}

/// `GET /fapi/v3/positionRisk`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PositionRiskRow {
    symbol: String,
    position_amt: Num,
    entry_price: Num,
    mark_price: Num,
    un_realized_profit: Num,
    position_side: String,
    initial_margin: Num,
    maint_margin: Num,
    notional: Num,
    liquidation_price: Num,
}

/// `GET /fapi/v3/account`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountRow {
    total_wallet_balance: Num,
    total_unrealized_profit: Num,
    total_margin_balance: Num,
    available_balance: Num,
    total_initial_margin: Num,
    total_maint_margin: Num,
    assets: Vec<AssetRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetRow {
    asset: String,
    wallet_balance: Num,
    available_balance: Num,
}

/// Tradable perpetual contracts quoted in USDT, ordered by symbol.
pub(crate) fn symbols(payload: Value) -> Result<Vec<Symbol>, String> {
    let info: ExchangeInfo =
        serde_json::from_value(payload).map_err(|error| format!("exchangeInfo: {error}"))?;

    let mut symbols: Vec<Symbol> = info
        .symbols
        .into_iter()
        .filter(|entry| {
            entry.status == "TRADING"
                && entry.contract_type == "PERPETUAL"
                && entry.quote_asset == "USDT"
        })
        .map(|entry| Symbol {
            name: entry.symbol,
            base_asset: entry.base_asset,
            quote_asset: entry.quote_asset,
        })
        .collect();
    symbols.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(symbols)
}

/// Open positions, flat contracts omitted, ordered by symbol.
pub(crate) fn positions(payload: Value) -> Result<Vec<Position>, String> {
    let rows: Vec<PositionRiskRow> =
        serde_json::from_value(payload).map_err(|error| format!("positionRisk: {error}"))?;

    let mut positions = Vec::with_capacity(rows.len());
    for row in rows {
        let signed_size = row.position_amt.f64()?;
        if signed_size == 0.0 {
            // v3 already filters these, but a flat row must never reach the UI.
            continue;
        }

        // One-way mode reports `BOTH` and encodes direction in the sign;
        // hedge mode reports LONG/SHORT explicitly.
        let side = match row.position_side.as_str() {
            "LONG" => PositionSide::Long,
            "SHORT" => PositionSide::Short,
            _ if signed_size < 0.0 => PositionSide::Short,
            _ => PositionSide::Long,
        };

        let liquidation_price = row.liquidation_price.f64()?;
        positions.push(Position {
            symbol: row.symbol,
            side,
            size: signed_size.abs(),
            entry_price: row.entry_price.f64()?,
            mark_price: row.mark_price.f64()?,
            unrealized_pnl: row.un_realized_profit.f64()?,
            initial_margin: row.initial_margin.f64()?,
            maintenance_margin: row.maint_margin.f64()?,
            notional: row.notional.f64()?,
            liquidation_price: (liquidation_price > 0.0).then_some(liquidation_price),
        });
    }

    positions.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    Ok(positions)
}

/// Account totals and non-empty asset balances.
pub(crate) fn account(payload: Value) -> Result<AccountSnapshot, String> {
    let row: AccountRow =
        serde_json::from_value(payload).map_err(|error| format!("account: {error}"))?;

    let mut balances = Vec::new();
    for asset in row.assets {
        let total = asset.wallet_balance.f64()?;
        if total == 0.0 {
            continue;
        }
        balances.push(Balance {
            asset: asset.asset,
            total,
            available: asset.available_balance.f64()?,
        });
    }
    balances.sort_by(|left, right| left.asset.cmp(&right.asset));

    Ok(AccountSnapshot {
        balances,
        wallet_balance: row.total_wallet_balance.f64()?,
        equity: row.total_margin_balance.f64()?,
        unrealized_pnl: row.total_unrealized_profit.f64()?,
        available_balance: row.available_balance.f64()?,
        initial_margin: row.total_initial_margin.f64()?,
        maintenance_margin: row.total_maint_margin.f64()?,
    })
}

/// Candle array rows: `[openTime, open, high, low, close, volume, closeTime, …]`.
pub(crate) fn klines(payload: &Value, now_ms: i64) -> Result<Vec<Kline>, String> {
    let rows = payload
        .as_array()
        .ok_or_else(|| "expected an array of candles".to_owned())?;

    let mut klines = Vec::with_capacity(rows.len());
    for row in rows {
        let cells = row
            .as_array()
            .ok_or_else(|| "expected each candle to be an array".to_owned())?;
        let cell = |index: usize| -> Result<&Value, String> {
            cells
                .get(index)
                .ok_or_else(|| format!("candle is missing column {index}"))
        };

        let open_time_ms = parse_i64(cell(0)?, "open time")?;
        let close_time_ms = parse_i64(cell(6)?, "close time")?;

        klines.push(Kline {
            open_time_ms,
            open: parse_f64(cell(1)?, "open")?,
            high: parse_f64(cell(2)?, "high")?,
            low: parse_f64(cell(3)?, "low")?,
            close: parse_f64(cell(4)?, "close")?,
            volume: parse_f64(cell(5)?, "volume")?,
            close_time_ms,
            closed: now_ms > close_time_ms,
        });
    }

    Ok(klines)
}

/// Read a JSON number or numeric string as `f64`.
fn parse_f64(value: &Value, what: &str) -> Result<f64, String> {
    if let Some(number) = value.as_f64() {
        return Ok(number);
    }
    if let Some(text) = value.as_str() {
        return text
            .trim()
            .parse::<f64>()
            .map_err(|error| format!("{what}: `{text}` is not a number: {error}"));
    }
    Err(format!("{what}: expected a number, found {value}"))
}

/// Read a JSON integer or numeric string as `i64`.
fn parse_i64(value: &Value, what: &str) -> Result<i64, String> {
    if let Some(number) = value.as_i64() {
        return Ok(number);
    }
    if let Some(text) = value.as_str() {
        return text
            .trim()
            .parse::<i64>()
            .map_err(|error| format!("{what}: `{text}` is not an integer: {error}"));
    }
    Err(format!("{what}: expected an integer, found {value}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{account, klines, positions, symbols};
    use crate::venue::PositionSide;

    #[test]
    fn keeps_only_tradable_perpetuals() {
        let payload = json!({
            "symbols": [
                { "symbol": "BTCUSDT", "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "USDT", "contractType": "PERPETUAL" },
                { "symbol": "ETHUSDT_260327", "status": "TRADING", "baseAsset": "ETH", "quoteAsset": "USDT", "contractType": "CURRENT_QUARTER" },
                { "symbol": "DELISTEDUSDT", "status": "SETTLING", "baseAsset": "DELISTED", "quoteAsset": "USDT", "contractType": "PERPETUAL" },
                { "symbol": "BTCEUR", "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "EUR", "contractType": "PERPETUAL" }
            ]
        });

        let symbols = symbols(payload).expect("payload parses");
        assert_eq!(symbols.len(), 1, "only the USDT perpetual is tradable");
        assert_eq!(symbols[0].name, "BTCUSDT");
        assert_eq!(symbols[0].base_asset, "BTC");
    }

    #[test]
    fn reads_direction_from_the_sign_in_one_way_mode() {
        let payload = json!([
            { "symbol": "BTCUSDT", "positionAmt": "-0.5", "entryPrice": "60000", "markPrice": "61000",
              "unRealizedProfit": "-500", "positionSide": "BOTH", "initialMargin": "6100",
              "maintMargin": "305", "notional": "30500", "liquidationPrice": "0" },
            { "symbol": "ETHUSDT", "positionAmt": "2", "entryPrice": "3000", "markPrice": "3100",
              "unRealizedProfit": "200", "positionSide": "BOTH", "initialMargin": "6200",
              "maintMargin": "310", "notional": "6200", "liquidationPrice": "2500" }
        ]);

        let positions = positions(payload).expect("payload parses");
        assert_eq!(positions.len(), 2);

        let btc = &positions[0];
        assert_eq!(btc.side, PositionSide::Short);
        assert_eq!(btc.size, 0.5, "size is the absolute amount");
        assert_eq!(btc.unrealized_pnl, -500.0);
        assert_eq!(btc.initial_margin, 6100.0);
        assert_eq!(
            btc.liquidation_price, None,
            "a zero liquidation price means unset"
        );

        assert_eq!(positions[1].side, PositionSide::Long);
        assert_eq!(positions[1].liquidation_price, Some(2500.0));
    }

    #[test]
    fn accepts_hedge_mode_sides_and_drops_flat_rows() {
        let payload = json!([
            { "symbol": "BTCUSDT", "positionAmt": "1", "entryPrice": "1", "markPrice": "1",
              "unRealizedProfit": "0", "positionSide": "SHORT", "initialMargin": "1",
              "maintMargin": "0", "notional": "1", "liquidationPrice": "0" },
            { "symbol": "FLATUSDT", "positionAmt": "0.000", "entryPrice": "0", "markPrice": "0",
              "unRealizedProfit": "0", "positionSide": "BOTH", "initialMargin": "0",
              "maintMargin": "0", "notional": "0", "liquidationPrice": "0" }
        ]);

        let positions = positions(payload).expect("payload parses");
        assert_eq!(positions.len(), 1, "flat rows never reach the UI");
        assert_eq!(positions[0].side, PositionSide::Short);
    }

    #[test]
    fn reads_account_totals_and_skips_empty_assets() {
        let payload = json!({
            "totalWalletBalance": "1204.53",
            "totalUnrealizedProfit": "85.14",
            "totalMarginBalance": "1289.67",
            "availableBalance": "1063.21",
            "totalInitialMargin": "226.46",
            "totalMaintMargin": "11.32",
            "assets": [
                { "asset": "USDT", "walletBalance": "1204.53", "availableBalance": "1063.21" },
                { "asset": "BNB", "walletBalance": "0.00000000", "availableBalance": "0.00000000" }
            ]
        });

        let snapshot = account(payload).expect("payload parses");
        assert_eq!(snapshot.equity, 1289.67);
        assert_eq!(snapshot.balances.len(), 1, "empty assets are omitted");
        assert_eq!(snapshot.balances[0].asset, "USDT");
    }

    #[test]
    fn flags_the_in_progress_candle() {
        let payload = json!([
            [
                1790773200000i64,
                "85259.80",
                "85632.70",
                "85071.20",
                "85357.30",
                "8559.056",
                1790774099999i64,
                "0",
                1,
                "0",
                "0",
                "0"
            ],
            [
                1790774100000i64,
                "85357.30",
                "85500.00",
                "85300.00",
                "85450.00",
                "1200.5",
                1790774999999i64,
                "0",
                1,
                "0",
                "0",
                "0"
            ]
        ]);

        let klines = klines(&payload, 1790774500000).expect("payload parses");
        assert_eq!(klines.len(), 2);
        assert_eq!(klines[0].close, 85357.30);
        assert!(klines[0].closed, "the first candle has finished");
        assert!(!klines[1].closed, "the second candle is still forming");
        assert_eq!(klines[0].close_time_ms, 1790774099999);
    }

    #[test]
    fn rejects_a_payload_that_is_not_an_array_of_candles() {
        let error = klines(&json!({ "not": "candles" }), 0).unwrap_err();
        assert!(error.contains("array"), "got: {error}");
    }
}
