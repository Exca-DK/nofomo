mod filtered;
mod gates;

pub use filtered::FilteredSource;
pub use gates::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FUTURE_TOLERANCE_SECS, is_implausible, is_stale,
};

use std::pin::Pin;

use futures::Stream;
use serde::Serialize;

/// A token and its chain-independent EVM chain ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
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

/// A price with the feed's publication time.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceTick {
    pub pair: PricePair,
    pub price_usd: f64,
    pub published_at: i64,
}

pub type PriceStream = Pin<Box<dyn Stream<Item = anyhow::Result<PriceTick>> + Send>>;

/// Object-safe source of live prices.
pub trait PriceSource: Send + Sync {
    /// Streams quotes and transient errors; ends only for an unsupported pair.
    fn stream(&self, pair: &PricePair) -> PriceStream;

    /// Checks support before storing a rule; unknown support defaults to true.
    fn supports(&self, _pair: &PricePair) -> bool {
        true
    }
}
