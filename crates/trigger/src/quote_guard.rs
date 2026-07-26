use anyhow::{Result, bail};
use tempo_agentic_domain::QuoteDraft;
use tempo_agentic_price::PriceTick;
use tempo_agentic_strategy::{Side, StrategyLevel, trade_direction};

use crate::resolver::TokenResolver;

/// Rejects pegged quotes that diverge from the triggering price.
/// Unpegged counter tokens skip the check.
pub fn check_quote(
    tokens: &TokenResolver,
    entry: &StrategyLevel,
    draft: &QuoteDraft,
    tick: &PriceTick,
    max_deviation_bps: u16,
) -> Result<()> {
    let direction = trade_direction(&entry.strategy, entry.level.side);
    let quote_leg = match entry.level.side {
        Side::Buy => direction.token_in,
        Side::Sell => direction.token_out,
    };
    let pegged = tokens
        .token(&entry.strategy.chain, quote_leg)
        .is_some_and(|token| token.usd_peg);
    if !pegged {
        tracing::warn!(
            level = %entry.level.id,
            quote_leg,
            "quote left unchecked: the counter token has no dollar peg configured"
        );
        return Ok(());
    }

    let amount_in = amount(&draft.amount_in, "amount_in")?;
    let amount_out = amount(&draft.expected_amount_out, "expected_amount_out")?;
    if tick.price_usd <= 0.0 {
        bail!("the observed price is not positive, so the quote cannot be checked");
    }

    // The pegged leg is already denominated in dollars.
    let (dollars_in, dollars_out) = match entry.level.side {
        Side::Buy => (amount_in, amount_out * tick.price_usd),
        Side::Sell => (amount_in * tick.price_usd, amount_out),
    };
    if dollars_in <= 0.0 {
        bail!("the quote spends nothing, so it cannot be checked");
    }

    let deviation_bps = ((dollars_in - dollars_out).abs() / dollars_in) * 10_000.0;
    if deviation_bps > f64::from(max_deviation_bps) {
        bail!(
            "quote is {:.0} bps away from the observed price, over the {max_deviation_bps} bps limit: \
             spending {dollars_in:.2} USD would return {dollars_out:.2} USD",
            deviation_bps
        );
    }
    Ok(())
}

fn amount(raw: &str, field: &str) -> Result<f64> {
    match raw.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => bail!("quote field {field} is not a number: {raw}"),
    }
}
