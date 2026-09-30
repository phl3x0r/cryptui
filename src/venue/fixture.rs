//! An offline venue backed by a JSON file.
//!
//! Exists so account switching, the chart and the tables can be exercised
//! without a second live account. Fixtures are deliberately synthetic: no real
//! account data belongs in a repository, so the committed fixture is invented
//! rather than recorded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::venue::{
    AccountSnapshot, Interval, Kline, Position, Symbol, Venue, VenueError, VenueFuture, VenueId,
};

/// The shape of a fixture file.
#[derive(Debug, Deserialize)]
struct FixtureFile {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    symbols: Vec<Symbol>,
    account: AccountSnapshot,
    #[serde(default)]
    positions: Vec<Position>,
    /// Candles keyed by `SYMBOL|interval`, oldest first.
    #[serde(default)]
    klines: HashMap<String, Vec<Kline>>,
}

/// A venue that answers everything from a file.
#[derive(Debug)]
pub struct FixtureVenue {
    label: String,
    path: PathBuf,
    file: FixtureFile,
}

impl FixtureVenue {
    /// Read and parse a fixture.
    pub fn load(path: &Path) -> Result<Self, VenueError> {
        let failure = |detail: String| VenueError::Fixture {
            path: path.to_path_buf(),
            detail,
        };

        let raw = std::fs::read_to_string(path)
            .map_err(|error| failure(format!("cannot read the file: {error}")))?;
        let file: FixtureFile = serde_json::from_str(&raw)
            .map_err(|error| failure(format!("invalid fixture JSON: {error}")))?;

        let label = file
            .label
            .clone()
            .or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "fixture".to_owned());

