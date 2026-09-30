//! Exchange abstraction shared by the configuration and the venue clients.
//!
//! Nothing outside this module may hard-code a venue string: the application
//! talks to exchanges through [`Venue`] and the domain types defined here.

pub mod binance_futures;

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use serde::Deserialize;

/// Which exchange and market an account talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueId {
    /// Binance USDⓈ-M futures (`fapi` endpoints).
    BinanceFutures,
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BinanceFutures => "binance_futures",
        };
        f.write_str(name)
    }
}

/// A candlestick interval offered by the chart and accepted by the venues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Interval {
    /// One minute.
    M1,
    /// Five minutes.
    M5,
    /// Fifteen minutes.
    M15,
    /// One hour.
    H1,
    /// Four hours.
    H4,
    /// One day.
    D1,
}

impl Interval {
    /// Every supported interval, in ascending order.
    pub const ALL: [Self; 6] = [Self::M1, Self::M5, Self::M15, Self::H1, Self::H4, Self::D1];

    /// The venue's wire representation, for example `15m`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H4 => "4h",
            Self::D1 => "1d",
        }
    }

    /// The next interval in [`Interval::ALL`], wrapping around at the end.
    pub fn next(self) -> Self {
        match Self::ALL.iter().position(|candidate| *candidate == self) {
            Some(index) => Self::ALL[(index + 1) % Self::ALL.len()],
            None => Self::ALL[0],
        }
    }

    /// The previous interval in [`Interval::ALL`], wrapping around at the start.
    pub fn previous(self) -> Self {
        match Self::ALL.iter().position(|candidate| *candidate == self) {
            Some(0) | None => Self::ALL[Self::ALL.len() - 1],
            Some(index) => Self::ALL[index - 1],
        }
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Interval {
    type Err = IntervalParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|interval| interval.as_str() == input)
            .ok_or_else(|| IntervalParseError {
                input: input.to_owned(),
            })
    }
}

impl TryFrom<String> for Interval {
    type Error = IntervalParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.parse() {
            Ok(interval) => Ok(interval),
            Err(_) => Err(IntervalParseError { input: value }),
        }
    }
}

/// Error returned when a string is not a supported candle interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalParseError {
    input: String,
}

impl fmt::Display for IntervalParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported candle interval `{}` (expected one of {})",
            self.input,
            Interval::ALL
                .iter()
                .map(|interval| interval.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for IntervalParseError {}

/// A tradable contract as advertised by the venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Venue symbol, for example `BTCUSDT`.
    pub name: String,
    /// Base asset, for example `BTC`.
    pub base_asset: String,
    /// Quote asset, for example `USDT`.
    pub quote_asset: String,
}

/// One candle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kline {
    /// Opening time, in milliseconds since the UNIX epoch.
    pub open_time_ms: i64,
    /// Price at the start of the interval.
    pub open: f64,
    /// Highest price in the interval.
    pub high: f64,
    /// Lowest price in the interval.
    pub low: f64,
    /// Price at the end of the interval.
    pub close: f64,
    /// Traded contract volume.
    pub volume: f64,
    /// Closing time, in milliseconds since the UNIX epoch.
    pub close_time_ms: i64,
    /// Whether the venue has finished this candle.
    pub closed: bool,
}

/// Direction of a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    /// Long.
    Long,
    /// Short.
    Short,
}

impl fmt::Display for PositionSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Long => "Long",
            Self::Short => "Short",
        })
    }
}

/// An open position.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    /// Contract, for example `BTCUSDT`.
    pub symbol: String,
    /// Direction.
    pub side: PositionSide,
    /// Absolute contract quantity.
    pub size: f64,
    /// Average entry price.
    pub entry_price: f64,
    /// Current mark price.
    pub mark_price: f64,
    /// Unrealized profit and loss in the margin asset.
    pub unrealized_pnl: f64,
    /// Margin committed at the current leverage.
    pub initial_margin: f64,
    /// Maintenance margin the venue requires.
    pub maintenance_margin: f64,
    /// Position value at the mark price.
    pub notional: f64,
    /// Liquidation price, when the venue reports one.
    pub liquidation_price: Option<f64>,
}

/// One asset balance.
#[derive(Debug, Clone, PartialEq)]
pub struct Balance {
    /// Asset symbol, for example `USDT`.
    pub asset: String,
    /// Total wallet balance for this asset.
    pub total: f64,
    /// Amount available to trade.
    pub available: f64,
}

