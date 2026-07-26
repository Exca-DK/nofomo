mod filtered;
mod gates;

pub use filtered::FilteredSource;
pub use gates::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FUTURE_TOLERANCE_SECS, is_implausible, is_stale,
};

use std::pin::Pin;

use futures::Stream;

/// A token priced on one chain.
///
/// The chain is an EVM chain ID rather than a feed-specific name, so this type
/// stays independent of whichever provider ends up quoting it. Translating to a
/// provider's own vocabulary belongs to that provider's implementation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PricePair {
    pub chain_id: u64,
    pub token_address: String,
}

impl PricePair {
    pub fn new(chain_id: u64, token_address: impl Into<String>) -> Self {
        Self {
            chain_id,
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
    /// Quotes for `pair`, indefinitely.
    ///
    /// A failed read is an item, not the end of the stream, so a consumer can
    /// ride out transient trouble. The stream ends only when the source cannot
    /// serve this pair at all — an unsupported chain, say — because retrying
    /// that would never succeed.
    fn stream(&self, pair: &PricePair) -> PriceStream;

    /// Whether this source can price `pair` at all.
    ///
    /// Asked before a rule is stored. A rule the source cannot serve would sit
    /// in the database looking armed while its stream ended after one warning,
    /// so it is refused up front instead.
    ///
    /// Defaults to `true`: a source that cannot say in advance should let the
    /// rule through rather than reject everything.
    fn supports(&self, _pair: &PricePair) -> bool {
        true
    }
}
