use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use tempo_agentic_domain::VenueName;

/// Whether a level buys or sells the asset it is priced against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
}

impl std::str::FromStr for Side {
    type Err = anyhow::Error;

    /// Parses [`Side::as_str`] output, rejecting unknown values.
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "buy" => Ok(Self::Buy),
            "sell" => Ok(Self::Sell),
            other => anyhow::bail!("unknown side '{other}'"),
        }
    }
}

/// One configured market shared by all of its levels.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Strategy {
    pub id: String,
    pub venue: VenueName,
    /// Chain name as configured in `EvmConfig::chains`, e.g. `base`.
    pub chain: String,
    /// Asset whose USD price every level observes, e.g. `WETH`.
    pub base_token: String,
    /// Counter asset used to buy or sell the base asset, e.g. `USDC`.
    pub quote_token: String,
}

/// A threshold and amount belonging to one [`Strategy`].
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Level {
    pub id: String,
    pub strategy_id: String,
    pub side: Side,
    /// USD trigger for the strategy's base token.
    pub trigger_price_usd: f64,
    /// Raw base units of the token returned by [`trade_direction`].
    pub amount: U256,
    /// Snapshotted decimals of the token being spent.
    pub amount_decimals: u8,
    /// Maximum tolerated slippage, in basis points.
    pub slippage_bps: u16,
}

/// One consistent database snapshot of a strategy and its level.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct StrategyLevel {
    pub strategy: Strategy,
    pub level: Level,
}

/// Input and output tokens for a side of a strategy market.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradeDirection<'a> {
    pub token_in: &'a str,
    pub token_out: &'a str,
}

/// The sole buy/sell mapping used by validation, quoting, and order snapshots.
pub fn trade_direction(strategy: &Strategy, side: Side) -> TradeDirection<'_> {
    match side {
        Side::Buy => TradeDirection {
            token_in: &strategy.quote_token,
            token_out: &strategy.base_token,
        },
        Side::Sell => TradeDirection {
            token_in: &strategy.base_token,
            token_out: &strategy.quote_token,
        },
    }
}

/// Checks whether a matching asset price crosses the level's threshold.
pub fn level_fires(level: &Level, price_usd: f64) -> bool {
    match level.side {
        Side::Buy => price_usd <= level.trigger_price_usd,
        Side::Sell => price_usd >= level.trigger_price_usd,
    }
}
