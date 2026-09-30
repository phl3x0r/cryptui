//! Request signing for authenticated venue endpoints.
//!
//! Binance signs the exact query string that is sent, so encoding and signing
//! must agree byte for byte: [`Query`] encodes once and [`Signer::sign_query`]
//! signs precisely what [`Query::encode`] produced.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::config::Secret;

/// Receive window sent with signed requests, in milliseconds.
pub const DEFAULT_RECV_WINDOW_MS: u64 = 5_000;

type HmacSha256 = Hmac<Sha256>;

/// Signs query strings with an account's API secret.
#[derive(Debug)]
pub struct Signer {
    secret: Secret,
    recv_window_ms: u64,
}

impl Signer {
    /// Create a signer for `secret`.
    pub fn new(secret: Secret, recv_window_ms: u64) -> Self {
        Self {
            secret,
            recv_window_ms,
        }
    }

    /// Receive window this signer advertises.
    pub fn recv_window_ms(&self) -> u64 {
        self.recv_window_ms
    }

    /// Lowercase hex HMAC-SHA256 of `message`.
    pub fn sign(&self, message: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.expose().as_bytes())
            .expect("HMAC accepts keys of any length");
        mac.update(message.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Extend an already-encoded query with `timestamp`, `recvWindow` and
    /// `signature`, in the order the venue expects.
    pub fn sign_query(&self, query: &str, timestamp_ms: i64) -> String {
        let tail = format!(
            "timestamp={timestamp_ms}&recvWindow={}",
            self.recv_window_ms
        );
        let base = if query.is_empty() {
            tail
        } else {
            format!("{query}&{tail}")
        };
        let signature = self.sign(&base);
        format!("{base}&signature={signature}")
    }
}

/// Ordered query parameters, percent-encoded once and reused for both the
/// request and the signature.
#[derive(Debug, Default)]
pub struct Query {
    pairs: Vec<(String, String)>,
}

impl Query {
    /// An empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a parameter. Insertion order is preserved, which the signature
    /// depends on.
    pub fn push(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        self.pairs.push((key.to_owned(), value.into()));
        self
    }

    /// Encode as `key=value&key=value`.
    pub fn encode(&self) -> String {
        self.pairs
            .iter()
            .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
            .collect::<Vec<_>>()
            .join("&")
    }
}

/// Percent-encode one component, leaving the unreserved set intact.
///
/// Values are percent-encoded in UTF-8, as the venue requires.
pub fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Current wall-clock time in milliseconds since the UNIX epoch.
///
/// Returns `0` if the system clock reports a time before the epoch, which only
/// matters because signed requests would then be rejected loudly.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::{Query, Signer, encode_component, now_ms};
    use crate::config::Secret;

    /// The worked example from the venue's signed-endpoint documentation.
    const DOC_SECRET: &str = "NhqPtMDcxFSmvz3eFzbn3Ux8Gc1oGq5mCk8LhrzLoZqXbLy5qC1k7A0mZ0qXb3X";
    const DOC_QUERY: &str = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";
    /// Cross-checked with `openssl dgst -sha256 -hmac` and Python's `hmac`.
    const DOC_SIGNATURE: &str = "fbafb81b6de5b895db05a4ce215121252d5cbf5ffe7b43ca809d7046fa1cb423";

    fn signer(secret: &str) -> Signer {
        Signer::new(Secret::from_test_value(secret), 5_000)
    }

    #[test]
    fn matches_documented_signature() {
        assert_eq!(signer(DOC_SECRET).sign(DOC_QUERY), DOC_SIGNATURE);
    }

    #[test]
    fn sign_query_appends_parameters_in_order() {
        let signed = signer(DOC_SECRET).sign_query("symbol=BTCUSDT", 1_499_827_319_559);
        let expected_tail = "&timestamp=1499827319559&recvWindow=5000&signature=";
        assert!(signed.starts_with("symbol=BTCUSDT"), "got: {signed}");
        assert!(signed.contains(expected_tail), "got: {signed}");

        let (base, signature) = signed.split_once("&signature=").expect("signature present");
        assert_eq!(signature, signer(DOC_SECRET).sign(base));
    }

    #[test]
    fn sign_query_without_parameters_has_no_leading_ampersand() {
        let signed = signer(DOC_SECRET).sign_query("", 42);
        assert!(signed.starts_with("timestamp=42"), "got: {signed}");
    }

    #[test]
    fn query_encodes_and_preserves_order() {
        let mut query = Query::new();
        query.push("symbol", "BTC/USDT").push("limit", "5");
        assert_eq!(query.encode(), "symbol=BTC%2FUSDT&limit=5");
    }

    #[test]
    fn encodes_unreserved_and_special_characters() {
        assert_eq!(encode_component("BTCUSDT"), "BTCUSDT");
        assert_eq!(encode_component("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_component("a b"), "a%20b");
        assert_eq!(encode_component("&="), "%26%3D");
        assert_eq!(encode_component("ä"), "%C3%A4");
    }

    #[test]
    fn now_ms_is_after_2023() {
        // Guards against a clock so far off that signing would never work.
        assert!(now_ms() > 1_700_000_000_000, "system clock looks unset");
    }
}