        Ok(Self {
            label,
            path: path.to_path_buf(),
            file,
        })
    }

    /// Fixture path, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Name of this fixture, from the file or its file name.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Venue for FixtureVenue {
    fn id(&self) -> VenueId {
        VenueId::BinanceFutures
    }

    fn sync(&self) -> VenueFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn symbols(&self) -> VenueFuture<'_, Vec<Symbol>> {
        Box::pin(async move { Ok(self.file.symbols.clone()) })
    }

    fn positions(&self) -> VenueFuture<'_, Vec<Position>> {
        Box::pin(async move { Ok(self.file.positions.clone()) })
    }

    fn account(&self) -> VenueFuture<'_, AccountSnapshot> {
        Box::pin(async move { Ok(self.file.account.clone()) })
    }

    fn klines(&self, symbol: &str, interval: Interval, limit: u32) -> VenueFuture<'_, Vec<Kline>> {
        let key = format!("{}|{}", symbol.to_uppercase(), interval.as_str());
        let path = self.path.clone();

        Box::pin(async move {
            let candles = self
                .file
                .klines
                .get(&key)
                .ok_or_else(|| VenueError::Fixture {
                    path,
                    detail: format!("no candles for `{key}` in the fixture"),
                })?;

            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let start = candles.len().saturating_sub(limit);
            Ok(candles[start..].to_vec())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureVenue;
    use crate::venue::{Interval, Venue};

    const FIXTURE: &str = r#"{
      "label": "Paper",
      "symbols": [
        { "name": "BTCUSDT", "base_asset": "BTC", "quote_asset": "USDT" }
      ],
      "account": {
        "balances": [ { "asset": "USDT", "total": 1000.0, "available": 800.0 } ],
        "wallet_balance": 1000.0,
        "equity": 1010.0,
        "unrealized_pnl": 10.0,
        "available_balance": 800.0,
        "initial_margin": 200.0,
        "maintenance_margin": 10.0
      },
      "positions": [
        { "symbol": "BTCUSDT", "side": "long", "size": 0.5, "entry_price": 60000.0,
          "mark_price": 61000.0, "unrealized_pnl": 500.0, "initial_margin": 200.0,
          "maintenance_margin": 10.0, "notional": 30500.0, "liquidation_price": null }
      ],
      "klines": {
        "BTCUSDT|15m": [
          { "open_time_ms": 1790726400000, "open": 1.0, "high": 2.0, "low": 0.5,
            "close": 1.5, "volume": 10.0, "close_time_ms": 1790727299999, "closed": true },
          { "open_time_ms": 1790727300000, "open": 1.5, "high": 2.5, "low": 1.2,
            "close": 2.0, "volume": 12.0, "close_time_ms": 1790728199999, "closed": false }
        ]
      }
    }"#;

    fn write_temp(label: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cryptui-fixture-{label}-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("temp fixture is writable");
        path
    }

    #[tokio::test]
    async fn serves_the_recorded_account() {
        let path = write_temp("ok", FIXTURE);
        let venue = FixtureVenue::load(&path).expect("fixture loads");

        let positions = venue.positions().await.expect("positions");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTCUSDT");

        let account = venue.account().await.expect("account");
        assert_eq!(account.equity, 1010.0);
        assert_eq!(account.balances.len(), 1);

        let symbols = venue.symbols().await.expect("symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(venue.id(), crate::venue::VenueId::BinanceFutures);
    }

    #[tokio::test]
    async fn returns_the_newest_candles_up_to_the_limit() {
        let path = write_temp("klines", FIXTURE);
        let venue = FixtureVenue::load(&path).expect("fixture loads");

        let all = venue
            .klines("btcusdt", Interval::M15, 500)
            .await
            .expect("candles");
        assert_eq!(all.len(), 2, "the symbol is upper-cased before lookup");
        assert!(!all[1].closed, "the forming candle is preserved");

        let limited = venue
            .klines("BTCUSDT", Interval::M15, 1)
            .await
            .expect("candles");
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].close, 2.0, "the newest candle is kept");
    }

    #[tokio::test]
    async fn a_missing_key_is_an_explicit_error() {
        let path = write_temp("missing", FIXTURE);
        let venue = FixtureVenue::load(&path).expect("fixture loads");

        let error = venue
            .klines("BTCUSDT", Interval::H1, 10)
            .await
            .expect_err("no hourly candles in the fixture");
        let message = error.to_string();
        assert!(message.contains("BTCUSDT|1h"), "got: {message}");
        assert!(message.contains("fixture"), "got: {message}");
    }

    #[tokio::test]
    async fn the_committed_fixture_stays_usable() {
        // Guards the file the config template points at.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/account_paper.json");
        let venue = FixtureVenue::load(&path).expect("committed fixture loads");

        let positions = venue.positions().await.expect("positions");
        assert!(positions.len() >= 3, "the fixture has a few positions");
        assert!(
            positions
                .iter()
                .any(|p| p.side == crate::venue::PositionSide::Short),
            "a short is included so sign handling is exercised"
        );

        for position in &positions {
            let candles = venue
                .klines(&position.symbol, Interval::M15, 500)
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", position.symbol));
            assert!(
                candles.len() >= 100,
                "{} needs enough history to render",
                position.symbol
            );
        }

        let account = venue.account().await.expect("account");
        assert!(account.equity > 0.0);
        assert_eq!(venue.label(), "Paper");
    }

    #[test]
    fn a_missing_file_reports_the_path() {
        let path = std::path::Path::new("/nonexistent/paper.json");
        let error = FixtureVenue::load(path).expect_err("file does not exist");
        assert!(error.to_string().contains("/nonexistent/paper.json"));
    }

    #[test]
    fn invalid_json_reports_the_reason() {
        let path = write_temp("invalid", "{ not json");
        let error = FixtureVenue::load(&path).expect_err("invalid fixture");
        assert!(error.to_string().contains("invalid fixture JSON"));
    }

    #[test]
    fn the_label_falls_back_to_the_file_name() {
        let path = write_temp("nameless", &FIXTURE.replace(r#""label": "Paper","#, ""));
        let venue = FixtureVenue::load(&path).expect("fixture loads");
        assert!(venue.label.starts_with("cryptui-fixture-nameless"));
    }
}
