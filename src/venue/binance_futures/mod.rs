//! Binance USDⓈ-M futures client.
//!
//! Endpoint versions were verified against the live API before being wired in:
//! `/fapi/v3/positionRisk` returns only non-flat positions and carries the
//! initial/maintenance margin that the table needs, whereas the deprecated v2
//! endpoint returns every symbol. `leverage` was dropped by v3, so it is not
//! modelled yet — sourcing it would cost a second signed call per refresh.

mod income;
mod rest;
mod wire;
mod ws;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use reqwest::Client;

use crate::auth::{DEFAULT_RECV_WINDOW_MS, Query, Signer, now_ms};
use crate::config::Secret;

use super::{VenueError, VenueId};

/// Mainnet REST base URL.
const MAINNET_BASE: &str = "https://fapi.binance.com";
/// Testnet REST base URL.
const TESTNET_BASE: &str = "https://testnet.binancefuture.com";
/// Mainnet market-stream base URL.
const MAINNET_STREAM: &str = "wss://fstream.binance.com";
/// Testnet market-stream base URL.
const TESTNET_STREAM: &str = "wss://stream.binancefuture.com";
/// How the client identifies itself to the venue.
const USER_AGENT: &str = concat!("cryptui/", env!("CARGO_PKG_VERSION"));
/// Per-request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Read-only client for Binance USDⓈ-M futures.
#[derive(Debug)]
pub struct BinanceFutures {
    http: Client,
    base: &'static str,
    stream_base: &'static str,
    api_key: Secret,
    signer: Signer,
    /// Venue clock minus local clock, in milliseconds.
    clock_offset_ms: AtomicI64,
}

impl BinanceFutures {
    /// Build a client for `api_key`/`api_secret`.
    ///
    /// Call [`BinanceFutures::sync_clock`] once before signed requests: a local
    /// clock that drifts past the receive window makes every signed call fail.
    pub fn new(api_key: Secret, api_secret: Secret, testnet: bool) -> Result<Self, VenueError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| VenueError::Network {
                venue: VenueId::BinanceFutures,
                source,
            })?;

        Ok(Self {
            http,
            base: if testnet { TESTNET_BASE } else { MAINNET_BASE },
            stream_base: if testnet {
                TESTNET_STREAM
            } else {
                MAINNET_STREAM
            },
            api_key,
            signer: Signer::new(api_secret, DEFAULT_RECV_WINDOW_MS),
            clock_offset_ms: AtomicI64::new(0),
        })
    }

    /// Base URL in use, for diagnostics. Contains no secret material.
    pub fn base_url(&self) -> &'static str {
        self.base
    }

    /// Market-stream base URL in use.
    pub fn stream_base_url(&self) -> &'static str {
        self.stream_base
    }

    /// Learn the venue's clock so signed requests survive local clock drift.
    pub async fn sync_clock(&self) -> Result<(), VenueError> {
        let before = now_ms();
        let payload = self.get_json("/fapi/v1/time", None, false).await?;
        let after = now_ms();

        let server_time = payload
            .get("serverTime")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| self.malformed("time payload without `serverTime`"))?;

        // The response arrived somewhere between the two local reads, so
        // compare against the midpoint to halve the round-trip error.
        let midpoint = before + (after - before) / 2;
        self.clock_offset_ms
            .store(server_time - midpoint, Ordering::Relaxed);

        tracing::debug!(
            offset_ms = server_time - midpoint,
            "synchronised clock with the venue"
        );
        Ok(())
    }

    /// Timestamp to send with signed requests, corrected for clock skew.
    fn timestamp_ms(&self) -> i64 {
        now_ms() + self.clock_offset_ms.load(Ordering::Relaxed)
    }

    /// Build a payload-shape error.
    fn malformed(&self, detail: impl Into<String>) -> VenueError {
        VenueError::Malformed {
            venue: VenueId::BinanceFutures,
            detail: detail.into(),
        }
    }

    /// `GET` a JSON document, optionally signed.
    ///
    /// The URL is never logged for signed requests: it carries the signature.
    async fn get_json(
        &self,
        path: &str,
        query: Option<&Query>,
        signed: bool,
    ) -> Result<serde_json::Value, VenueError> {
        let encoded = match query {
            Some(query) => query.encode(),
            None => String::new(),
        };
        let encoded = if signed {
            self.signer.sign_query(&encoded, self.timestamp_ms())
        } else {
            encoded
        };

        let url = if encoded.is_empty() {
            format!("{}{path}", self.base)
        } else {
            format!("{}{path}?{encoded}", self.base)
        };

        let mut request = self.http.get(&url);
        if signed {
            request = request.header("X-MBX-APIKEY", self.api_key.expose());
        }

        let response = request.send().await.map_err(|source| VenueError::Network {
            venue: VenueId::BinanceFutures,
            source,
        })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|source| VenueError::Network {
                venue: VenueId::BinanceFutures,
                source,
            })?;

        if !status.is_success() {
            return Err(Self::http_error(status.as_u16(), &body));
        }

        serde_json::from_str(&body)
            .map_err(|error| self.malformed(format!("response is not valid JSON: {error}")))
    }

    /// Translate a failed response, preferring the venue's structured error.
    fn http_error(status: u16, body: &str) -> VenueError {
        let venue = VenueId::BinanceFutures;

        if let Ok(api_error) = serde_json::from_str::<wire::ApiError>(body) {
            return VenueError::Api {
                venue,
                code: api_error.code,
                message: api_error.msg,
            };
        }

        // Cloudflare-style blocks return HTML, so keep only a short excerpt.
        let excerpt: String = body.trim().chars().take(200).collect();
        let detail = if status == 451 || status == 403 {
            format!("{excerpt} (the venue appears to block this network or region)")
        } else {
            excerpt
        };
        VenueError::Http {
            venue,
            status,
            detail,
        }
    }
}
