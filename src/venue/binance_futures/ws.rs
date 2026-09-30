//! Live market streams over WebSocket.
//!
//! Binance pushes each update as a JSON object. Reconnection lives here, so a
//! subscription behaves as "runs until the receiver goes away", and callers see
//! a steady flow of parsed values rather than connection handling.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::venue::{Interval, Kline, StreamEvent};

/// Delay before the first reconnect attempt.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling on the reconnect delay.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Symbols allowed in one combined mark-price subscription, keeping the URL
/// short enough for the venue to accept.
pub(crate) const MAX_MARK_SYMBOLS: usize = 150;

/// Follow one symbol's candles until `updates` is dropped.
pub(crate) async fn follow_klines(
    base: &str,
    symbol: &str,
    interval: Interval,
    updates: UnboundedSender<StreamEvent<Kline>>,
) {
    let stream = format!("{}@kline_{}", symbol.to_lowercase(), interval.as_str());
    let url = format!("{base}/ws/{stream}");
    pump(&url, parse_kline, &updates).await;
}

/// Follow mark prices for `symbols` until `updates` is dropped.
///
/// Uses a combined stream of exactly the contracts the account holds, rather
/// than the `!markPrice@arr` firehose of every symbol on the venue.
pub(crate) async fn follow_marks(
    base: &str,
    symbols: &[String],
    updates: UnboundedSender<StreamEvent<(String, f64)>>,
) {
    if symbols.is_empty() {
        return;
    }
    let streams = symbols
        .iter()
        .take(MAX_MARK_SYMBOLS)
        .map(|symbol| format!("{}@markPrice", symbol.to_lowercase()))
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("{base}/stream?streams={streams}");
    pump(&url, parse_mark_price, &updates).await;
}

/// Keep one subscription alive, reconnecting with exponential backoff.
///
/// Returns when the receiver is dropped, which is how the caller cancels.
async fn pump<T>(
    url: &str,
    parse: fn(&str) -> Option<T>,
    updates: &UnboundedSender<StreamEvent<T>>,
) {
    let mut backoff = INITIAL_BACKOFF;

    loop {
        if updates.is_closed() {
            return;
        }
        match connect(url, parse, updates).await {
            // A connection that ended cleanly starts fresh at the shortest delay.
            Ok(()) => {
                backoff = INITIAL_BACKOFF;
                if updates
                    .send(StreamEvent::Disconnected("stream closed".to_owned()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, %url, "market stream dropped");
                if updates.send(StreamEvent::Disconnected(error)).is_err() {
                    return;
                }
            }
        }
        if updates.is_closed() {
            return;
        }

        tracing::debug!(?backoff, %url, "reconnecting market stream");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// One connection's lifetime.
async fn connect<T>(
    url: &str,
    parse: fn(&str) -> Option<T>,
    updates: &UnboundedSender<StreamEvent<T>>,
) -> Result<(), String> {
    let (mut socket, _) = connect_async(url)
        .await
        .map_err(|error| error.to_string())?;
    tracing::debug!(%url, "market stream connected");

    while let Some(message) = socket.next().await {
        match message.map_err(|error| error.to_string())? {
            Message::Text(text) => {
                tracing::trace!(bytes = text.len(), "stream frame received");
                match parse(&text) {
                    Some(value) => {
                        if updates.send(StreamEvent::Data(value)).is_err() {
                            return Ok(()); // the receiver is gone
                        }
                    }
                    // A venue that answers with an error frame, or a stream we
                    // did not ask for, must not look like silence.
                    None => tracing::debug!(%text, "ignoring unrecognised stream message"),
                }
            }
            Message::Ping(payload) => {
                // The venue drops connections that do not answer pings, and
                // nothing else flushes the queued reply.
                socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Message::Close(frame) => {
                tracing::debug!(?frame, "stream closed by the venue");
                return Ok(());
            }
            other => tracing::trace!(?other, "ignoring non-text stream frame"),
        }
    }

    Ok(())
}

/// Parse a kline event into a candle.
fn parse_kline(text: &str) -> Option<Kline> {
    let event: Value = serde_json::from_str(text).ok()?;
    let kline = event.get("k")?;

    Some(Kline {
        open_time_ms: kline.get("t")?.as_i64()?,
        open: number(kline.get("o")?)?,
        high: number(kline.get("h")?)?,
        low: number(kline.get("l")?)?,
        close: number(kline.get("c")?)?,
        volume: number(kline.get("v")?)?,
        close_time_ms: kline.get("T")?.as_i64()?,
        closed: kline.get("x")?.as_bool()?,
    })
}

/// Parse a mark-price update into `(symbol, price)`.
///
/// Handles both the bare stream and the combined-stream wrapper.
fn parse_mark_price(text: &str) -> Option<(String, f64)> {
    let event: Value = serde_json::from_str(text).ok()?;
    let data = event.get("data").unwrap_or(&event);
    if data.get("e")?.as_str()? != "markPriceUpdate" {
        return None;
    }

    Some((
        data.get("s")?.as_str()?.to_uppercase(),
        number(data.get("p")?)?,
    ))
}

/// Read a decimal the venue may encode as a string or a number.
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::{parse_kline, parse_mark_price};

    /// Shape verified against the live `btcusdt@kline_15m` stream.
    const KLINE: &str = r#"{"e":"kline","E":1790773260000,"s":"BTCUSDT",
        "k":{"t":1790773200000,"T":1790774099999,"s":"BTCUSDT","i":"15m",
        "o":"85259.80","c":"85357.30","h":"85632.70","l":"85071.20","v":"8559.056","x":false}}"#;

    #[test]
    fn parses_a_kline_event() {
        let candle = parse_kline(KLINE).expect("payload parses");

        assert_eq!(candle.open_time_ms, 1_790_773_200_000);
        assert_eq!(candle.close_time_ms, 1_790_774_099_999);
        assert_eq!(candle.open, 85_259.80);
        assert_eq!(candle.high, 85_632.70);
        assert_eq!(candle.low, 85_071.20);
        assert_eq!(candle.close, 85_357.30);
        assert_eq!(candle.volume, 8_559.056);
        assert!(!candle.closed, "the forming candle is not closed");
    }

    #[test]
    fn ignores_unrelated_payloads() {
        assert!(parse_kline("not json").is_none());
        assert!(parse_kline(r#"{"e":"aggTrade"}"#).is_none());
        assert!(
            parse_kline(r#"{"e":"kline","k":{"o":"1"}}"#).is_none(),
            "a partial payload is rejected rather than half-parsed"
        );
    }

    #[test]
    fn parses_a_combined_stream_mark_price() {
        let payload = r#"{"stream":"btcusdt@markPrice","data":{"e":"markPriceUpdate","E":1790773260000,
            "s":"BTCUSDT","p":"85350.50000000","i":"85300.00"}}"#;

        let (symbol, price) = parse_mark_price(payload).expect("payload parses");
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(price, 85_350.5);
    }

    #[test]
    fn parses_a_bare_mark_price_and_upper_cases_the_symbol() {
        let payload = r#"{"e":"markPriceUpdate","s":"ethusdt","p":"3000.25"}"#;
        let (symbol, price) = parse_mark_price(payload).expect("payload parses");
        assert_eq!(symbol, "ETHUSDT");
        assert_eq!(price, 3_000.25);
    }

    #[test]
    fn ignores_other_event_types_on_the_same_socket() {
        assert!(parse_mark_price(r#"{"e":"kline","s":"BTCUSDT"}"#).is_none());
        assert!(parse_mark_price(r#"{"result":null,"id":1}"#).is_none());
    }
}
