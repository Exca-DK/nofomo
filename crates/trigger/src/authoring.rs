use alloy_primitives::U256;
use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use tempo_agentic_domain::{VenueName, parse_units_string};
use tempo_agentic_price::PriceSource;
use tempo_agentic_strategy::{Level, Strategy, StrategyLevel, trade_direction};

use crate::resolver::{RegisteredToken, TokenResolver};

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
///
/// # Errors
///
/// Returns an error if the venue is unknown, if it trades a different chain
/// family than the chain does, if either token is unconfigured, or if no price
/// source quotes the base token.
pub fn validate_strategy(
    tokens: &TokenResolver,
    prices: &dyn PriceSource,
    draft: &StrategyDraft,
) -> Result<Strategy> {
    let venue: VenueName = draft.venue.parse()?;
    let base = token(tokens, &draft.chain, &draft.base_token)?;
    let quote = token(tokens, &draft.chain, &draft.quote_token)?;
    if base.id.eq_ignore_ascii_case(&quote.id) {
        bail!("base_token and quote_token must differ");
    }

    let strategy = Strategy {
        id: draft.id.clone(),
        venue,
        // Keep the configured spelling, which is what the resolver keys on.
        chain: base.chain_name.clone(),
        base_token: base.id.clone(),
        quote_token: quote.id.clone(),
    };
    validate_strategy_model(tokens, prices, &strategy)?;
    Ok(strategy)
}

/// Validates a stored strategy against the current config snapshot.
///
/// # Errors
///
/// Returns an error if the venue no longer matches the chain's family, if a
/// token left the configuration, or if the base token became unquotable.
pub fn validate_strategy_model(
    tokens: &TokenResolver,
    prices: &dyn PriceSource,
    strategy: &Strategy,
) -> Result<()> {
    let base = token(tokens, &strategy.chain, &strategy.base_token)?;
    token(tokens, &strategy.chain, &strategy.quote_token)?;
    if strategy
        .base_token
        .eq_ignore_ascii_case(&strategy.quote_token)
    {
        bail!("base_token and quote_token must differ");
    }
    // A venue trading another family would only fail later, at quote time.
    if strategy.venue.family() != base.chain.family() {
        bail!(
            "venue {} does not trade {}",
            strategy.venue.as_str(),
            base.chain.family()
        );
    }

    let pair = tokens
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
///
/// # Errors
///
/// Returns an error if the draft names another strategy, if the strategy no
/// longer validates, if slippage exceeds the maximum, or if the amount does not
/// fit the input token.
pub fn validate_level(
    tokens: &TokenResolver,
    max_slippage_bps: u16,
    prices: &dyn PriceSource,
    strategy: &Strategy,
    draft: &LevelDraft,
) -> Result<Level> {
    if draft.strategy_id != strategy.id {
        bail!("level strategy_id does not match strategy {}", strategy.id);
    }
    validate_strategy_model(tokens, prices, strategy)?;
    if draft.slippage_bps > max_slippage_bps {
        bail!("slippage_bps must not exceed the configured maximum {max_slippage_bps}");
    }
    let side = draft.side.parse()?;
    let direction = trade_direction(strategy, side);
    let input = token(tokens, &strategy.chain, direction.token_in)?;
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
///
/// # Errors
///
/// Returns an error if the strategy no longer validates, if the level belongs to
/// a different one, or if the stored decimals drifted from the configuration.
pub fn validate_stored_level(
    tokens: &TokenResolver,
    max_slippage_bps: u16,
    prices: &dyn PriceSource,
    entry: &StrategyLevel,
) -> Result<()> {
    validate_strategy_model(tokens, prices, &entry.strategy)?;
    if entry.level.strategy_id != entry.strategy.id {
        bail!("level references a different strategy");
    }
    if entry.level.slippage_bps > max_slippage_bps {
        bail!("slippage exceeds configured maximum {max_slippage_bps}");
    }
    let direction = trade_direction(&entry.strategy, entry.level.side);
    let input = token(tokens, &entry.strategy.chain, direction.token_in)?;
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

fn token<'a>(tokens: &'a TokenResolver, chain: &str, name: &str) -> Result<&'a RegisteredToken> {
    tokens
        .token(chain, name)
        .with_context(|| format!("{chain} does not configure {name}"))
}
