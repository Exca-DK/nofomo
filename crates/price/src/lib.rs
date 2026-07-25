use std::pin::Pin;

use futures::Stream;
use serde_json::Value;

/// A token priced on one chain, addressed the way the price feed addresses it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PricePair {
    pub chain_slug: String,
    pub token_address: String,
}

impl PricePair {
    pub fn new(chain_slug: impl Into<String>, token_address: impl Into<String>) -> Self {
        Self {
            chain_slug: chain_slug.into(),
            token_address: token_address.into(),
        }
    }
}

/// One price observation.
///
/// `published_at` comes from the feed rather than the local clock, so staleness
/// is measured against when the price was made, not when it arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceTick {
    pub pair: PricePair,
    pub price_usd: f64,
    pub published_at: i64,
}

pub type PriceStream = Pin<Box<dyn Stream<Item = anyhow::Result<PriceTick>> + Send>>;

/// Source of live prices for one pair.
///
/// Returns a boxed stream rather than `impl Stream` so the trait stays
/// object-safe: the daemon holds `Arc<dyn PriceSource>` and tests substitute a
/// scripted double.
pub trait PriceSource: Send + Sync {
    /// An endless stream. A failed read is an item, not the end of the stream,
    /// so the consumer decides whether to tolerate it.
    fn stream(&self, pair: &PricePair) -> PriceStream;
}

/// A tick older than this is not worth trading on.
pub const DEFAULT_MAX_AGE_SECS: i64 = 120;

/// How far ahead of the local clock a feed may be before its timestamps are
/// treated as wrong rather than merely skewed.
pub const FUTURE_TOLERANCE_SECS: i64 = 5;

/// Reads a price frame for `pair`.
///
/// Returns `None` when the frame is not a usable quote for this pair: a
/// heartbeat, malformed JSON, a missing price or timestamp, or a frame about
/// some other token. Callers skip those rather than tearing down the stream.
pub fn parse_tick(pair: &PricePair, data: &str) -> Option<PriceTick> {
    let value: Value = serde_json::from_str(data).ok()?;
    // Some feeds wrap the payload; accept either shape.
    let body = value.get("data").unwrap_or(&value);

    // A crossed subscription would otherwise price the wrong token, and the
    // daemon spends real funds on whatever price it is handed.
    if !addresses_match(body, pair) {
        return None;
    }

    Some(PriceTick {
        pair: pair.clone(),
        price_usd: price_of(body)?,
        // Required, not defaulted: substituting "now" would silently disable the
        // staleness gate for any feed that stops sending timestamps.
        published_at: body.get("timestamp").and_then(Value::as_i64)?,
    })
}

/// The chain slug the price feed uses for an EVM chain ID.
///
/// Covers exactly the chains `Config::validate` accepts. Adding a chain to the
/// configuration without adding it here yields levels that nothing ever prices,
/// so the two lists have to move together. An unknown chain is `None`, never a
/// fallback slug: pricing against the wrong chain is worse than not pricing.
pub fn chain_slug(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("ethereum"),
        8453 => Some("base"),
        42161 => Some("arbitrum"),
        _ => None,
    }
}

/// Whether a tick is too old, or dated far enough ahead that its clock cannot
/// be trusted.
///
/// The future check matters: comparing only `now - published_at` would let a
/// feed with a fast clock look permanently fresh, defeating the gate entirely.
pub fn is_stale(tick: &PriceTick, now: i64, max_age_secs: i64) -> bool {
    let age = now.saturating_sub(tick.published_at);
    age > max_age_secs || age < -FUTURE_TOLERANCE_SECS
}

/// Whether a price moved further from the previous one than a real market would.
///
/// Anything that cannot be compared meaningfully counts as implausible: a zero
/// or negative previous price, or a non-finite value on either side.
pub fn is_implausible(prev_usd: f64, next_usd: f64, max_move_bps: u32) -> bool {
    if !prev_usd.is_finite() || !next_usd.is_finite() {
        return true;
    }
    if prev_usd <= 0.0 || next_usd <= 0.0 {
        return true;
    }
    let move_bps = ((next_usd - prev_usd).abs() / prev_usd) * 10_000.0;
    move_bps > f64::from(max_move_bps)
}

// The feed echoes the token and chain it is quoting; both must be the ones asked
// for. Compared case-insensitively because an EIP-55 checksummed address and its
// lowercase form name the same token.
fn addresses_match(body: &Value, pair: &PricePair) -> bool {
    let address = body.get("address").and_then(Value::as_str);
    let chain = body.get("chain").and_then(Value::as_str);
    match (address, chain) {
        (Some(address), Some(chain)) => {
            address.eq_ignore_ascii_case(&pair.token_address)
                && chain.eq_ignore_ascii_case(&pair.chain_slug)
        }
        _ => false,
    }
}

// The feed reports the price as a JSON number in some frames and as a string in
// others, so both are accepted.
fn price_of(body: &Value) -> Option<f64> {
    let raw = body.get("price").or_else(|| body.get("price_usd"))?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|text| text.parse().ok()))
}
