use alloy_primitives::U256;
use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken};
use tempo_agentic_domain::parse_units_string;
use tempo_agentic_price::PriceSource;
use tempo_agentic_strategy::{Level, base_token};

use crate::resolver::TokenResolver;

/// Human-readable input for a rule.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct LevelDraft {
    pub id: String,
    pub venue: String,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    /// `buy` watches the price of `token_out`, `sell` the price of `token_in`.
    pub side: String,
    pub trigger_price_usd: f64,
    /// How much `token_in` to spend, in whole units rather than base units.
    pub amount: String,
    pub slippage_bps: u16,
}

/// Validates and resolves a draft into an executable rule.
pub fn validate_level(
    evm: &EvmConfig,
    max_slippage_bps: u16,
    prices: &dyn PriceSource,
    draft: &LevelDraft,
) -> Result<Level> {
    let chain = evm
        .chains
        .iter()
        .find(|chain| chain.name.eq_ignore_ascii_case(&draft.chain))
        .with_context(|| format!("EVM chain {} is not configured", draft.chain))?;
    let input = find_token(chain, &draft.token_in)
        .with_context(|| format!("{} does not configure {}", chain.name, draft.token_in))?;
    find_token(chain, &draft.token_out)
        .with_context(|| format!("{} does not configure {}", chain.name, draft.token_out))?;
    if draft.token_in.eq_ignore_ascii_case(&draft.token_out) {
        bail!("token_in and token_out must differ");
    }
    if draft.slippage_bps > max_slippage_bps {
        bail!("slippage_bps must not exceed the configured maximum {max_slippage_bps}");
    }
    let amount = parse_units_string(&draft.amount, input.decimals)?;

    let level = Level {
        id: draft.id.clone(),
        venue: draft.venue.parse()?,
        // Keep the configured spelling used by the resolver.
        chain: chain.name.clone(),
        token_in: draft.token_in.to_ascii_uppercase(),
        token_out: draft.token_out.to_ascii_uppercase(),
        side: draft.side.parse()?,
        trigger_price_usd: draft.trigger_price_usd,
        amount: U256::from_str_radix(&amount, 10).context("amount does not fit in 256 bits")?,
        amount_decimals: input.decimals,
        slippage_bps: draft.slippage_bps,
    };

    // Reject rules the source cannot price.
    let pair = TokenResolver::from_config(evm)
        .price_pair(&level)
        .context("cannot work out which token this rule would be priced on")?;
    if !prices.supports(&pair) {
        bail!(
            "no price source quotes {} on {}, so this rule could never fire",
            base_token(&level),
            level.chain
        );
    }
    Ok(level)
}

fn find_token<'a>(chain: &'a EvmChain, symbol: &str) -> Option<&'a EvmToken> {
    chain
        .tokens
        .iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(symbol))
        .map(|(_, token)| token)
}