/// Account totals plus per-asset balances.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSnapshot {
    /// Balances with a non-zero wallet balance.
    pub balances: Vec<Balance>,
    /// Wallet balance across all assets.
    pub wallet_balance: f64,
    /// Wallet balance plus unrealized profit and loss.
    pub equity: f64,
    /// Total unrealized profit and loss.
    pub unrealized_pnl: f64,
    /// Funds available to open new positions.
    pub available_balance: f64,
    /// Initial margin currently committed.
    pub initial_margin: f64,
    /// Maintenance margin currently required.
    pub maintenance_margin: f64,
}

/// A boxed future, so [`Venue`] stays object-safe while other venues are added.
pub type VenueFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VenueError>> + Send + 'a>>;

/// Everything that can go wrong while talking to a venue.
#[derive(Debug, thiserror::Error)]
pub enum VenueError {
    /// The request never produced a response.
    #[error("network error talking to {venue}: {source}")]
    Network {
        /// Venue that was contacted.
        venue: VenueId,
        /// Underlying transport failure.
        #[source]
        source: reqwest::Error,
    },

    /// The venue answered with a non-success status and no structured error.
    #[error("{venue} returned HTTP {status}: {detail}")]
    Http {
        /// Venue that was contacted.
        venue: VenueId,
        /// HTTP status code.
        status: u16,
        /// Short excerpt of the response body.
        detail: String,
    },

    /// The venue rejected the request with its own error code.
    #[error("{venue} rejected the request: code {code}, {message}")]
    Api {
        /// Venue that was contacted.
        venue: VenueId,
        /// Venue-specific error code.
        code: i64,
        /// Venue-provided explanation.
        message: String,
    },

    /// The response did not have the expected shape.
    #[error("{venue} returned an unexpected payload: {detail}")]
    Malformed {
        /// Venue that was contacted.
        venue: VenueId,
        /// What was wrong with the payload.
        detail: String,
    },

    /// The account cannot be used for live calls.
    #[error("cannot use this account: {0}")]
    Credentials(String),

    /// Offline fixture accounts are not wired up yet.
    #[error("fixture accounts are not supported yet (would read {path})")]
    FixtureUnsupported {
        /// Fixture the account points at.
        path: PathBuf,
    },
}

/// What the application needs from an exchange.
///
/// Implementations are read-only in v0.1; order entry is deliberately absent.
pub trait Venue: Send + Sync {
    /// Which exchange this client talks to.
    fn id(&self) -> VenueId;

    /// Prepare the connection: synchronise clocks, validate credentials.
    fn sync(&self) -> VenueFuture<'_, ()>;

    /// Tradable contracts, ordered by symbol.
    fn symbols(&self) -> VenueFuture<'_, Vec<Symbol>>;

    /// Open positions, flat contracts omitted, ordered by symbol.
    fn positions(&self) -> VenueFuture<'_, Vec<Position>>;

    /// Balances and account totals.
    fn account(&self) -> VenueFuture<'_, AccountSnapshot>;

    /// The most recent `limit` candles for `symbol`, oldest first.
    fn klines(&self, symbol: &str, interval: Interval, limit: u32) -> VenueFuture<'_, Vec<Kline>>;
}

#[cfg(test)]
mod tests {
    use super::{Interval, IntervalParseError};

    #[test]
    fn parses_every_wire_form() {
        for interval in Interval::ALL {
            assert_eq!(interval.as_str().parse::<Interval>(), Ok(interval));
        }
    }

    #[test]
    fn rejects_unknown_interval() {
        let error = "7m".parse::<Interval>().unwrap_err();
        assert_eq!(
            error,
            IntervalParseError {
                input: "7m".to_owned()
            }
        );
        assert!(error.to_string().contains("7m"));
    }

    #[test]
    fn cycles_through_all_intervals() {
        let mut interval = Interval::M1;
        for expected in Interval::ALL.iter().skip(1) {
            interval = interval.next();
            assert_eq!(interval, *expected);
        }
        assert_eq!(interval.next(), Interval::M1, "next wraps around");
        assert_eq!(
            Interval::M1.previous(),
            Interval::D1,
            "previous wraps around"
        );
        assert_eq!(Interval::H1.previous(), Interval::M15);
    }
}
