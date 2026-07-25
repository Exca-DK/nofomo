use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tempo_agentic_domain::{QuoteTradeRequest, TradeVenue, format_units_string};
use tempo_agentic_price::PriceTick;
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderStore};
use tokio::sync::{Notify, mpsc};

use crate::fired::{cooling_down, fired_levels};
use crate::resolver::TokenResolver;

/// How long a level stays quiet after its pre-flight was rejected.
///
/// A pre-flight costs six network calls, some of them billed. Without this a
/// level that cannot execute — too small a balance, a pool under the liquidity
/// floor — would re-quote on every tick, which is dozens of times a minute.
const PREFLIGHT_RETRY_SECS: i64 = 60;

pub struct TriggerDeps {
    pub levels: Arc<dyn LevelStore>,
    pub orders: Arc<dyn OrderStore>,
    pub venues: Vec<Arc<dyn TradeVenue>>,
    pub resolver: TokenResolver,
}

impl TriggerDeps {
    fn venue(&self, name: &str) -> Result<&Arc<dyn TradeVenue>> {
        self.venues
            .iter()
            .find(|venue| venue.name() == name)
            .with_context(|| format!("unsupported trade venue {name}"))
    }
}

/// Turns price ticks into stored orders.
///
/// Runs until the tick channel closes. The orders it writes sit untouched until
/// something drives them; `waker` is how that something is told to look.
pub async fn run(deps: TriggerDeps, mut ticks: mpsc::Receiver<PriceTick>, waker: Arc<Notify>) {
    let mut quiet_until: HashMap<String, i64> = HashMap::new();
    while let Some(tick) = ticks.recv().await {
        // One bad tick must never end the loop: the next one may be fine, and a
        // trigger that quietly stopped would look exactly like a flat market.
        if let Err(error) = handle_tick(&deps, &mut quiet_until, &waker, &tick).await {
            tracing::error!(%error, "failed to handle price tick");
        }
    }
}

async fn handle_tick(
    deps: &TriggerDeps,
    quiet_until: &mut HashMap<String, i64>,
    waker: &Notify,
    tick: &PriceTick,
) -> Result<()> {
    let levels = deps.levels.list_levels().await.context("list levels")?;
    let orders = deps.orders.list_orders().await.context("list orders")?;
    let now = now_secs();

    let mut created = false;
    for level in fired_levels(&levels, &orders, tick, &deps.resolver) {
        if quiet_until.get(&level.id).is_some_and(|until| now < *until) {
            continue;
        }
        // The two rests do different jobs and neither replaces the other. This one
        // reads the orders on disk, so it survives a restart; the map above holds
        // levels whose pre-flight was refused, where no order exists to read.
        if cooling_down(&level.id, &orders, now) {
            continue;
        }
        match place_order(deps, level, tick, now).await {
            Ok(()) => {
                quiet_until.remove(&level.id);
                created = true;
            }
            Err(error) => {
                quiet_until.insert(level.id.clone(), now + PREFLIGHT_RETRY_SECS);
                tracing::warn!(level = %level.id, %error, "pre-flight rejected; level quiet for a while");
            }
        }
    }

    if created {
        waker.notify_one();
    }
    Ok(())
}

// The quote doubles as the pre-flight: it checks the spend balance and the pool
// liquidity floor, and the plan it returns is what the order carries forward.
async fn place_order(deps: &TriggerDeps, level: &Level, tick: &PriceTick, now: i64) -> Result<()> {
    let venue = deps.venue(level.venue.as_str())?;
    let draft = venue.quote(&quote_request(level)?).await?;

    // Derived from the level and the tick rather than random, so re-handling the
    // same tick upserts the one order instead of adding a second.
    let id = format!("{}-{}", level.id, tick.published_at);
    let order = Order::new(id, level, draft.plan, now);
    deps.orders
        .upsert_order(&order)
        .await
        .context("record new order")?;
    tracing::info!(level = %level.id, order = %order.id, "order created");
    Ok(())
}

fn quote_request(level: &Level) -> Result<QuoteTradeRequest> {
    Ok(QuoteTradeRequest {
        venue: level.venue,
        token_in: level.token_in.clone(),
        token_out: level.token_out.clone(),
        amount: format_units_string(&level.amount.to_string(), level.amount_decimals)?,
        slippage_bps: level.slippage_bps,
        // Pinned to the level's own chain: left empty the venue would compare
        // every configured chain and execute wherever the quote looked best.
        chains: vec![level.chain.clone()],
    })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
