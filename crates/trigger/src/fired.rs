use tempo_agentic_price::PriceTick;
use tempo_agentic_strategy::{Level, Order, OrderStatus, level_fires};

use crate::resolver::TokenResolver;

/// Returns unspent levels fired by this tick, preserving input order.
pub fn fired_levels<'a>(
    levels: &'a [Level],
    orders: &[Order],
    tick: &PriceTick,
    resolver: &TokenResolver,
) -> Vec<&'a Level> {
    levels
        .iter()
        .filter(|level| !is_spent(&level.id, orders))
        .filter(|level| prices_this_tick(level, tick, resolver))
        .filter(|level| level_fires(level, tick.price_usd))
        .collect()
}

/// How long a level rests after an attempt before it may start another.
const LEVEL_COOLDOWN_SECS: i64 = 60;

/// Applies a cooldown after failed orders.
pub fn cooling_down(level_id: &str, orders: &[Order], now: i64) -> bool {
    orders
        .iter()
        .filter(|order| order.level_id == level_id)
        .any(|order| now < order.created_at + LEVEL_COOLDOWN_SECS)
}

/// Treats any non-failed order as having spent its level.
pub fn is_spent(level_id: &str, orders: &[Order]) -> bool {
    orders
        .iter()
        .filter(|order| order.level_id == level_id)
        .any(|order| order.status() != OrderStatus::Failed)
}

// Skip unresolved levels to avoid pricing the wrong token.
fn prices_this_tick(level: &Level, tick: &PriceTick, resolver: &TokenResolver) -> bool {
    let Some(pair) = resolver.price_pair(level) else {
        tracing::warn!(
            level = %level.id,
            chain = %level.chain,
            "level names a chain or token the configuration does not; nothing can price it"
        );
        return false;
    };
    pair.chain_id == tick.pair.chain_id
        // Checksummed and lowercase spellings name the same token.
        && pair
            .token_address
            .eq_ignore_ascii_case(&tick.pair.token_address)
}
