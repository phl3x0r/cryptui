//! Exchange abstraction shared by the configuration and the venue clients.
//!
//! Phase P1 defines the identifiers only (venue and candle interval). The
//! `Venue` trait and the Binance USDⓈ-M futures client are added alongside it,
//! but nothing outside this module may hard-code a venue string.

use std::fmt;
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
