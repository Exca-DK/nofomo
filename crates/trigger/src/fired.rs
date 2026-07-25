use tempo_agentic_price::PriceTick;
use tempo_agentic_strategy::{Level, Order, OrderStatus, level_fires};

use crate::resolver::TokenResolver;

/// The levels this tick fires, in the order they were given.
///
/// Whether a level has already fired is read from its orders rather than stored
/// on the level, so the two can never drift apart.
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

/// Whether the level acted too recently to act again.
///
/// A failed order leaves the level armed, so without this a rule whose swap keeps
/// reverting would start a fresh order on every tick — several a minute, each one
/// costing a quote and often gas.
///
/// The status is deliberately not checked: any order that did not fail already
/// blocks the level through [`is_spent`], so only failed ones reach here.
pub fn cooling_down(level_id: &str, orders: &[Order], now: i64) -> bool {
    orders
        .iter()
        .filter(|order| order.level_id == level_id)
        .any(|order| now < order.created_at + LEVEL_COOLDOWN_SECS)
}

/// Whether a level has already been acted on.
///
/// Any order that did not fail counts: one in flight must not be raced, and one
/// that filled means the rule has done its job. Only a failed attempt leaves the
/// level free, because nothing was committed.
///
/// Without this a rule would fire on every tick for as long as its price still
/// qualifies — several times a minute on a live feed.
pub fn is_spent(level_id: &str, orders: &[Order]) -> bool {
    orders
        .iter()
        .filter(|order| order.level_id == level_id)
        .any(|order| order.status() != OrderStatus::Failed)
}

// A level the configuration cannot resolve is skipped rather than matched on
// looser terms: quoting it against the wrong token would spend real funds.
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
