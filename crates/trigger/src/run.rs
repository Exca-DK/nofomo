use std::sync::Arc;

use anyhow::{Context, Result};
use tempo_agentic_domain::{QuoteTradeRequest, TradeVenue, format_units_string};
use tempo_agentic_price::PriceTick;
use tempo_agentic_strategy::{LevelStore, Order, OrderStore, StrategyLevel, trade_direction};
use tokio::sync::{Notify, mpsc};

use crate::fired::{cooling_down, fired_levels};
use crate::resolver::TokenResolver;
use crate::runtime::{RuntimeStatus, now_secs};

/// Delay after a rejected pre-flight to limit network calls.
const PREFLIGHT_RETRY_SECS: i64 = 60;

pub struct TriggerDeps {
    pub levels: Arc<dyn LevelStore>,
    pub orders: Arc<dyn OrderStore>,
    pub venues: Vec<Arc<dyn TradeVenue>>,
    pub resolver: TokenResolver,
    pub runtime: Arc<RuntimeStatus>,
}

impl TriggerDeps {
    fn venue(&self, name: &str) -> Result<&Arc<dyn TradeVenue>> {
        self.venues
            .iter()
            .find(|venue| venue.name() == name)
            .with_context(|| format!("unsupported trade venue {name}"))
    }
}

/// Stores orders from ticks and wakes their executor until the channel closes.
pub async fn run(deps: TriggerDeps, mut ticks: mpsc::Receiver<PriceTick>, waker: Arc<Notify>) {
    while let Some(tick) = ticks.recv().await {
        // One bad tick must not stop the loop.
        if let Err(error) = handle_tick(&deps, &waker, &tick).await {
            tracing::error!(%error, "failed to handle price tick");
        }
    }
}

async fn handle_tick(deps: &TriggerDeps, waker: &Notify, tick: &PriceTick) -> Result<()> {
    let levels = deps.levels.list_levels().await.context("list levels")?;
    let orders = deps.orders.list_orders().await.context("list orders")?;
    let now = now_secs();

    let mut created = false;
    for entry in fired_levels(&levels, &orders, tick, &deps.resolver) {
        let level = &entry.level;
        if deps.runtime.is_quiet(&level.id, now) {
            continue;
        }
        // Persisted orders and rejected pre-flights use separate cooldowns.
        if cooling_down(&level.id, &orders, now) {
            continue;
        }
        match place_order(deps, entry, tick, now).await {
            Ok(()) => {
                deps.runtime.clear_quiet(&level.id);
                created = true;
            }
            Err(error) => {
                deps.runtime
                    .set_quiet_until(level.id.clone(), now + PREFLIGHT_RETRY_SECS);
                tracing::warn!(level = %level.id, %error, "pre-flight rejected; level quiet for a while");
            }
        }
    }

    if created {
        waker.notify_one();
    }
    Ok(())
}

// The quote checks funds and liquidity and supplies the execution plan.
async fn place_order(
    deps: &TriggerDeps,
    entry: &StrategyLevel,
    tick: &PriceTick,
    now: i64,
) -> Result<()> {
    let venue = deps.venue(entry.strategy.venue.as_str())?;
    let draft = venue.quote(&quote_request(entry)?).await?;

    // A deterministic ID makes tick replays idempotent.
    let id = format!("{}-{}", entry.level.id, tick.published_at);
    let order = Order::new(id, entry, draft.plan, now);
    deps.orders
        .upsert_order(&order)
        .await
        .context("record new order")?;
    tracing::info!(level = %entry.level.id, order = %order.id, "order created");
    Ok(())
}

fn quote_request(entry: &StrategyLevel) -> Result<QuoteTradeRequest> {
    let direction = trade_direction(&entry.strategy, entry.level.side);
    Ok(QuoteTradeRequest {
        venue: entry.strategy.venue,
        token_in: direction.token_in.to_owned(),
        token_out: direction.token_out.to_owned(),
        amount: format_units_string(&entry.level.amount.to_string(), entry.level.amount_decimals)?,
        slippage_bps: entry.level.slippage_bps,
        // Never let the venue choose a different chain.
        chains: vec![entry.strategy.chain.clone()],
    })
}
