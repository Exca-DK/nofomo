use alloy_primitives::U256;
use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken};
use tempo_agentic_domain::{VenueName, parse_units_string};
use tempo_agentic_price::PriceSource;
use tempo_agentic_strategy::{Level, Strategy, StrategyLevel, trade_direction};

use crate::resolver::TokenResolver;

/// Human-readable input for one market.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct StrategyDraft {
    pub id: String,
    pub venue: String,
    pub chain: String,
    pub base_token: String,
    pub quote_token: String,
}

/// Human-readable input for one threshold belonging to a strategy.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct LevelDraft {
    pub id: String,
    pub strategy_id: String,
    pub side: String,
    pub trigger_price_usd: f64,
    /// How much of the side's input token to spend, in whole units.
    pub amount: String,
    pub slippage_bps: u16,
}

/// Validates and canonicalizes a strategy against the daemon's config snapshot.
pub fn validate_strategy(
    evm: &EvmConfig,
    prices: &dyn PriceSource,
    draft: &StrategyDraft,
) -> Result<Strategy> {
    let venue: VenueName = draft.venue.parse()?;
    if venue != VenueName::Uniswap {
        bail!("unsupported trade venue {}", draft.venue);
    }
    let chain = find_chain(evm, &draft.chain)?;
    let (base_symbol, _) = find_token(chain, &draft.base_token)?;
    let (quote_symbol, _) = find_token(chain, &draft.quote_token)?;
    if base_symbol.eq_ignore_ascii_case(quote_symbol) {
        bail!("base_token and quote_token must differ");
    }

    let strategy = Strategy {
        id: draft.id.clone(),
        venue,
        chain: chain.name.clone(),
        base_token: base_symbol.to_ascii_uppercase(),
        quote_token: quote_symbol.to_ascii_uppercase(),
    };
    validate_strategy_model(evm, prices, &strategy)?;
    Ok(strategy)
}

/// Validates a stored strategy against the current config snapshot.
pub fn validate_strategy_model(
    evm: &EvmConfig,
    prices: &dyn PriceSource,
    strategy: &Strategy,
) -> Result<()> {
    if strategy.venue != VenueName::Uniswap {
        bail!("unsupported trade venue {}", strategy.venue.as_str());
    }
    let chain = find_chain(evm, &strategy.chain)?;
    find_token(chain, &strategy.base_token)?;
    find_token(chain, &strategy.quote_token)?;
    if strategy
        .base_token
        .eq_ignore_ascii_case(&strategy.quote_token)
    {
        bail!("base_token and quote_token must differ");
    }
    let pair = TokenResolver::from_config(evm)
        .price_pair(strategy)
        .context("cannot resolve the strategy's base token")?;
    if !prices.supports(&pair) {
        bail!(
            "no price source quotes {} on {}, so this strategy could never fire",
            strategy.base_token,
            strategy.chain
        );
    }
    Ok(())
}

/// Validates and resolves a draft into an executable level.
pub fn validate_level(
    evm: &EvmConfig,
    max_slippage_bps: u16,
    prices: &dyn PriceSource,
    strategy: &Strategy,
    draft: &LevelDraft,
) -> Result<Level> {
    if draft.strategy_id != strategy.id {
        bail!("level strategy_id does not match strategy {}", strategy.id);
    }
    validate_strategy_model(evm, prices, strategy)?;
    if draft.slippage_bps > max_slippage_bps {
        bail!("slippage_bps must not exceed the configured maximum {max_slippage_bps}");
    }
    let side = draft.side.parse()?;
    let direction = trade_direction(strategy, side);
    let chain = find_chain(evm, &strategy.chain)?;
    let (_, input) = find_token(chain, direction.token_in)?;
    let amount = parse_units_string(&draft.amount, input.decimals)?;

    Ok(Level {
        id: draft.id.clone(),
        strategy_id: strategy.id.clone(),
        side,
        trigger_price_usd: draft.trigger_price_usd,
        amount: U256::from_str_radix(&amount, 10).context("amount does not fit in 256 bits")?,
        amount_decimals: input.decimals,
        slippage_bps: draft.slippage_bps,
    })
}

/// Checks the persisted decimals snapshot as part of fail-closed daemon startup.
pub fn validate_stored_level(
    evm: &EvmConfig,
    max_slippage_bps: u16,
    prices: &dyn PriceSource,
    entry: &StrategyLevel,
) -> Result<()> {
    validate_strategy_model(evm, prices, &entry.strategy)?;
    if entry.level.strategy_id != entry.strategy.id {
        bail!("level references a different strategy");
    }
    if entry.level.slippage_bps > max_slippage_bps {
        bail!("slippage exceeds configured maximum {max_slippage_bps}");
    }
    let direction = trade_direction(&entry.strategy, entry.level.side);
    let chain = find_chain(evm, &entry.strategy.chain)?;
    let (_, input) = find_token(chain, direction.token_in)?;
    if entry.level.amount_decimals != input.decimals {
        bail!(
            "level {} snapshots {} decimals for {}, config has {}",
            entry.level.id,
            entry.level.amount_decimals,
            direction.token_in,
            input.decimals
        );
    }
    Ok(())
}

fn find_chain<'a>(evm: &'a EvmConfig, name: &str) -> Result<&'a EvmChain> {
    evm.chains
        .iter()
        .find(|chain| chain.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("EVM chain {name} is not configured"))
}

fn find_token<'a>(chain: &'a EvmChain, symbol: &str) -> Result<(&'a str, &'a EvmToken)> {
    chain
        .tokens
        .iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(symbol))
        .map(|(configured, token)| (configured.as_str(), token))
        .with_context(|| format!("{} does not configure {symbol}", chain.name))
}
