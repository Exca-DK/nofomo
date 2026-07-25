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

    /// Parses the stable SQLite representation produced by [`Side::as_str`].
    ///
    /// Returns an error for any other value so a corrupted row fails loudly
    /// instead of silently flipping a buy into a sell.
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "buy" => Ok(Self::Buy),
            "sell" => Ok(Self::Sell),
            other => anyhow::bail!("unknown side '{other}'"),
        }
    }
}

/// A standing rule: swap `token_in` for `token_out` once the priced asset
/// crosses `trigger_price_usd`.
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
    /// Decimals of `token_in`, snapshotted so the amount stays readable and
    /// correctly scaled even if the token config later changes.
    pub amount_decimals: u8,
    /// Maximum tolerated slippage, in basis points.
    pub slippage_bps: u16,
}

/// The asset `trigger_price_usd` refers to: what a buy acquires, what a sell disposes of.
pub fn base_token(level: &Level) -> &str {
    match level.side {
        Side::Buy => &level.token_out,
        Side::Sell => &level.token_in,
    }
}

/// Decides whether `level` should fire at `price_usd`, the USD price of the
/// asset named by [`base_token`]. A buy fires at or below its trigger, a sell
/// at or above. The caller is responsible for supplying a price for the right
/// asset.
pub fn level_fires(level: &Level, price_usd: f64) -> bool {
    match level.side {
        Side::Buy => price_usd <= level.trigger_price_usd,
        Side::Sell => price_usd >= level.trigger_price_usd,
    }
}
