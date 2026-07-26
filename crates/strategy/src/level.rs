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

/// Swaps tokens when the priced asset crosses a threshold.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Level {
    pub id: String,
    pub venue: VenueName,
    /// Chain name as configured in `EvmConfig::chains`, e.g. `base`.
    pub chain: String,
    /// Symbol of the token being spent, e.g. `USDC`.
    pub token_in: String,
    /// Symbol of the token being acquired, e.g. `WETH`.
    pub token_out: String,
    pub side: Side,
    /// USD trigger for the asset named by [`base_token`].
    pub trigger_price_usd: f64,
    /// Raw base units of `token_in` to spend.
    pub amount: U256,
    /// Snapshotted `token_in` decimals.
    pub amount_decimals: u8,
    /// Maximum tolerated slippage, in basis points.
    pub slippage_bps: u16,
}

/// Asset priced by this level.
pub fn base_token(level: &Level) -> &str {
    match level.side {
        Side::Buy => &level.token_out,
        Side::Sell => &level.token_in,
    }
}

/// Checks whether a matching asset price crosses the level's threshold.
pub fn level_fires(level: &Level, price_usd: f64) -> bool {
    match level.side {
        Side::Buy => price_usd <= level.trigger_price_usd,
        Side::Sell => price_usd >= level.trigger_price_usd,
    }
}
